//! One account: a task owning the connection, and the handle that talks to it.

use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio_xmpp::connect::DnsConfig;
use tokio_xmpp::xmlstream::Timeouts;
use tokio_xmpp::{Client, Event as ClientEvent, IqRequest, IqResponse, Stanza};
use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;

use crate::pep::{self, Access};
use crate::{AccountConfig, AccountEvent, Error, Event, PepItem, Server, disco};

enum Command {
    Send(Box<Stanza>, oneshot::Sender<Result<(), Error>>),
    Iq {
        to: Option<Jid>,
        request: IqRequest,
        reply: oneshot::Sender<Result<Option<Element>, Error>>,
    },
    Stop(oneshot::Sender<()>),
}

/// Handle to a running account. Clones talk to the same connection.
#[derive(Clone, Debug)]
pub struct Account {
    jid: BareJid,
    commands: mpsc::Sender<Command>,
    iq_timeout: Duration,
}

impl Account {
    /// Starts connecting in the background; [`Event::Online`] tells when it is up.
    /// Commands sent before that wait in the queue.
    pub(crate) fn spawn(config: AccountConfig, events: mpsc::UnboundedSender<AccountEvent>) -> Self {
        let (commands, queue) = mpsc::channel(64);
        let jid = config.jid.clone();
        tokio::spawn(run(connect(&config), jid.clone(), queue, events));
        Self {
            jid,
            commands,
            iq_timeout: config.iq_timeout,
        }
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
    pub async fn publish(
        &self,
        node: &str,
        id: Option<&str>,
        payload: Element,
        access: Access,
    ) -> Result<(), Error> {
        self.set(None, pep::publish_request(node, id, payload, access))
            .await
            .map(drop)
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
    match &config.server {
        Server::Auto => Client::new(jid, password),
        Server::StartTls { host, port } => {
            Client::new_starttls(jid, password, DnsConfig::no_srv(host, *port), Timeouts::default())
        }
        Server::DirectTls { host, port } => Client::new_direct_tls_with_config(
            jid,
            password,
            DnsConfig::no_srv(host, *port),
            Timeouts::default(),
        ),
        #[cfg(feature = "insecure-tcp")]
        Server::Plaintext { addr } => {
            Client::new_plaintext(jid, password, DnsConfig::addr(addr), Timeouts::default())
        }
    }
}

async fn run(
    mut client: Client,
    account: BareJid,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::UnboundedSender<AccountEvent>,
) {
    let emit = |event| {
        let _ = events.send(AccountEvent {
            account: account.clone(),
            event,
        });
    };
    loop {
        tokio::select! {
            event = client.next() => match event {
                Some(event) => {
                    if let Some(event) = handle(&mut client, event).await {
                        emit(event);
                    }
                }
                None => break,
            },
            command = commands.recv() => match command {
                Some(Command::Send(stanza, reply)) => {
                    let result = client.send_stanza(*stanza).await.map(drop).map_err(|e| Error::Send(e.to_string()));
                    let _ = reply.send(result);
                }
                Some(Command::Iq { to, request, reply }) => {
                    let token = client.send_iq(to, request).await;
                    // The answer arrives later; waiting for it here would stall the stream.
                    tokio::spawn(async move {
                        let _ = reply.send(match token.await {
                            Ok(IqResponse::Result(payload)) => Ok(payload),
                            Ok(IqResponse::Error(error)) => Err(Error::Stanza(Box::new(error))),
                            Err(failure) => Err(Error::Send(format!("{failure:?}"))),
                        });
                    });
                }
                Some(Command::Stop(done)) => {
                    let _ = client.send_end().await;
                    let _ = done.send(());
                    return;
                }
                None => {
                    let _ = client.send_end().await;
                    return;
                }
            },
        }
    }
}

/// Answers what the protocol requires and turns the rest into an [`Event`].
async fn handle(client: &mut Client, event: ClientEvent) -> Option<Event> {
    match event {
        ClientEvent::Online {
            bound_jid, resumed, ..
        } => {
            if !resumed {
                let _ = client.send_stanza(disco::presence().into()).await;
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
            if let Some(answer) = disco::answer(iq) {
                let _ = client.send_stanza(answer.into()).await;
            }
            None
        }
        ClientEvent::Stanza(Stanza::Message(message)) => Some(message_event(message)),
        ClientEvent::Stanza(Stanza::Presence(presence)) => Some(Event::Presence(Box::new(presence))),
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
