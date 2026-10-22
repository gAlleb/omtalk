//! The running program: accounts, the event pump, the interface loop, and the actions.
//!
//! Three kinds of tasks, so the interface never waits on the network: the pump turns
//! account events into messages for the interface, in order; the interface loop draws
//! and turns keys and mouse into actions; each action runs on its own and reports back.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use futures::StreamExt;
use omemo::OwnKeys;
use omtalk_omemo::{Direction, Incoming, Omemo, State};
use omtalk_store::{Store, StoredMessage};
use omtalk_xmpp::receipts;
use omtalk_xmpp::services::{CommandAction, CommandStep};
use omtalk_xmpp::{Account, AccountConfig, AccountEvent, Accounts, BareJid, Event, Jid, Server, muc};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use tokio::sync::mpsc;
use xmpp_parsers::message::{Lang, Message};

use crate::app::{Action, App, ChatKind, Delivery, DeviceRow, Line, MediaState};
use crate::clipboard;
use crate::config::{self, AccountEntry, Config};
use crate::services::{Listing, Request};
use crate::view::{self, Pictures};

/// How many messages of each chat are loaded on start.
const HISTORY: usize = 300;

pub struct Options {
    config: Config,
    db: PathBuf,
}

impl Options {
    pub fn from_args() -> Result<Self, String> {
        let usage = || {
            format!(
                "usage: omtalk [JID] [--config PATH] [--db PATH]{}",
                if cfg!(feature = "insecure-tcp") {
                    " [--plaintext HOST:PORT]"
                } else {
                    ""
                }
            )
        };
        let mut args = std::env::args().skip(1);
        let mut config_path = config::config_dir().join("config.toml");
        let mut db = config::data_dir().join("omtalk.db");
        let mut single: Option<String> = None;
        #[cfg(feature = "insecure-tcp")]
        let mut plaintext: Option<String> = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => config_path = args.next().ok_or_else(usage)?.into(),
                "--db" => db = args.next().ok_or_else(usage)?.into(),
                #[cfg(feature = "insecure-tcp")]
                "--plaintext" => plaintext = Some(args.next().ok_or_else(usage)?),
                "-h" | "--help" => return Err(usage()),
                jid if !jid.starts_with('-') && single.is_none() => single = Some(jid.to_owned()),
                _ => return Err(usage()),
            }
        }
        let mut config = config::load(&config_path)?;
        if let Some(jid) = single {
            let entry = config
                .accounts
                .iter()
                .find(|a| a.jid == jid)
                .cloned()
                .unwrap_or(AccountEntry {
                    jid,
                    password_command: None,
                    host: None,
                    port: None,
                    direct_tls: None,
                    #[cfg(feature = "insecure-tcp")]
                    plaintext: None,
                });
            config.accounts = vec![entry];
        }
        #[cfg(feature = "insecure-tcp")]
        if let Some(addr) = plaintext {
            for account in &mut config.accounts {
                account.plaintext = Some(addr.clone());
            }
        }
        if config.accounts.is_empty() {
            return Err(format!(
                "no account: give one (omtalk me@example.org) or list them in {}",
                config_path.display()
            ));
        }
        Ok(Self { config, db })
    }
}

/// Account settings with passwords, read before the screen is taken over.
pub fn prepare_accounts(options: &Options) -> Result<Vec<AccountConfig>, String> {
    options
        .config
        .accounts
        .iter()
        .map(|entry| {
            let jid: BareJid = entry.jid.parse().map_err(|e| format!("{}: {e}", entry.jid))?;
            let password = match (&entry.password_command, std::env::var("OMTALK_PASSWORD")) {
                (Some(command), _) => config::run_password_command(command)?,
                (None, Ok(password)) => password,
                (None, Err(_)) => {
                    rpassword::prompt_password(format!("пароль для {jid}: ")).map_err(|e| e.to_string())?
                }
            };
            let mut account = AccountConfig::new(jid, password);
            if let Some(host) = &entry.host {
                account.server = Server::StartTls {
                    host: host.clone(),
                    port: entry.port.unwrap_or(5222),
                };
            }
            if let Some(addr) = &entry.direct_tls {
                let (host, port) = addr
                    .rsplit_once(':')
                    .ok_or_else(|| format!("direct_tls: {addr} is not HOST:PORT"))?;
                account.server = Server::DirectTls {
                    host: host.to_owned(),
                    port: port
                        .parse()
                        .map_err(|_| format!("direct_tls: bad port in {addr}"))?,
                };
            }
            #[cfg(feature = "insecure-tcp")]
            if let Some(addr) = &entry.plaintext {
                account.server = Server::Plaintext { addr: addr.clone() };
            }
            Ok(account)
        })
        .collect()
}

/// What tasks tell the interface.
enum Msg {
    Incoming {
        account: BareJid,
        message: Incoming,
        at: i64,
    },
    Notice {
        account: BareJid,
        chat: Option<BareJid>,
        text: String,
    },
    Connection {
        account: BareJid,
        state: String,
    },
    Devices {
        account: BareJid,
        jid: BareJid,
        rows: Vec<DeviceRow>,
    },
    Joined {
        account: BareJid,
        room: BareJid,
        nick: String,
    },
    Left {
        account: BareJid,
        room: BareJid,
    },
    Occupant {
        account: BareJid,
        room: BareJid,
        nick: String,
        real: Option<BareJid>,
        present: bool,
    },
    /// A shared file was fetched (or not).
    Media {
        url: String,
        state: MediaState,
    },
    /// A fetched picture, decoded and ready to be drawn.
    Picture {
        url: String,
        image: image::DynamicImage,
    },
    /// A line for the status bar, about no account in particular.
    Status(String),
    /// A client of someone (or one's own) came, went or changed.
    Online {
        account: BareJid,
        online: omtalk_xmpp::presence::Online,
    },
    /// The resource this program got on the account.
    Bound {
        account: BareJid,
        resource: String,
    },
    /// News for the account's server channel, not shown below.
    Log {
        account: BareJid,
        text: String,
    },
    /// Text from a clipboard, for the input.
    Paste(String),
    /// Another of one's clients read `chat`.
    ReadElsewhere {
        account: BareJid,
        chat: BareJid,
    },
    /// How far one's message `id` got.
    Delivery {
        account: BareJid,
        id: String,
        state: Delivery,
    },
    /// What an address offers, for the services window.
    Listed {
        jid: Jid,
        node: Option<String>,
        listing: Result<Listing, String>,
    },
    /// The answer to a command step.
    Stepped {
        jid: Jid,
        title: String,
        step: Result<CommandStep, String>,
    },
}

#[derive(Clone)]
struct Shared {
    accounts: HashMap<BareJid, (Account, Arc<Omemo>)>,
    /// Where fetched files are kept.
    cache: PathBuf,
    /// The program that shows a file outside the terminal.
    viewer: String,
    store: Store,
    tx: mpsc::UnboundedSender<Msg>,
}

/// How long a message stays in the status line.
const STATUS_FADE: std::time::Duration = std::time::Duration::from_secs(10);

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub async fn run(options: Options, configs: Vec<AccountConfig>) -> Result<(), String> {
    let store = Store::open(&options.db).map_err(|e| format!("{}: {e}", options.db.display()))?;
    let (mut accounts, events) = Accounts::new();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut shared = Shared {
        accounts: HashMap::new(),
        cache: config::cache_dir().join("media"),
        viewer: options.config.ui.viewer.clone(),
        store: store.clone(),
        tx,
    };
    let mut jids = Vec::new();
    for config in configs {
        let jid = config.jid.clone();
        let state = match store.load_omemo(&jid).map_err(|e| e.to_string())? {
            Some(state) => state,
            None => State::new(OwnKeys::generate(now() as u64).map_err(|e| e.to_string())?),
        };
        let account = accounts.add(config).await;
        let omemo = Arc::new(Omemo::persisting(account.clone(), state, store.omemo(&jid)));
        shared.accounts.insert(jid.clone(), (account, omemo));
        jids.push(jid);
    }

    let ui = &options.config.ui;
    let mut app = App::new(jids.clone(), ui.modifier, ui.left, ui.right);
    app.leader = config::parse_key(&ui.leader)?;
    app.read_markers = ui.read_markers;
    app.mouse = ui.mouse;
    app.icons = match ui.icons {
        config::IconSet::Nerd => crate::theme::NERD,
        config::IconSet::Text => crate::theme::TEXT_ICONS,
    };
    load_history(&mut app, &store, &jids);
    app.folders = store.folders().map_err(|e| e.to_string())?;
    // A chat put in a folder stays in the list, also before anyone wrote in it.
    for folder in app.folders.clone() {
        for chat in folder.chats {
            if jids.contains(&folder.account)
                && !store
                    .closed_chats(&folder.account)
                    .unwrap_or_default()
                    .contains(&chat)
                && !app
                    .chats
                    .iter()
                    .any(|c| c.account == folder.account && c.jid == chat)
            {
                app.chat(&folder.account, &chat, ChatKind::Direct);
            }
        }
    }
    // Shared files in the history: from the cache, or fetched again.
    let mut urls: Vec<String> = app
        .chats
        .iter()
        .flat_map(|c| c.lines.iter())
        .filter_map(|l| l.media.as_ref().map(|m| m.link.clone()))
        .collect();
    urls.dedup();
    for link in urls {
        tokio::spawn(fetch_media(link, shared.clone()));
    }
    tokio::spawn(pump(events, shared.clone()));
    if !app.chats.is_empty() {
        // Opening the first chat asks for what its panes show (the peer's devices).
        for action in app.select(0) {
            tokio::spawn(perform(action, shared.clone()));
        }
    }

    // Asks the terminal what graphics it speaks (kitty, sixel), before anything else reads
    // its answers; plain block characters where it speaks none, as Alacritty.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let mut pictures = Pictures::new();
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut input = EventStream::new();
    // A message in the status line fades after a while; a new one starts the clock again.
    let mut status = (String::new(), tokio::time::Instant::now());
    // The terminal starts with the mouse captured (main.rs).
    let mut mouse_captured = true;
    loop {
        terminal
            .draw(|frame| view::draw(frame, &mut app, &mut pictures))
            .map_err(|e| e.to_string())?;
        // Handing the mouse to the terminal and back, as asked.
        if app.mouse != mouse_captured {
            let toggled = if app.mouse {
                crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)
            } else {
                crossterm::execute!(io::stdout(), crossterm::event::DisableMouseCapture)
            };
            if toggled.is_ok() {
                mouse_captured = app.mouse;
            }
        }
        if app.status != status.0 {
            status = (app.status.clone(), tokio::time::Instant::now());
        }
        let fades = !app.status.is_empty();
        let actions = tokio::select! {
            _ = tokio::time::sleep_until(status.1 + STATUS_FADE), if fades => {
                app.status.clear();
                Vec::new()
            }
            event = input.next() => match event {
                Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(Ok(TermEvent::Mouse(mouse))) => app.on_mouse(mouse),
                // Pasted text arrives whole (bracketed paste), line breaks and all,
                // instead of as keys where each break would send.
                Some(Ok(TermEvent::Paste(text))) => app.on_paste(&text),
                Some(Ok(_)) => Vec::new(),
                Some(Err(_)) | None => break,
            },
            Some(msg) = rx.recv() => {
                // Drain what else is waiting, then draw once.
                let mut next = Some(msg);
                while let Some(msg) = next {
                    match msg {
                        Msg::Picture { url, image } => drop(pictures.insert(url, picker.new_resize_protocol(image))),
                        msg => apply(&mut app, msg, &shared),
                    }
                    next = rx.try_recv().ok();
                }
                Vec::new()
            }
            _ = tokio::signal::ctrl_c() => break,
        };
        for action in actions {
            if action == Action::Quit {
                break;
            }
            tokio::spawn(perform(action, shared.clone()));
        }
        if app.quit {
            break;
        }
    }
    // Leave cleanly, or rooms keep showing us online for minutes (XEP-0198 parking).
    for (account, _) in shared.accounts.values() {
        account.stop().await;
    }
    drop(accounts);
    Ok(())
}

fn load_history(app: &mut App, store: &Store, accounts: &[BareJid]) {
    for account in accounts {
        let closed = store.closed_chats(account).unwrap_or_default();
        let rooms: HashMap<BareJid, String> = store.rooms(account).unwrap_or_default().into_iter().collect();
        for (room, nick) in &rooms {
            app.chat(account, room, ChatKind::Room { nick: nick.clone() });
        }
        for summary in store
            .chats(account)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !closed.contains(&c.chat))
        {
            let kind = match rooms.get(&summary.chat) {
                Some(nick) => ChatKind::Room { nick: nick.clone() },
                None => ChatKind::Direct,
            };
            let index = app.chat(account, &summary.chat, kind);
            for message in store
                .recent_messages(account, &summary.chat, HISTORY)
                .unwrap_or_default()
            {
                let who = match (&message.nick, message.outgoing) {
                    (Some(nick), _) => nick.clone(),
                    (None, true) => "я".to_owned(),
                    (None, false) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
                };
                app.chats[index].lines.push(Line::message(
                    message.at,
                    who,
                    message.body,
                    message.encrypted,
                    message.outgoing,
                ));
            }
        }
    }
}

fn apply(app: &mut App, msg: Msg, shared: &Shared) {
    match msg {
        Msg::Incoming { account, message, at } => {
            for action in app.on_incoming(&account, &message, at) {
                tokio::spawn(perform(action, shared.clone()));
            }
            if let Some(text) = &message.text {
                fetch_if_media(text, shared);
            }
        }
        Msg::Media { url, state } => {
            // A picture is decoded aside: large ones take a moment.
            if let MediaState::Ready(path) = &state
                && (omtalk_media::Media::Plain { url: url.clone() }).is_image()
            {
                let (path, url, tx) = (path.clone(), url.clone(), shared.tx.clone());
                tokio::task::spawn_blocking(move || {
                    let reader = image::ImageReader::open(&path).and_then(|r| r.with_guessed_format());
                    if let Ok(Ok(image)) = reader.map(|r| r.decode()) {
                        let _ = tx.send(Msg::Picture { url, image });
                    }
                });
            }
            app.media_done(&url, state);
        }
        Msg::Picture { .. } => {}
        Msg::Status(text) => match app.active.clone().or_else(|| app.accounts.first().cloned()) {
            Some(account) => app.server_notice(&account, text, now()),
            None => app.status = text,
        },
        Msg::Delivery { account, id, state } => app.delivery(&account, &id, state),
        Msg::ReadElsewhere { account, chat } => app.read_elsewhere(&account, &chat),
        Msg::Paste(text) => drop(app.on_paste(&text)),
        Msg::Online { account, online } => app.presence(&account, online, now()),
        Msg::Log { account, text } => app.server_log(&account, text, now()),
        Msg::Bound { account, resource } => drop(app.own_resource.insert(account, resource)),
        Msg::Listed { jid, node, listing } => {
            if let Some(services) = app.services.as_mut() {
                services.listed(&jid, &node, listing);
            }
        }
        Msg::Stepped { jid, title, step } => {
            if let Some(services) = app.services.as_mut() {
                services.stepped(jid, title, step);
            }
        }
        // About a chat: there; about the account: in its server channel.
        Msg::Notice {
            account,
            chat: Some(chat),
            text,
        } => {
            let index = app.chat(&account, &chat, ChatKind::Direct);
            app.notice(Some(index), text, now());
        }
        Msg::Notice {
            account,
            chat: None,
            text,
        } => app.server_notice(&account, text, now()),
        Msg::Connection { account, state } => {
            // Offline, nobody is known to be online: the server tells again on return.
            if state != "в сети" {
                app.online.retain(|(a, _), _| *a != account);
            }
            let index = app.server_chat(&account);
            if app.connection.get(&account) != Some(&state) {
                app.notice(Some(index), state.clone(), now());
            }
            app.connection.insert(account, state);
        }
        Msg::Devices { account, jid, rows } => {
            let index = app.chat(&account, &jid, ChatKind::Direct);
            app.chats[index].devices = Some(rows);
        }
        Msg::Joined { account, room, nick } => {
            let index = app.chat(&account, &room, ChatKind::Room { nick });
            if app.selected.is_none() {
                app.select(index);
            }
        }
        Msg::Left { account, room } => {
            if let Some(index) = app
                .chats
                .iter()
                .position(|c| c.account == account && c.jid == room)
            {
                app.remove_chat(index);
            }
        }

        Msg::Occupant {
            account,
            room,
            nick,
            real,
            present,
        } => {
            if let Some(chat) = app
                .chats
                .iter_mut()
                .find(|c| c.account == account && c.jid == room)
            {
                if present {
                    chat.occupants.insert(nick, real);
                } else {
                    chat.occupants.remove(&nick);
                }
            }
        }
    }
}

/// Account events, in order: protocol upkeep, decryption, history.
async fn pump(mut events: mpsc::UnboundedReceiver<AccountEvent>, shared: Shared) {
    while let Some(AccountEvent { account, event }) = events.recv().await {
        let Some((xmpp, omemo)) = shared.accounts.get(&account).cloned() else {
            continue;
        };
        let tell = |msg| drop(shared.tx.send(msg));
        match &event {
            Event::Online { resumed, jid } => {
                tell(Msg::Bound {
                    account: account.clone(),
                    resource: jid.resource().to_string(),
                });
                tell(Msg::Connection {
                    account: account.clone(),
                    state: "в сети".into(),
                });
                tell(Msg::Log {
                    account: account.clone(),
                    text: if *resumed {
                        format!("сессия {jid} восстановлена (XEP-0198): пропущенное придёт само")
                    } else {
                        format!("вошли как {jid}")
                    },
                });
                if !resumed {
                    tokio::spawn(come_online(
                        account.clone(),
                        xmpp.clone(),
                        omemo.clone(),
                        shared.clone(),
                    ));
                }
            }
            Event::Disconnected { reason } => tell(Msg::Connection {
                account: account.clone(),
                state: format!("переподключение ({reason})"),
            }),
            Event::AuthFailed { reason } => tell(Msg::Connection {
                account: account.clone(),
                state: format!("пароль не принят: {reason}"),
            }),
            Event::SubscriptionRequest { from } => tell(Msg::Notice {
                account: account.clone(),
                chat: Some(from.clone()),
                text: format!("{from} хочет видеть ваш статус: /accept {from} или /refuse {from}"),
            }),
            Event::Message(message) => {
                for mark in receipts::marks(message) {
                    let (id, state) = match mark {
                        receipts::Mark::Received(id) => (id, Delivery::Delivered),
                        receipts::Mark::Displayed(id) => (id, Delivery::Read),
                    };
                    tell(Msg::Delivery {
                        account: account.clone(),
                        id,
                        state,
                    });
                }
                // One's message came back: the recipient's server (or one's own) refused it.
                if let Some((id, reason)) = receipts::bounce(message) {
                    tell(Msg::Delivery {
                        account: account.clone(),
                        id,
                        state: Delivery::Failed(reason),
                    });
                }
                if let Some(receipt) = receipts::answer(&account, message) {
                    let _ = xmpp.send(receipt).await;
                }
            }
            Event::Pep {
                from, node, items, ..
            } if node == receipts::MDS && *from == account => {
                for chat in receipts::read_elsewhere(items) {
                    tell(Msg::ReadElsewhere {
                        account: account.clone(),
                        chat,
                    });
                }
            }
            Event::Presence(presence) => {
                if let Some(online) = omtalk_xmpp::presence::online(presence) {
                    tell(Msg::Online {
                        account: account.clone(),
                        online,
                    });
                }
                if let Some(occupant) = muc::occupant(presence) {
                    tell(Msg::Occupant {
                        account: account.clone(),
                        room: occupant.room,
                        nick: occupant.nick,
                        real: occupant.real_jid,
                        present: occupant.present,
                    });
                }
            }
            _ => {}
        }
        match omemo.handle(&event).await {
            Ok(Some(message)) => deliver(&shared, &account, message),
            Ok(None) => {}
            Err(e) => tell(Msg::Notice {
                account: account.clone(),
                chat: None,
                text: e.to_string(),
            }),
        }
    }
}

/// Saves a message to the history and shows it.
fn deliver(shared: &Shared, account: &BareJid, message: Incoming) {
    let at = message.at.unwrap_or_else(now);
    if let Some(text) = &message.text {
        let _ = shared.store.save_message(
            account,
            &StoredMessage {
                chat: message.peer.clone(),
                sender: message.sender.clone(),
                nick: message.nick.clone(),
                outgoing: message.direction == Direction::Outgoing,
                encrypted: message.encrypted,
                body: text.clone(),
                at,
            },
        );
    }
    let _ = shared.tx.send(Msg::Incoming {
        account: account.clone(),
        message,
        at,
    });
}

/// After every new login: announce the device, read what was missed, enter the rooms.
async fn come_online(account: BareJid, xmpp: Account, omemo: Arc<Omemo>, shared: Shared) {
    let tell = |msg| drop(shared.tx.send(msg));
    match omemo.announce().await {
        Ok(()) => {
            let fingerprint = omtalk_omemo::fingerprint(&omemo.identity_key().await);
            tell(Msg::Log {
                account: account.clone(),
                text: format!(
                    "OMEMO: это устройство {}, отпечаток {fingerprint}",
                    omemo.device_id()
                ),
            });
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: format!("OMEMO: {e}"),
        }),
    }
    match omemo.catch_up().await {
        Ok(caught) => {
            let got = caught.messages.iter().filter(|m| m.text.is_some()).count();
            if got > 0 || caught.unreadable > 0 {
                let unreadable = match caught.unreadable {
                    0 => String::new(),
                    n => format!(", не для этого устройства: {n}"),
                };
                tell(Msg::Log {
                    account: account.clone(),
                    text: format!("из архива за время отсутствия: {got} сообщ.{unreadable}"),
                });
            }
            if caught.position_lost {
                tell(Msg::Notice {
                    account: account.clone(),
                    chat: None,
                    text: "часть сообщений за время отсутствия недоступна: сервер их уже не хранит".into(),
                });
            }
            for message in caught.messages {
                deliver(&shared, &account, message);
            }
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: format!("архив: {e}"),
        }),
    }
    for (room, _) in shared.store.rooms(&account).unwrap_or_default() {
        join(&account, &xmpp, &omemo, &shared, room, None).await;
    }
}

async fn join(
    account: &BareJid,
    xmpp: &Account,
    omemo: &Omemo,
    shared: &Shared,
    room: BareJid,
    nick: Option<String>,
) {
    let tell = |msg| drop(shared.tx.send(msg));
    // The nick: as asked, else the one the account's other clients use in this room (its
    // bookmark, so they all count as one occupant), else the last one used here, else
    // the account's name.
    let bookmarks = xmpp.bookmarks().await.unwrap_or_default();
    let bookmark = bookmarks.iter().find(|b| b.room == room);
    let nick = nick
        .or_else(|| bookmark.and_then(|b| b.nick.clone()))
        .or_else(|| {
            let rooms = shared.store.rooms(account).unwrap_or_default();
            rooms.into_iter().find(|(r, _)| *r == room).map(|(_, nick)| nick)
        })
        .unwrap_or_else(|| account.node().map(|n| n.to_string()).unwrap_or_default());
    match xmpp.join(&room, &nick).await {
        Ok(()) => {
            let _ = shared.store.remember_room(account, &room, &nick);
            tell(Msg::Log {
                account: account.clone(),
                text: format!("в комнате {room} как {nick}"),
            });
            // A room new to the account goes into its bookmarks, as Gajim does, so its
            // other clients know it and the nick.
            if bookmark.is_none() {
                let entry = omtalk_xmpp::bookmarks::Bookmark {
                    room: room.clone(),
                    name: None,
                    nick: Some(nick.clone()),
                    autojoin: true,
                };
                if let Err(e) = xmpp.bookmark(&entry).await {
                    tell(Msg::Status(format!("закладка для {room} не сохранилась: {e}")));
                }
            }
            tell(Msg::Joined {
                account: account.clone(),
                room: room.clone(),
                nick,
            });
            match omemo.catch_up_room(&room).await {
                Ok(caught) => {
                    if caught.position_lost {
                        tell(Msg::Notice {
                            account: account.clone(),
                            chat: Some(room.clone()),
                            text: "часть сообщений комнаты за время отсутствия недоступна: сервер их уже не хранит".into(),
                        });
                    }
                    if caught.unreadable > 0 {
                        tell(Msg::Notice {
                            account: account.clone(),
                            chat: Some(room.clone()),
                            text: format!(
                                "{} зашифр. сообщений из архива не прочитать: они написаны до появления \
                                 этого устройства или не для него",
                                caught.unreadable
                            ),
                        });
                    }
                    for message in caught.messages {
                        deliver(shared, account, message);
                    }
                }
                Err(e) => tell(Msg::Notice {
                    account: account.clone(),
                    chat: Some(room),
                    text: format!("архив комнаты: {e}"),
                }),
            }
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: format!("не удалось войти в {room}: {}", refusal(&e)),
        }),
    }
}

/// Why a room did not let us in, in words.
fn refusal(error: &omtalk_xmpp::Error) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    match error {
        omtalk_xmpp::Error::Stanza(e) => match e.defined_condition {
            C::RegistrationRequired => "только для участников, нужно приглашение".into(),
            C::Conflict => "этот ник занят".into(),
            C::Forbidden => "вас туда не пускают (бан)".into(),
            C::ServiceUnavailable => "комната переполнена".into(),
            C::ItemNotFound => "такой комнаты нет".into(),
            C::NotAuthorized => "нужен пароль".into(),
            ref other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}

/// A failed service query, in words.
fn service_error(error: &omtalk_xmpp::Error) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    match error {
        omtalk_xmpp::Error::Stanza(e) => {
            let what = match e.defined_condition {
                C::Forbidden | C::NotAuthorized => "нет прав".to_owned(),
                C::ItemNotFound => "такого нет".to_owned(),
                C::ServiceUnavailable | C::FeatureNotImplemented => "не поддерживается".to_owned(),
                C::BadRequest => "сервер не принял запрос".to_owned(),
                C::RemoteServerNotFound | C::RemoteServerTimeout => "сервер недоступен".to_owned(),
                ref other => format!("{other:?}"),
            };
            match e.texts.values().next() {
                Some(text) => format!("{what}: {text}"),
                None => what,
            }
        }
        other => other.to_string(),
    }
}

fn action_is_primary(action: &Action) -> bool {
    matches!(action, Action::PastePrimary)
}

async fn perform(action: Action, shared: Shared) {
    let tell = |msg| drop(shared.tx.send(msg));
    let get = |account: &BareJid| shared.accounts.get(account).cloned();
    match action {
        Action::Send {
            account,
            to,
            room,
            text,
            encrypted,
            id,
        } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            fetch_if_media(&text, &shared);
            let delivery = |state| {
                tell(Msg::Delivery {
                    account: account.clone(),
                    id: id.clone(),
                    state,
                })
            };
            if encrypted {
                let sent = if room {
                    omemo.send_group_with_id(&to, &text, &id).await
                } else {
                    omemo.send_with_id(&to, &text, &id).await
                };
                match sent {
                    Ok(report) => {
                        save_own(&shared, &account, &to, &text, true);
                        delivery(Delivery::Sent);
                        for (jid, device, e) in report.failed {
                            tell(Msg::Notice {
                                account: account.clone(),
                                chat: Some(to.clone()),
                                text: format!("устройство {device} у {jid} не сможет прочитать: {e}"),
                            });
                        }
                        for jid in report.without_omemo {
                            tell(Msg::Notice {
                                account: account.clone(),
                                chat: Some(to.clone()),
                                text: format!("у {jid} нет OMEMO"),
                            });
                        }
                    }
                    Err(e) => {
                        delivery(Delivery::Failed(e.to_string()));
                        tell(Msg::Notice {
                            account: account.clone(),
                            chat: Some(to),
                            text: format!("не отправлено: {e}. Без шифрования — выключите OMEMO (^O)"),
                        });
                    }
                }
            } else {
                let mut message = if room {
                    Message::groupchat(Some(to.clone().into()))
                } else {
                    Message::chat(Some(to.clone().into()))
                }
                .with_body(Lang::default(), text.clone());
                receipts::mark_outgoing(&mut message, &id);
                match xmpp.send(message).await {
                    Ok(()) => {
                        // A room's copy back goes into the history when it comes.
                        if !room {
                            save_own(&shared, &account, &to, &text, false);
                        }
                        delivery(Delivery::Sent);
                    }
                    Err(e) => delivery(Delivery::Failed(e.to_string())),
                }
            }
        }
        Action::Displayed {
            account,
            to,
            room,
            marker,
            archive_id,
        } => {
            let Some((xmpp, _)) = get(&account) else { return };
            if let Some(id) = marker {
                let _ = xmpp.send(receipts::displayed(to.clone().into(), &id)).await;
            }
            // In a room the room's archive names the message; in a chat, one's own.
            if let Some(id) = archive_id {
                let by = if room { to.clone() } else { account.clone() };
                if let Err(e) = xmpp.read_up_to(&to, &by, &id).await {
                    tell(Msg::Status(format!(
                        "отметка «прочитано» для других устройств: {e}"
                    )));
                }
            }
        }
        Action::Join { account, room, nick } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            join(&account, &xmpp, &omemo, &shared, room, nick).await;
        }
        Action::Leave { account, room, nick } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let _ = xmpp.leave(&room, &nick).await;
            let _ = shared.store.forget_room(&account, &room);
            tell(Msg::Left { account, room });
        }
        Action::AddContact { account, jid, name } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let text = match xmpp.add_contact(&jid, name.as_deref()).await {
                Ok(()) => "добавлен; запрошен его статус".to_owned(),
                Err(e) => format!("не добавлен: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: Some(jid),
                text,
            });
        }
        Action::Accept { account, jid } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let text = match xmpp.accept_contact(&jid).await {
                Ok(()) => "видит ваш статус".to_owned(),
                Err(e) => format!("не получилось: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: Some(jid),
                text,
            });
        }
        Action::Refuse { account, jid } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let _ = xmpp.refuse_contact(&jid).await;
            tell(Msg::Notice {
                account,
                chat: Some(jid),
                text: "не видит ваш статус".into(),
            });
        }
        Action::CloseChat { account, jid } => {
            if let Err(e) = shared.store.close_chat(&account, &jid) {
                tell(Msg::Status(format!("чат {jid} не закрылся насовсем: {e}")));
            }
        }
        Action::SaveFolders(folders) => {
            if let Err(e) = shared.store.save_folders(&folders) {
                tell(Msg::Status(format!("папки не сохранились: {e}")));
            }
        }
        Action::Devices { account, jid } => {
            let Some((_, omemo)) = get(&account) else { return };
            match omemo.devices(&jid).await {
                Ok(devices) => {
                    let rows = devices
                        .into_iter()
                        .map(|d| DeviceRow {
                            device: d.device,
                            fingerprint: d.identity_key.map(|k| omtalk_omemo::fingerprint(&k)),
                            has_session: d.has_session,
                            this_device: d.this_device,
                        })
                        .collect();
                    tell(Msg::Devices { account, jid, rows });
                }
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(jid),
                    text: format!("устройства: {e}"),
                }),
            }
        }
        Action::Services { request, title } => {
            let account = match &request {
                Request::Browse { account, .. } | Request::Execute { account, .. } => account.clone(),
            };
            let Some((xmpp, _)) = get(&account) else { return };
            match request {
                Request::Browse { jid, node, .. } => {
                    // Commands are listed under the address itself, not under its nodes.
                    let (info, items, commands) = tokio::join!(
                        xmpp.disco_info(&jid, node.as_deref()),
                        xmpp.disco_items(&jid, node.as_deref()),
                        async {
                            match node {
                                None => xmpp.commands(&jid).await.unwrap_or_default(),
                                Some(_) => Vec::new(),
                            }
                        },
                    );
                    // An address may answer one query and not the other; only both
                    // failing is a failure.
                    let listing = match (info, items) {
                        (Err(e), Err(_)) => Err(service_error(&e)),
                        (info, items) => {
                            let commands: Vec<_> = commands;
                            let items = items
                                .unwrap_or_default()
                                .into_iter()
                                // Commands also show up as items of their own node.
                                .filter(|i| !commands.contains(i))
                                .collect();
                            Ok(Listing {
                                info: info.ok(),
                                items,
                                commands,
                            })
                        }
                    };
                    tell(Msg::Listed { jid, node, listing });
                }
                Request::Execute {
                    jid,
                    node,
                    session,
                    action,
                    form,
                    ..
                } => {
                    let step = xmpp.execute(&jid, &node, session.as_deref(), action, form).await;
                    // A cancelled command's window is already gone.
                    if action != CommandAction::Cancel {
                        tell(Msg::Stepped {
                            jid,
                            title,
                            step: step.map_err(|e| service_error(&e)),
                        });
                    }
                }
            }
        }
        Action::Copy(text) => {
            // wl-copy is quick; OSC 52 writes to the terminal, which the drawing also does,
            // but one whole sequence between frames is harmless.
            let lines = text.lines().count();
            match clipboard::copy(&text) {
                Ok(how) => tell(Msg::Status(format!("скопировано ({lines} стр., {how})"))),
                Err(e) => tell(Msg::Status(format!("не скопировалось: {e}"))),
            }
        }
        Action::PastePrimary | Action::PasteClipboard => {
            let primary = action_is_primary(&action);
            let pasted = tokio::task::spawn_blocking(move || {
                if !primary && clipboard::holds_image() {
                    return Err(
                        "в буфере картинка: отправка картинок будет вместе с отправкой файлов".to_owned(),
                    );
                }
                if primary {
                    clipboard::primary()
                } else {
                    clipboard::clipboard()
                }
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            match pasted {
                Ok(text) => tell(Msg::Paste(text)),
                Err(e) => tell(Msg::Status(e)),
            }
        }
        Action::Open(path) => {
            // Detached and silent: the viewer must not write over the interface.
            let opened = std::process::Command::new(&shared.viewer)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            if let Err(e) = opened {
                tell(Msg::Notice {
                    account: BareJid::new("omtalk@localhost").unwrap(),
                    chat: None,
                    text: format!("{}: {e}", shared.viewer),
                });
            }
        }
        Action::Quit => {}
    }
}

fn save_own(shared: &Shared, account: &BareJid, chat: &BareJid, text: &str, encrypted: bool) {
    let _ = shared.store.save_message(
        account,
        &StoredMessage {
            chat: chat.clone(),
            sender: account.clone(),
            nick: None,
            outgoing: true,
            encrypted,
            body: text.to_owned(),
            at: now(),
        },
    );
}

fn fetch_if_media(text: &str, shared: &Shared) {
    if omtalk_media::Media::from_text(text).is_some() {
        tokio::spawn(fetch_media(text.trim().to_owned(), shared.clone()));
    }
}

/// Fetches (and decrypts) a shared file into the cache, and tells the interface.
async fn fetch_media(link: String, shared: Shared) {
    let Some(media) = omtalk_media::Media::from_text(&link) else {
        return;
    };
    let state = match media.fetch(&shared.cache).await {
        Ok(path) => MediaState::Ready(path),
        Err(e) => MediaState::Failed(e.to_string()),
    };
    let _ = shared.tx.send(Msg::Media {
        url: media.url().to_owned(),
        state,
    });
}
