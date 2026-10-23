//! One account: a task owning the connection, and the handle that talks to it.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_xmpp::connect::DnsConfig;
use tokio_xmpp::stanzastream::{StanzaStage, StanzaState};
use tokio_xmpp::{Client, Event as ClientEvent, IqRequest, IqResponse, Stanza};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;

use crate::login::{self, Login};
use crate::mam::{self, Archived, Page};
use crate::pep::{self, Access};
use crate::{AccountConfig, AccountEvent, Error, Event, PepItem, Server, disco};
use crate::{muc, roster};

enum Command {
    Send(Box<Stanza>, oneshot::Sender<Result<(), Error>>),
    Iq {
        to: Option<Jid>,
        request: IqRequest,
        reply: oneshot::Sender<Result<Option<Element>, Error>>,
    },
    /// Entering a room: done when the room sends our own presence back, or an error.
    Join {
        room: BareJid,
        presence: Box<xmpp_parsers::presence::Presence>,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    /// A message archive query: its results arrive as messages before the answer.
    Archive {
        to: Option<Jid>,
        archive: BareJid,
        queryid: String,
        query: Element,
        reply: oneshot::Sender<Result<Page, Error>>,
    },
    Stop(oneshot::Sender<()>),
}

/// An archive query whose results are still arriving.
struct Collecting {
    archive: BareJid,
    /// The account's own archive, not a room's.
    own: bool,
    messages: Vec<Archived>,
    reply: oneshot::Sender<Result<Page, Error>>,
}

/// Handle to a running account. Clones talk to the same connection.
#[derive(Clone, Debug)]
pub struct Account {
    jid: BareJid,
    commands: mpsc::Sender<Command>,
    iq_timeout: Duration,
    /// The rooms this account entered (or is entering) and has not left: only they may
    /// say who their occupants are and speak in groupchat. Anyone can send a presence
    /// or a groupchat message that looks like a room's.
    /// With the nick each is entered under: a status changed goes to them too.
    rooms: std::sync::Arc<std::sync::Mutex<HashMap<BareJid, String>>>,
    /// One's own status, said on every login and on entering a room.
    status: std::sync::Arc<std::sync::Mutex<crate::presence::OwnStatus>>,
}

impl Account {
    /// Starts connecting in the background; [`Event::Online`] tells when it is up.
    /// Commands sent before that wait in the queue.
    pub(crate) fn spawn(config: AccountConfig, events: mpsc::UnboundedSender<AccountEvent>) -> Self {
        let (commands, queue) = mpsc::channel(64);
        let jid = config.jid.clone();
        let account = jid.clone();
        let iq_timeout = config.iq_timeout;
        let status: std::sync::Arc<std::sync::Mutex<crate::presence::OwnStatus>> = Default::default();
        tokio::spawn(supervise(config, account, queue, events, status.clone()));
        Self {
            jid,
            commands,
            iq_timeout,
            rooms: Default::default(),
            status,
        }
    }

    /// Whether `room` is one this account entered and has not left.
    pub fn in_room(&self, room: &BareJid) -> bool {
        self.rooms.lock().unwrap().contains_key(room)
    }

    /// One's own status now.
    pub fn status(&self) -> crate::presence::OwnStatus {
        self.status.lock().unwrap().clone()
    }

    /// Says `status` to contacts (the server passes it on) and to every room entered;
    /// kept for the next logins and rooms.
    pub async fn set_status(&self, status: crate::presence::OwnStatus) -> Result<(), Error> {
        *self.status.lock().unwrap() = status.clone();
        let rooms: Vec<(BareJid, String)> = self
            .rooms
            .lock()
            .unwrap()
            .iter()
            .map(|(room, nick)| (room.clone(), nick.clone()))
            .collect();
        self.send(status.on(crate::disco::presence())).await?;
        for (room, nick) in rooms {
            if let Ok(to) = room.with_resource_str(&nick) {
                self.send(status.on(crate::disco::presence()).with_to(to)).await?;
            }
        }
        Ok(())
    }

    /// Counts `room` in under `nick`, or out (`None`).
    pub(crate) fn entered(&self, room: &BareJid, nick: Option<&str>) {
        let mut rooms = self.rooms.lock().unwrap();
        match nick {
            Some(nick) => drop(rooms.insert(room.clone(), nick.to_owned())),
            None => drop(rooms.remove(room)),
        }
    }

    pub(crate) fn nick_in(&self, room: &BareJid) -> Option<String> {
        self.rooms.lock().unwrap().get(room).cloned()
    }

    pub fn jid(&self) -> &BareJid {
        &self.jid
    }

    pub async fn send(&self, stanza: impl Into<Stanza>) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.command(Command::Send(Box::new(stanza.into()), reply))
            .await?;
        answer.await.map_err(|_| Error::Stopped)?
    }

    /// Sends an `<iq type="get">`; `to: None` addresses the own account.
    pub async fn get(&self, to: Option<Jid>, payload: impl Into<Element>) -> Result<Option<Element>, Error> {
        self.iq(to, IqRequest::Get(payload.into())).await
    }

    pub async fn set(&self, to: Option<Jid>, payload: impl Into<Element>) -> Result<Option<Element>, Error> {
        self.iq(to, IqRequest::Set(payload.into())).await
    }

    async fn iq(&self, to: Option<Jid>, request: IqRequest) -> Result<Option<Element>, Error> {
        let (reply, answer) = oneshot::channel();
        self.command(Command::Iq { to, request, reply }).await?;
        match tokio::time::timeout(self.iq_timeout, answer).await {
            Ok(answer) => answer.map_err(|_| Error::Stopped)?,
            Err(_) => Err(Error::Timeout),
        }
    }

    /// Publishes one item to a PEP node of the own account, creating the node if needed.
    /// A node another client created with other access is reconfigured first: OMEMO
    /// nodes have to be open, whoever made them.
    pub async fn publish(
        &self,
        node: &str,
        id: Option<&str>,
        payload: Element,
        access: Access,
    ) -> Result<(), Error> {
        let request = || pep::publish_request(node, id, payload.clone(), access);
        match self.set(None, request()).await {
            Err(Error::Stanza(error)) if pep::options_conflict(&error) => {
                self.set(None, pep::configure_request(node, access)).await?;
                self.set(None, request()).await.map(drop)
            }
            other => other.map(drop),
        }
    }

    /// Reads the items of a PEP node of `owner` (the own account included).
    pub async fn items(&self, owner: &BareJid, node: &str) -> Result<Vec<PepItem>, Error> {
        let to = (owner != &self.jid).then(|| owner.clone().into());
        pep::parse_items(self.get(to, pep::items_request(node)).await?)
    }

    /// Closes the stream and ends the task.
    pub async fn stop(&self) {
        let (reply, done) = oneshot::channel();
        if self.commands.send(Command::Stop(reply)).await.is_ok() {
            let _ = done.await;
        }
    }

    pub(crate) async fn join_room(
        &self,
        room: BareJid,
        presence: xmpp_parsers::presence::Presence,
    ) -> Result<(), Error> {
        let (reply, answer) = oneshot::channel();
        self.command(Command::Join {
            room,
            presence: Box::new(presence),
            reply,
        })
        .await?;
        match tokio::time::timeout(self.iq_timeout, answer).await {
            Ok(answer) => answer.map_err(|_| Error::Stopped)?,
            Err(_) => Err(Error::Timeout),
        }
    }

    pub(crate) async fn archive_query(
        &self,
        to: Option<Jid>,
        archive: BareJid,
        queryid: String,
        query: Element,
    ) -> Result<Page, Error> {
        let (reply, answer) = oneshot::channel();
        self.command(Command::Archive {
            to,
            archive,
            queryid,
            query,
            reply,
        })
        .await?;
        // A long archive takes several pages, each its own query; one page is quick.
        match tokio::time::timeout(self.iq_timeout, answer).await {
            Ok(answer) => answer.map_err(|_| Error::Stopped)?,
            Err(_) => Err(Error::Timeout),
        }
    }

    async fn command(&self, command: Command) -> Result<(), Error> {
        self.commands.send(command).await.map_err(|_| Error::Stopped)
    }
}

fn connect(config: &AccountConfig) -> Client {
    let jid = config
        .jid
        .with_resource_str(&config.resource)
        .expect("a resource string from the config is a valid resource");
    let password = config.password.clone();
    // Every way in gets the keepalive. tokio-xmpp's own constructor for the usual case
    // (SRV records) uses its defaults, five minutes of silence plus five for an answer:
    // a laptop whose network went away stayed "online" for up to ten, and missed what
    // came meanwhile. That was Client::new here once.
    match &config.server {
        Server::Auto => {
            let dns = DnsConfig::srv_default_client(config.jid.domain().as_str());
            Client::new_starttls(jid, password, dns, config.timeouts())
        }
        Server::StartTls { host, port } => {
            Client::new_starttls(jid, password, DnsConfig::no_srv(host, *port), config.timeouts())
        }
        Server::DirectTls { host, port } => Client::new_direct_tls_with_config(
            jid,
            password,
            DnsConfig::no_srv(host, *port),
            config.timeouts(),
        ),
        #[cfg(feature = "insecure-tcp")]
        Server::Plaintext { addr } => {
            Client::new_plaintext(jid, password, DnsConfig::addr(addr), config.timeouts())
        }
    }
}

/// Checks the password once, then runs the connection. tokio-xmpp re-establishes a
/// broken connection on its own, also after a server restart; it would also retry a
/// refused password forever without a word, which the check prevents.
async fn supervise(
    config: AccountConfig,
    account: BareJid,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<AccountEvent>,
    status: std::sync::Arc<std::sync::Mutex<crate::presence::OwnStatus>>,
) {
    let emit = |event| {
        let _ = events.send(AccountEvent {
            account: account.clone(),
            event,
        });
    };
    match login::check(&config).await {
        Login::Accepted => {}
        Login::Refused(reason) => return emit(Event::AuthFailed { reason }),
        // tokio-xmpp keeps trying from here, with growing pauses.
        Login::Unreachable(reason) => emit(Event::Disconnected { reason }),
    }
    let liveness = Liveness {
        quiet: config.keepalive,
        answer: config.keepalive_answer,
    };
    // tokio-xmpp's own task ends only when it fails inside; the account would stay
    // offline till omtalk restarts. A new connection instead, after a pause that grows.
    let mut pause = Duration::from_secs(2);
    while let Ended::Lost = run(connect(&config), &config, &mut commands, &emit, liveness, &status).await {
        emit(Event::Disconnected {
            reason: "the connection task ended; connecting again".into(),
        });
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_secs(60));
    }
}

/// How long a stanza may wait to go out on the stream before the sender is told it is
/// queued: no connection.
const SEND_WAIT: Duration = Duration::from_secs(3);
/// How long the server has to acknowledge a stanza sent (XEP-0198). Long: on a network
/// gone quiet the bytes leave into the socket all the same, and the word comes once the
/// stream is resumed; till then the message shows as going, which is true.
const ACK_WAIT: Duration = Duration::from_secs(600);

/// How long an answer is waited for aside, after its asker has given up (it waits
/// `iq_timeout`): then what was kept for it (results of an archive query, the place for
/// the answer) goes.
const ANSWER_WAIT: Duration = Duration::from_secs(120);

/// How often the password is tried again while the server does not answer.
const LOGIN_RECHECK: Duration = Duration::from_secs(60);

enum Ended {
    /// Asked to stop, or every handle is gone.
    Stopped,
    /// tokio-xmpp's own task ended, which it does only when it fails internally.
    Lost,
}

/// How the connection's life is checked. tokio-xmpp notices a dead connection and comes
/// back on its own, but says nothing of it: the "suspended" state of its stream never
/// reaches a Client's user. So omtalk asks the server itself when it has been quiet.
#[derive(Clone, Copy)]
struct Liveness {
    /// Quiet this long, the server is pinged (XEP-0199).
    quiet: Duration,
    /// Unanswered this long, the connection counts as lost until the server speaks again.
    answer: Duration,
}

async fn run(
    mut client: Client,
    config: &AccountConfig,
    commands: &mut mpsc::Receiver<Command>,
    emit: &impl Fn(Event),
    liveness: Liveness,
    status: &std::sync::Mutex<crate::presence::OwnStatus>,
) -> Ended {
    let own = &config.jid;
    // While the server does not answer, whether the password still works: tokio-xmpp
    // retries a refused login forever without a word, so a password changed elsewhere
    // (and the session closed by the server) would look like an endless "connecting".
    let (login_tx, mut login) = mpsc::unbounded_channel::<login::Login>();
    let mut login_asked: Option<tokio::time::Instant> = None;
    // When the server last said anything, and whether it answers now.
    let mut heard = tokio::time::Instant::now();
    let mut lost = false;
    // When the session last began. A ping sent while tokio-xmpp is still settling a new
    // or resumed stream upsets that stream (the server then answered nothing more), so
    // none goes out until it has stood a while.
    let mut settled = tokio::time::Instant::now();
    let mut pinging = false;
    let (alive_tx, mut alive) = mpsc::unbounded_channel::<bool>();
    let mut check = tokio::time::interval(
        liveness
            .quiet
            .min(Duration::from_secs(5))
            .max(Duration::from_millis(500)),
    );
    check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut queries: HashMap<String, Collecting> = HashMap::new();
    // Rooms being entered, until the room confirms or refuses.
    let mut joins: HashMap<BareJid, oneshot::Sender<Result<(), Error>>> = HashMap::new();
    let (finished_tx, mut finished) = mpsc::unbounded_channel::<(String, Result<Option<Element>, Error>)>();
    loop {
        // Biased: a query's results come before its answer on the stream, but tokio-xmpp
        // settles the answer at once, so pending stream events must be read first.
        tokio::select! {
            biased;
            event = client.next() => match event {
                Some(event) => {
                    heard = tokio::time::Instant::now();
                    if let ClientEvent::Online { .. } = &event {
                        lost = false;
                        settled = tokio::time::Instant::now();
                    } else if lost {
                        // The server speaks again on the same connection: online as before,
                        // nothing to redo, so not an Online.
                        lost = false;
                        emit(Event::Reachable);
                    }
                    if let ClientEvent::Stanza(Stanza::Message(message)) = &event
                        && collect(&mut queries, message)
                    {
                        continue;
                    }
                    if let ClientEvent::Stanza(Stanza::Presence(presence)) = &event
                        && let Some((room, outcome)) = muc::join_outcome(presence)
                        && let Some(reply) = joins.remove(&room)
                    {
                        let _ = reply.send(outcome);
                    }
                    let said = status.lock().unwrap().clone();
                    if let Some(event) = handle(&mut client, own, event, &said).await {
                        emit(event);
                    }
                }
                // The client's worker is gone; `send_end` would panic now.
                None => return Ended::Lost,
            },
            _ = check.tick() => {
                if !pinging && heard.elapsed() >= liveness.quiet && settled.elapsed() >= liveness.quiet * 2 {
                    pinging = true;
                    let server: Jid = own.domain().as_str().parse().expect("a domain is a JID");
                    let token = client.send_iq(Some(server), IqRequest::Get(xmpp_parsers::ping::Ping.into())).await;
                    let alive_tx = alive_tx.clone();
                    let wait = liveness.answer;
                    tokio::spawn(async move {
                        let answered = tokio::time::timeout(wait, token).await.is_ok_and(|a| a.is_ok());
                        let _ = alive_tx.send(answered);
                    });
                }
            }
            Some(answered) = alive.recv() => {
                pinging = false;
                if answered {
                    heard = tokio::time::Instant::now();
                } else if !lost {
                    lost = true;
                    emit(Event::Disconnected {
                        reason: "the server does not answer".into(),
                    });
                }
                // Lost: try the password once, and again each minute while it lasts.
                if lost && login_asked.is_none_or(|at| at.elapsed() >= LOGIN_RECHECK) {
                    login_asked = Some(tokio::time::Instant::now());
                    let (config, login_tx) = (config.clone(), login_tx.clone());
                    tokio::spawn(async move {
                        let _ = login_tx.send(login::check(&config).await);
                    });
                }
            }
            Some(outcome) = login.recv() => {
                if let login::Login::Refused(reason) = outcome {
                    emit(Event::AuthFailed { reason });
                    return Ended::Stopped;
                }
            }
            Some((queryid, answer)) = finished.recv() => {
                if let Some(query) = queries.remove(&queryid) {
                    let _ = query.reply.send(answer.and_then(|answer| mam::page(query.messages, answer)));
                }
            }
            command = commands.recv() => match command {
                Some(Command::Join { room, presence, reply }) => {
                    match client.send_stanza((*presence).into()).await {
                        Ok(_) => drop(joins.insert(room, reply)),
                        Err(e) => drop(reply.send(Err(Error::Send(e.to_string())))),
                    }
                }
                Some(Command::Archive { to, archive, queryid, query, reply }) => {
                    let own = to.is_none();
                    let token = client.send_iq(to, IqRequest::Set(query)).await;
                    queries.insert(queryid.clone(), Collecting { archive, own, messages: Vec::new(), reply });
                    let finished = finished_tx.clone();
                    tokio::spawn(async move {
                        let answer = match tokio::time::timeout(ANSWER_WAIT, token).await {
                            Ok(answer) => iq_answer(answer),
                            Err(_) => Err(Error::Timeout),
                        };
                        let _ = finished.send((queryid, answer));
                    });
                }
                Some(command) => {
                    if let Some(done) = execute(&mut client, command).await {
                        return stop(client, done).await;
                    }
                }
                // Every handle is gone.
                None => return stop(client, None).await,
            },
        }
    }
}

/// Takes the message into its archive query, if it is a result of one and comes from
/// the archive that query asked.
fn collect(queries: &mut HashMap<String, Collecting>, message: &Message) -> bool {
    let Some(query) = mam::queryid_of(message).and_then(|id| queries.get_mut(&id)) else {
        return false;
    };
    match mam::result_of(message, &query.archive, query.own) {
        Some(archived) => {
            query.messages.push(archived);
            true
        }
        None => false,
    }
}

fn iq_answer(answer: Result<IqResponse, tokio_xmpp::IqFailure>) -> Result<Option<Element>, Error> {
    match answer {
        Ok(IqResponse::Result(payload)) => Ok(payload),
        Ok(IqResponse::Error(error)) => Err(Error::Stanza(Box::new(error))),
        Err(failure) => Err(Error::Send(format!("{failure:?}"))),
    }
}

async fn stop(client: Client, done: Option<oneshot::Sender<()>>) -> Ended {
    let _ = client.send_end().await;
    if let Some(done) = done {
        let _ = done.send(());
    }
    Ended::Stopped
}

/// Carries out one command; `Some` if it asks to stop, carrying whom to tell.
async fn execute(client: &mut Client, command: Command) -> Option<Option<oneshot::Sender<()>>> {
    match command {
        Command::Send(stanza, reply) => {
            // tokio-xmpp waits until the stanza is out on the stream, which without a
            // connection is not until it is back: here only a moment, so this loop goes
            // on (pings, other commands, stopping). The stanza stays queued meanwhile.
            let managed = client
                .get_stream_features()
                .is_some_and(|f| f.stream_management.is_some());
            match tokio::time::timeout(SEND_WAIT, client.send_stanza(*stanza)).await {
                Err(_) => drop(reply.send(Err(Error::Queued))),
                Ok(Err(e)) => drop(reply.send(Err(Error::Send(e.to_string())))),
                // Without stream management, out on the stream is all there is to know.
                Ok(Ok(_)) if !managed => drop(reply.send(Ok(()))),
                // With it, sent is not yet received: the server acknowledges (XEP-0198),
                // or, a resumption refused, the stanza is dropped and said lost.
                Ok(Ok(mut token)) => drop(tokio::spawn(async move {
                    let acked = tokio::time::timeout(ACK_WAIT, token.wait_for(StanzaStage::Acked)).await;
                    let result = match acked {
                        Ok(Some(StanzaState::Acked { .. })) => Ok(()),
                        Ok(Some(StanzaState::Failed { error })) => {
                            Err(Error::Send(error.into_io_error().to_string()))
                        }
                        Ok(Some(StanzaState::Dropped) | None) => {
                            Err(Error::Send("lost with the connection; send it again".into()))
                        }
                        Ok(Some(_)) | Err(_) => Err(Error::NotConfirmed),
                    };
                    let _ = reply.send(result);
                })),
            }
        }
        Command::Iq { to, request, reply } => {
            let token = client.send_iq(to, request).await;
            // The answer arrives later; waiting for it here would stall the stream.
            tokio::spawn(async move {
                let answer = match tokio::time::timeout(ANSWER_WAIT, token).await {
                    Ok(answer) => iq_answer(answer),
                    Err(_) => Err(Error::Timeout),
                };
                let _ = reply.send(answer);
            });
        }
        Command::Archive { .. } | Command::Join { .. } => unreachable!("run() handles these itself"),
        Command::Stop(done) => return Some(Some(done)),
    }
    None
}

/// Answers what the protocol requires and turns the rest into an [`Event`].
async fn handle(
    client: &mut Client,
    own: &BareJid,
    event: ClientEvent,
    status: &crate::presence::OwnStatus,
) -> Option<Event> {
    match event {
        ClientEvent::Online {
            bound_jid, resumed, ..
        } => {
            if !resumed {
                // Asking for the roster first makes this an interested resource (RFC 6121,
                // 2.1.6): without it, the server sends no roster pushes here. The answer
                // itself is not needed; `Account::roster` reads the list when wanted.
                let query = xmpp_parsers::roster::Roster {
                    ver: None,
                    items: Vec::new(),
                };
                drop(client.send_iq(None, IqRequest::Get(query.into())).await);
                let _ = client.send_stanza(status.on(disco::presence()).into()).await;
            }
            Some(Event::Online {
                jid: bound_jid.try_into_full().ok()?,
                resumed,
            })
        }
        ClientEvent::Disconnected(error) => Some(Event::Disconnected {
            reason: error.to_string(),
        }),
        ClientEvent::Stanza(Stanza::Iq(iq)) => {
            if let Some(contacts) = roster::push(&iq, own) {
                // Acknowledged, as RFC 6121 requires; refusing it was a protocol error.
                let ack = Iq::empty_result(iq.from().cloned().unwrap_or_else(|| own.clone().into()), iq.id());
                let _ = client.send_stanza(ack.into()).await;
                return Some(Event::Roster(contacts));
            }
            if let Some(answer) = disco::answer(iq) {
                let _ = client.send_stanza(answer.into()).await;
            }
            None
        }
        ClientEvent::Stanza(Stanza::Message(message)) => Some(message_event(message)),
        ClientEvent::Stanza(Stanza::Presence(presence)) => match roster::subscription_request(&presence) {
            Some(from) => Some(Event::SubscriptionRequest { from }),
            None => Some(Event::Presence(Box::new(presence))),
        },
    }
}

fn message_event(message: Message) -> Event {
    let notification = message.from.as_ref().and_then(|from| {
        Some((
            from.to_bare(),
            message.payloads.iter().find_map(pep::parse_event)?,
        ))
    });
    match notification {
        Some((from, (node, items, retracted))) => Event::Pep {
            from,
            node,
            items,
            retracted,
        },
        None => Event::Message(Box::new(message)),
    }
}
