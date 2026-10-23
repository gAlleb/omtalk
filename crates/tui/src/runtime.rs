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
use omtalk_xmpp::services::{CommandAction, CommandStep};
use omtalk_xmpp::{Account, AccountConfig, AccountEvent, Accounts, BareJid, Event, Jid, Server, muc};
use omtalk_xmpp::{avatar, receipts};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use tokio::sync::mpsc;
use xmpp_parsers::message::{Lang, Message};

use crate::app::{Action, App, ChatKind, Connection, Delivery, DeviceRow, Line, MediaState};
use crate::clipboard;
use crate::config::{self, AccountEntry, Config};
use crate::services::{Listing, Request};
use crate::settings::Written;
use crate::view::{self, Pictures};

/// How many messages of each chat are loaded on start.
const HISTORY: usize = 300;
/// How many messages of a direct chat are read from the archive when this device has none.
const HISTORY_FROM_ARCHIVE: usize = 50;

pub struct Options {
    pub config: Config,
    /// Where the config is: the settings window writes changes there.
    config_path: PathBuf,
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
        Ok(Self {
            config,
            config_path,
            db,
        })
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
                    rpassword::prompt_password(trf!("password for {jid}: ", "пароль для {jid}: "))
                        .map_err(|e| e.to_string())?
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
    /// The avatar of `jid` is ready to draw ("avatar:HASH" among the pictures), or gone.
    Avatar {
        jid: BareJid,
        hash: Option<String>,
    },
    Notice {
        account: BareJid,
        chat: Option<BareJid>,
        text: String,
    },
    Connection {
        account: BareJid,
        state: Connection,
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
        /// Open it: the user entered it just now.
        open: bool,
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
        /// Leaving `nick` for this one (status 303).
        new_nick: Option<String>,
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
    /// Someone's typing state in a chat.
    Typing {
        account: BareJid,
        chat: BareJid,
        who: String,
        state: omtalk_xmpp::chatstates::ChatState,
    },
    /// A client of someone (or one's own) came, went or changed.
    Online {
        account: BareJid,
        online: omtalk_xmpp::presence::Online,
    },
    /// The resource this program got on the account; `fresh` for a new session (not
    /// one resumed), whose server tells all presences again.
    Bound {
        account: BareJid,
        resource: String,
        fresh: bool,
    },
    /// News for the account's server channel, not shown below.
    Log {
        account: BareJid,
        text: String,
    },
    /// The file of message `id` is up, at `link`.
    Uploaded {
        account: BareJid,
        id: String,
        link: String,
    },
    /// A file to offer for sending (a picture pasted from the clipboard).
    Offer(PathBuf),
    /// A room's name.
    RoomName {
        account: BareJid,
        room: BareJid,
        name: Option<String>,
    },
    /// A room's subject; `by` is who changed it, none when it is told on entering.
    Topic {
        account: BareJid,
        room: BareJid,
        by: Option<String>,
        topic: String,
    },
    /// Who belongs to a room.
    Members {
        account: BareJid,
        room: BareJid,
        members: Vec<BareJid>,
    },
    /// The keys window's devices, or why not.
    Keys(Result<Vec<crate::keys::KeyRow>, String>),
    /// A device's trust changed: the pane beside that chat reads them again.
    DevicesStale {
        account: BareJid,
        jid: BareJid,
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
    /// Fetch shared files as they come; else on a click.
    auto_download: bool,
    /// Avatars are fetched (the settings window switches it).
    avatars: Arc<std::sync::atomic::AtomicBool>,
    /// Avatars being fetched now, by hash or by whose: one fetch each.
    avatar_loads: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    store: Store,
    tx: mpsc::UnboundedSender<Msg>,
}

/// Writes a terminal event to the journal, if there is one, and hands it on.
fn log_event<T: std::fmt::Debug>(journal: &Option<std::fs::File>, event: T) -> T {
    if let Some(mut file) = journal.as_ref() {
        use std::io::Write;
        let _ = writeln!(file, "{} {event:?}", chrono::Utc::now().format("%H:%M:%S%.3f"));
    }
    event
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
        auto_download: options.config.ui.auto_download,
        avatars: Arc::new(options.config.ui.avatars.into()),
        avatar_loads: Arc::default(),
        store: store.clone(),
        tx,
    };
    let mut jids = Vec::new();
    for mut config in configs {
        let jid = config.jid.clone();
        let state = match store.load_omemo(&jid).map_err(|e| e.to_string())? {
            Some(state) => state,
            None => State::new(OwnKeys::generate(now() as u64).map_err(|e| e.to_string())?),
        };
        config.resource = omtalk_xmpp::device_resource(state.keys.device_id);
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
    app.send_typing = ui.typing;
    let mut notify = options.config.notify.ready()?;
    // The older switch: bell = false is "bell_on = none".
    if !ui.bell {
        notify.bell_on = crate::notify::When::NEVER;
    }
    app.notify = notify;
    app.config_ui = ui.clone();
    app.show_avatars = ui.avatars;
    app.config_notify = options.config.notify.clone();
    app.icons = match ui.icons {
        config::IconSet::Nerd => crate::theme::nerd(),
        config::IconSet::Text => crate::theme::text_icons(),
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
    // OMTALK_LOG=file: every event the terminal sends, as omtalk reads it, for finding
    // out what a terminal does (a dropped file, a key that does nothing).
    let journal = std::env::var_os("OMTALK_LOG").and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    // A closed terminal window (SIGHUP) or a plain kill (SIGTERM) leaves as cleanly as
    // quitting: else the server keeps showing this client online for minutes.
    use tokio::signal::unix::{SignalKind, signal};
    let mut hangup = signal(SignalKind::hangup()).map_err(|e| e.to_string())?;
    let mut terminate = signal(SignalKind::terminate()).map_err(|e| e.to_string())?;
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
        // While someone types here, or this side types, look again every two seconds:
        // "paused" goes out, and a silent typist's word goes away.
        let typing = matches!(
            app.typing_out,
            Some((_, omtalk_xmpp::chatstates::ChatState::Composing))
        ) || app.typing_in.values().any(|t| !t.is_empty());
        let actions = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)), if typing => app.typing_tick(now()),
            _ = tokio::time::sleep_until(status.1 + STATUS_FADE), if fades => {
                app.status.clear();
                Vec::new()
            }
            event = input.next() => match log_event(&journal, event) {
                Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => app.on_key(key),
                Some(Ok(TermEvent::Mouse(mouse))) => app.on_mouse(mouse),
                Some(Ok(TermEvent::FocusGained)) => app.on_focus(true),
                Some(Ok(TermEvent::FocusLost)) => app.on_focus(false),
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
                        // An avatar: round, by the terminal if it shows pictures, else by
                        // omtalk in block characters, which know no transparency.
                        Msg::Picture { url, image } if url.starts_with("avatar:") => {
                            let image = crate::avatar::square(&image);
                            if picker.protocol_type() == ratatui_image::picker::ProtocolType::Halfblocks {
                                app.avatar_blocks.insert(url, image);
                            } else {
                                let round = image::DynamicImage::ImageRgba8(crate::avatar::round(&image));
                                pictures.insert(url, picker.new_resize_protocol(round));
                            }
                        }
                        Msg::Picture { url, image } => drop(pictures.insert(url, picker.new_resize_protocol(image))),
                        msg => apply(&mut app, msg, &shared),
                    }
                    next = rx.try_recv().ok();
                }
                Vec::new()
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = hangup.recv() => break,
            _ = terminate.recv() => break,
        };
        for action in actions {
            if action == Action::Quit {
                break;
            }
            if let Action::Setting { section, key, value } = action {
                // What the runtime itself keeps is used from now on; the file is written aside.
                match (key, &value) {
                    ("auto_download", Written::Bool(on)) => shared.auto_download = *on,
                    ("viewer", Written::Text(text)) => shared.viewer = text.clone(),
                    ("avatars", Written::Bool(on)) => {
                        shared.avatars.store(*on, std::sync::atomic::Ordering::Relaxed)
                    }
                    _ => {}
                }
                let (path, tx) = (options.config_path.clone(), shared.tx.clone());
                tokio::task::spawn_blocking(move || {
                    if let Err(problem) = crate::settings::write(&path, section, key, &value) {
                        let _ = tx.send(Msg::Status(trf!(
                            "the setting was not written: {problem}",
                            "настройка не записана: {problem}"
                        )));
                    }
                });
                continue;
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
                    (None, true) => tr!("me", "я").to_owned(),
                    (None, false) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
                };
                let mut line =
                    Line::message(message.at, who, message.body, message.encrypted, message.outgoing);
                line.id = message.id;
                line.delivery = message.delivery.map(|d| match d {
                    omtalk_store::Delivery::Sent => Delivery::Sent,
                    omtalk_store::Delivery::Delivered => Delivery::Delivered,
                    omtalk_store::Delivery::Read => Delivery::Read,
                    omtalk_store::Delivery::Failed(why) => Delivery::Failed(why),
                });
                app.chats[index].lines.push(line);
            }
        }
    }
}

fn apply(app: &mut App, msg: Msg, shared: &Shared) {
    match msg {
        Msg::Incoming { account, message, at } => {
            // Before the chat shows it, while it is still "not looked at".
            let alert = app.alert_for(&account, &message);
            if alert.bell {
                use std::io::Write;
                let mut out = io::stdout();
                let _ = out.write_all(b"\x07").and_then(|_| out.flush());
            }
            if alert.sound {
                crate::notify::run(&app.notify.sound);
            }
            if alert.desktop
                && let Some(text) = &message.text
            {
                let (who, room) = app.notice_names(&account, &message);
                let (person, room_avatar) = app.notice_avatars(&account, &message);
                // An action reads as one: "* bob waves".
                let text = match crate::app::action_of(text) {
                    Some(action) => format!("* {who} {action}"),
                    None => text.clone(),
                };
                let chat = message.peer.to_string();
                // The icon is made aside (once per pair of avatars), then the notification goes.
                tokio::task::spawn_blocking(move || {
                    let avatars = config::cache_dir().join("avatars");
                    let icon = crate::avatar::notice_icon(
                        &avatars,
                        &avatars.join("notify"),
                        person.as_deref(),
                        room_avatar.as_deref(),
                    );
                    crate::notify::run_blocking(&crate::notify::desktop(
                        &chat,
                        &who,
                        room.as_deref(),
                        &text,
                        icon.as_deref(),
                    ));
                });
            }
            if let Some(action) = app.ask_writer_avatar(&account, &message) {
                tokio::spawn(perform(action, shared.clone()));
            }
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
        Msg::Delivery { account, id, state } => {
            // Kept, so the ✓✓ is there after a restart too.
            let kept = match &state {
                Delivery::Sent => Some(omtalk_store::Delivery::Sent),
                Delivery::Delivered => Some(omtalk_store::Delivery::Delivered),
                Delivery::Read => Some(omtalk_store::Delivery::Read),
                Delivery::Failed(why) => Some(omtalk_store::Delivery::Failed(why.clone())),
                Delivery::Uploading(_) | Delivery::Sending | Delivery::Queued => None,
            };
            if let Some(kept) = kept {
                let _ = shared.store.set_delivery(&account, &id, &kept);
            }
            app.delivery(&account, &id, state)
        }
        Msg::ReadElsewhere { account, chat } => app.read_elsewhere(&account, &chat),
        Msg::Avatar { jid, hash } => match hash {
            Some(hash) => drop(app.avatars.insert(jid, hash)),
            None => drop(app.avatars.remove(&jid)),
        },
        Msg::RoomName { account, room, name } => {
            if let Some(chat) = app
                .chats
                .iter_mut()
                .find(|c| c.account == account && c.jid == room)
            {
                chat.name = name;
            }
        }
        Msg::Topic {
            account,
            room,
            by,
            topic,
        } => app.topic(&account, &room, by, topic, now()),
        Msg::Members {
            account,
            room,
            members,
        } => {
            if let Some(chat) = app
                .chats
                .iter_mut()
                .find(|c| c.account == account && c.jid == room)
            {
                chat.members = members;
            }
        }
        Msg::Keys(rows) => {
            if let Some(keys) = app.keys.as_mut() {
                keys.loaded(rows);
            }
        }
        Msg::DevicesStale { account, jid } => {
            if app
                .current()
                .is_some_and(|c| c.account == account && c.jid == jid)
            {
                tokio::spawn(perform(Action::Devices { account, jid }, shared.clone()));
            }
        }
        Msg::Paste(text) => {
            for action in app.on_paste(&text) {
                tokio::spawn(perform(action, shared.clone()));
            }
        }
        Msg::Typing {
            account,
            chat,
            who,
            state,
        } => app.typing(&account, &chat, who, state, now()),
        Msg::Uploaded { account, id, link } => app.uploaded(&account, &id, &link),
        Msg::Offer(path) => {
            app.input = format!("/upload {}", path.display());
            app.cursor = app.input.chars().count();
            app.focus = crate::app::Focus::Input;
            app.status = tr!(
                "Enter sends the picture from the clipboard",
                "Enter — отправить картинку из буфера"
            )
            .into();
        }
        Msg::Online { account, online } => app.presence(&account, online, now()),
        Msg::Log { account, text } => app.server_log(&account, text, now()),
        Msg::Bound {
            account,
            resource,
            fresh,
        } => {
            // A new session hears every presence again; what was known is stale.
            if fresh {
                app.online.retain(|(a, _), _| *a != account);
            }
            app.own_resource.insert(account, resource);
        }
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
            let index = app.server_chat(&account);
            if app.connection.get(&account) != Some(&state) {
                app.notice(Some(index), state.words(), now());
            }
            app.connection.insert(account, state);
        }
        Msg::Devices { account, jid, rows } => {
            let index = app.chat(&account, &jid, ChatKind::Direct);
            app.chats[index].devices = Some(rows);
        }
        Msg::Joined {
            account,
            room,
            nick,
            open,
        } => {
            tokio::spawn(perform(
                Action::Members {
                    account: account.clone(),
                    room: room.clone(),
                },
                shared.clone(),
            ));
            let index = app.chat(&account, &room, ChatKind::Room { nick });
            if open || app.selected.is_none() {
                for action in app.select(index) {
                    tokio::spawn(perform(action, shared.clone()));
                }
                if open {
                    app.focus = crate::app::Focus::Input;
                }
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
            new_nick,
        } => {
            // A nick changed: the same person, a new name, said in the room.
            if let Some(new) = new_nick
                && let Some(index) = app
                    .chats
                    .iter()
                    .position(|c| c.account == account && c.jid == room)
            {
                app.rename_occupant(index, &nick, &new, now());
                return;
            }
            // Entering, the room tells who is there before it confirms the entry: the
            // chat is made by the first of them, or they would be lost.
            let index = match app
                .chats
                .iter()
                .position(|c| c.account == account && c.jid == room)
            {
                Some(index) => Some(index),
                None if present => Some(app.chat(&account, &room, ChatKind::Room { nick: String::new() })),
                None => None,
            };
            if let Some(chat) = index.map(|i| &mut app.chats[i]) {
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
                    fresh: !*resumed,
                });
                tell(Msg::Connection {
                    account: account.clone(),
                    state: Connection::Online,
                });
                tell(Msg::Log {
                    account: account.clone(),
                    text: if *resumed {
                        tr!("connection restored", "связь восстановлена").to_owned()
                    } else {
                        trf!("logged in: {jid}", "вход: {jid}")
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
            Event::Reachable => tell(Msg::Connection {
                account: account.clone(),
                state: Connection::Online,
            }),
            Event::Disconnected { reason } => tell(Msg::Connection {
                account: account.clone(),
                state: Connection::Reconnecting(reason.clone()),
            }),
            Event::AuthFailed { reason } => tell(Msg::Connection {
                account: account.clone(),
                state: Connection::Refused(reason.clone()),
            }),
            Event::SubscriptionRequest { from } => tell(Msg::Notice {
                account: account.clone(),
                chat: Some(from.clone()),
                text: trf!(
                    "{from} asks to see your status: /accept {from} or /refuse {from}",
                    "{from} хочет видеть ваш статус: /accept {from} или /refuse {from}"
                ),
            }),
            Event::Message(message) => {
                if let Some((room, by, topic)) = muc::subject_of(message) {
                    tell(Msg::Topic {
                        account: account.clone(),
                        room,
                        by,
                        topic,
                    });
                }
                if let Some((room, from)) = muc::invitation_of(message) {
                    let who = from
                        .map(|f| f.to_string())
                        .unwrap_or_else(|| tr!("someone", "кто-то").into());
                    tell(Msg::Notice {
                        account: account.clone(),
                        chat: None,
                        text: trf!(
                            "{who} invites you to the room {room}: /join {room}",
                            "{who} зовёт в комнату {room}: /join {room}"
                        ),
                    });
                }
                if let Some(room) = muc::config_changed(message) {
                    tokio::spawn(perform(
                        Action::RoomName {
                            account: account.clone(),
                            room,
                        },
                        shared.clone(),
                    ));
                }
                // Someone typing; one's own other clients' carbons say nothing here.
                if let Some((chat, nick, state)) = omtalk_xmpp::chatstates::state_of(message)
                    && chat != account
                {
                    let who = nick.unwrap_or_else(|| chat.node().map(|n| n.to_string()).unwrap_or_default());
                    tell(Msg::Typing {
                        account: account.clone(),
                        chat,
                        who,
                        state,
                    });
                }
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
                if let Some((id, condition, text)) = receipts::bounce(message) {
                    tell(Msg::Delivery {
                        account: account.clone(),
                        id,
                        state: Delivery::Failed(bounce_words(&condition, text.as_deref())),
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
            Event::Pep {
                from, node, items, ..
            } if node == avatar::METADATA => match avatar::metadata_of(items) {
                Some(Some(id)) => drop(tokio::spawn(load_avatar(
                    shared.clone(),
                    xmpp.clone(),
                    from.clone(),
                    AvatarFrom::Pep(id),
                ))),
                Some(None) => tell(Msg::Avatar {
                    jid: from.clone(),
                    hash: None,
                }),
                None => {}
            },
            Event::Presence(presence) => {
                // A vCard avatar's hash, on a person's presence (not an occupant's).
                if let Some(Some(hash)) = avatar::photo_of(presence)
                    && muc::occupant(presence).is_none()
                    && let Some(from) = presence.from.as_ref().map(|f| f.to_bare())
                    && from != account
                {
                    tokio::spawn(load_avatar(
                        shared.clone(),
                        xmpp.clone(),
                        from,
                        AvatarFrom::VCard(Some(hash)),
                    ));
                }
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
                        new_nick: occupant.new_nick,
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
    // The device wrote: when, for the keys window ("писало вчера 14:05").
    if let Some(device) = message.device
        && message.text.is_some()
    {
        let _ = shared.store.device_seen(account, &message.sender, device, at);
    }
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
                id: message.id.clone(),
                // One's own, come back from the room: it got there.
                delivery: (message.direction == Direction::Outgoing && message.nick.is_some())
                    .then_some(omtalk_store::Delivery::Sent),
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
                text: trf!(
                    "OMEMO: device {}, fingerprint {fingerprint}",
                    "OMEMO: устройство {}, отпечаток {fingerprint}",
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
                    n => trf!(", unreadable: {n}", ", не прочитать: {n}"),
                };
                tell(Msg::Log {
                    account: account.clone(),
                    text: trf!(
                        "missed messages: {got}{unreadable}",
                        "пропущенных сообщений: {got}{unreadable}"
                    ),
                });
            }
            if caught.position_lost {
                tell(Msg::Notice {
                    account: account.clone(),
                    chat: None,
                    text: tr!(
                        "the server has already deleted some of the missed messages",
                        "часть пропущенных сообщений сервер уже удалил"
                    )
                    .into(),
                });
            }
            for message in caught.messages {
                deliver(&shared, &account, message);
            }
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: trf!("archive: {e}", "архив: {e}"),
        }),
    }
    for (room, _) in shared.store.rooms(&account).unwrap_or_default() {
        join(&account, &xmpp, &omemo, &shared, room, None, false).await;
    }
}

async fn join(
    account: &BareJid,
    xmpp: &Account,
    omemo: &Omemo,
    shared: &Shared,
    room: BareJid,
    nick: Option<String>,
    // Entered by the user just now: the room opens. Rooms entered again on start do not
    // take the view.
    open: bool,
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
                text: trf!("joined {room} as {nick}", "вход в {room} как {nick}"),
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
                    tell(Msg::Status(trf!(
                        "the bookmark for {room} was not saved: {e}",
                        "закладка для {room} не сохранилась: {e}"
                    )));
                }
            }
            tell(Msg::Joined {
                account: account.clone(),
                room: room.clone(),
                nick,
                open,
            });
            match omemo.catch_up_room(&room).await {
                Ok(caught) => {
                    if caught.position_lost {
                        tell(Msg::Notice {
                            account: account.clone(),
                            chat: Some(room.clone()),
                            text: tr!(
                                "the server has already deleted some of the room's missed messages",
                                "часть пропущенных сообщений комнаты сервер уже удалил"
                            )
                            .into(),
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
                    text: trf!("the room's archive: {e}", "архив комнаты: {e}"),
                }),
            }
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: trf!(
                "could not join {room}: {}",
                "не удалось войти в {room}: {}",
                refusal(&e)
            ),
        }),
    }
}

/// Why a room did not let us in, in words.
fn refusal(error: &omtalk_xmpp::Error) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    match error {
        omtalk_xmpp::Error::Stanza(e) => match e.defined_condition {
            C::RegistrationRequired => tr!(
                "members only: an invitation is needed",
                "только для участников, нужно приглашение"
            )
            .into(),
            C::Conflict => tr!("this nick is taken", "этот ник занят").into(),
            C::Forbidden => tr!("you are banned there", "вас туда не пускают (бан)").into(),
            C::ServiceUnavailable => tr!("the room is full", "комната переполнена").into(),
            C::ItemNotFound => tr!("no such room", "такой комнаты нет").into(),
            C::NotAuthorized => tr!("a password is needed", "нужен пароль").into(),
            ref other => format!("{other:?}"),
        },
        other => other.to_string(),
    }
}

/// Puts the file up on the account's upload service, sealed if `encrypted`, and gives
/// the link to share. The progress goes to the file's line as it moves.
async fn upload(
    xmpp: &Account,
    shared: &Shared,
    account: &BareJid,
    path: &std::path::Path,
    encrypted: bool,
    id: &str,
) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let service = xmpp
        .upload_service()
        .await
        .map_err(|e| e.to_string())?
        .ok_or(tr!("the server takes no files", "сервер не принимает файлы"))?;
    let (body, secret) = if encrypted {
        let sealed = omtalk_media::encrypt(&data);
        (sealed.data, Some((sealed.iv, sealed.key)))
    } else {
        (data, None)
    };
    if let Some(max) = service.max_size
        && body.len() as u64 > max
    {
        return Err(trf!(
            "the file is larger than the server takes ({} MB)",
            "файл больше, чем принимает сервер ({} МБ)",
            max / 1024 / 1024
        ));
    }
    let content_type = content_type(&name);
    let slot = xmpp
        .upload_slot(&service.jid, &name, body.len() as u64, content_type)
        .await
        .map_err(|e| service_error(&e))?;
    let (tx, account, id) = (shared.tx.clone(), account.clone(), id.to_owned());
    let last = std::sync::atomic::AtomicU8::new(0);
    omtalk_media::upload(
        &slot.put,
        &slot.headers,
        body,
        content_type,
        move |sent, total| {
            let percent = (sent * 100 / total.max(1)) as u8;
            // Only when the number changes: a hundred redraws at most.
            if last.swap(percent, std::sync::atomic::Ordering::Relaxed) != percent {
                let _ = tx.send(Msg::Delivery {
                    account: account.clone(),
                    id: id.clone(),
                    state: Delivery::Uploading(percent),
                });
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(match secret {
        Some((iv, key)) => omtalk_media::aesgcm_link(&slot.get, &iv, &key),
        None => slot.get,
    })
}

/// The type to name a file by, from its extension, for the few that matter.
fn content_type(name: &str) -> Option<&'static str> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "mp4" => "video/mp4",
        "ogg" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        _ => return None,
    })
}

/// A room's owners, admins and members. Open rooms may not show their lists to
/// everyone: then none.
async fn read_members(xmpp: &Account, room: &BareJid) -> Vec<BareJid> {
    let mut members = Vec::new();
    for affiliation in [
        omtalk_xmpp::Affiliation::Owner,
        omtalk_xmpp::Affiliation::Admin,
        omtalk_xmpp::Affiliation::Member,
    ] {
        if let Ok(list) = xmpp.affiliated(room, affiliation).await {
            members.extend(list);
        }
    }
    members.sort();
    members.dedup();
    members
}

/// The rows of the keys window: `peer`'s devices first, then one's own.
async fn key_rows(
    omemo: &Omemo,
    store: &Store,
    account: &BareJid,
    peer: Option<&BareJid>,
) -> Result<Vec<crate::keys::KeyRow>, String> {
    let mut rows = Vec::new();
    for owner in peer.into_iter().chain([account]) {
        let devices = omemo.devices(owner).await.map_err(|e| e.to_string())?;
        let seen = store.devices_seen(account, owner).unwrap_or_default();
        rows.extend(devices.into_iter().map(|d| crate::keys::KeyRow {
            owner: owner.clone(),
            device: d.device,
            fingerprint: d.identity_key.map(|k| omtalk_omemo::fingerprint(&k)),
            trust: d.trust,
            this_device: d.this_device,
            seen: seen.get(&d.device).copied(),
        }));
    }
    Ok(rows)
}

/// Why a message of one's came back, in words; the server's own after them.
fn bounce_words(condition: &omtalk_xmpp::stanza_error::DefinedCondition, server: Option<&str>) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    let what = match condition {
        C::PolicyViolation => tr!(
            "the recipient's server does not take such messages (policy-violation)",
            "сервер получателя не принимает такие сообщения (policy-violation)"
        ),
        C::RemoteServerNotFound => tr!(
            "the recipient's server was not found",
            "сервер получателя не найден"
        ),
        C::RemoteServerTimeout => tr!(
            "the recipient's server does not answer",
            "сервер получателя не отвечает"
        ),
        C::ServiceUnavailable => tr!(
            "the recipient takes no messages",
            "получатель не принимает сообщения"
        ),
        C::ItemNotFound => tr!("no such address", "такого адреса нет"),
        C::Forbidden | C::NotAllowed | C::NotAuthorized => tr!("refused", "отказано"),
        C::NotAcceptable => tr!("the message was not taken", "сообщение не принято"),
        C::ResourceConstraint => tr!("the recipient is out of room", "у получателя не хватает места"),
        C::RecipientUnavailable => tr!("the recipient is unavailable", "получатель недоступен"),
        C::JidMalformed => tr!("a wrong address", "неверный адрес"),
        _ => tr!("an error", "ошибка"),
    };
    match server {
        Some(text) => format!("{what}: {text}"),
        None => what.to_owned(),
    }
}

/// Why a stanza did not go, in the interface's words where omtalk knows them.
fn xmpp_error(error: &omtalk_xmpp::Error) -> String {
    match error {
        omtalk_xmpp::Error::NotConfirmed => tr!(
            "the server did not confirm it; it may be lost",
            "сервер не подтвердил получение; возможно, потеряно"
        )
        .into(),
        omtalk_xmpp::Error::Send(why) if why.starts_with("lost with the connection") => tr!(
            "lost with the connection; send it again",
            "потеряно вместе со связью; отправьте снова"
        )
        .into(),
        other => other.to_string(),
    }
}

/// Old messages encrypted before this device existed, counted in words.
fn unreadable_words(count: usize) -> String {
    if !crate::lang::russian() {
        let what = if count == 1 {
            "old message was"
        } else {
            "old messages were"
        };
        let them = if count == 1 { "it" } else { "them" };
        return format!(
            "{count} more {what} encrypted before this device existed: {them} cannot be read here"
        );
    }
    let ru = |one, few, many| crate::lang::ru_plural(count, one, few, many);
    let what = ru("старое сообщение", "старых сообщения", "старых сообщений");
    let verb = ru("зашифровано", "зашифрованы", "зашифрованы");
    let them = ru("его", "их", "их");
    format!("ещё {count} {what} {verb} до появления этого устройства — здесь {them} не прочитать")
}

/// A failed OMEMO send, in words.
fn omemo_error(error: &omtalk_omemo::Error) -> String {
    match error {
        omtalk_omemo::Error::NoTrustedDevices(jid) => {
            trf!(
                "{jid} has no trusted devices: decide about them in the keys window (/keys)",
                "у {jid} нет доверенных устройств — решите о них в окне ключей (/keys)"
            )
        }
        omtalk_omemo::Error::NoDevices(jid) => {
            trf!("{jid} has no OMEMO devices", "у {jid} нет устройств OMEMO")
        }
        omtalk_omemo::Error::Xmpp(error) => xmpp_error(error),
        other => other.to_string(),
    }
}

/// A failed service query, in words.
fn service_error(error: &omtalk_xmpp::Error) -> String {
    use omtalk_xmpp::stanza_error::DefinedCondition as C;
    match error {
        omtalk_xmpp::Error::Stanza(e) => {
            let what = match e.defined_condition {
                C::Forbidden | C::NotAuthorized => tr!("not allowed", "нет прав").to_owned(),
                C::ItemNotFound => tr!("not found", "такого нет").to_owned(),
                C::ServiceUnavailable | C::FeatureNotImplemented => {
                    tr!("not supported", "не поддерживается").to_owned()
                }
                C::BadRequest => {
                    tr!("the server did not take the request", "сервер не принял запрос").to_owned()
                }
                C::RemoteServerNotFound | C::RemoteServerTimeout => {
                    tr!("the server cannot be reached", "сервер недоступен").to_owned()
                }
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
        // Written by the loop, which keeps what the runtime uses.
        Action::Setting { .. } => {}
        Action::Send {
            account,
            to,
            room,
            text,
            encrypted,
            id,
            reply,
        } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            fetch_if_media(&text, &shared);
            // A reply names what it answers, beside the body (outside the encryption).
            let extra = reply
                .map(|(to, length)| omtalk_xmpp::replies::elements(&to, length))
                .unwrap_or_default();
            let delivery = |state| {
                tell(Msg::Delivery {
                    account: account.clone(),
                    id: id.clone(),
                    state,
                })
            };
            if encrypted {
                let sent = if room {
                    omemo.send_group_with(&to, &text, &id, extra).await
                } else {
                    omemo.send_with(&to, &text, &id, extra).await
                };
                match sent {
                    Ok(report) => {
                        save_own(
                            &shared,
                            &account,
                            &to,
                            &text,
                            true,
                            &id,
                            Some(omtalk_store::Delivery::Sent),
                        );
                        delivery(Delivery::Sent);
                        for (jid, device, e) in report.failed {
                            tell(Msg::Notice {
                                account: account.clone(),
                                chat: Some(to.clone()),
                                text: trf!(
                                    "device {device} of {jid} will not be able to read it: {e}",
                                    "устройство {device} у {jid} не сможет прочитать: {e}"
                                ),
                            });
                        }
                        for (jid, device) in report.untrusted {
                            tell(Msg::Notice {
                                account: account.clone(),
                                chat: Some(to.clone()),
                                text: trf!(
                                    "device {device} of {jid} is not verified and gets nothing (/keys)",
                                    "устройство {device} у {jid} не проверено, ему не шифруется (/keys)"
                                ),
                            });
                        }
                        for jid in report.without_omemo {
                            tell(Msg::Notice {
                                account: account.clone(),
                                chat: Some(to.clone()),
                                text: trf!("{jid} has no OMEMO", "у {jid} нет OMEMO"),
                            });
                        }
                    }
                    // Encrypted and queued: it goes when the connection is back.
                    Err(omtalk_omemo::Error::Xmpp(omtalk_xmpp::Error::Queued)) => {
                        save_own(&shared, &account, &to, &text, true, &id, None);
                        delivery(Delivery::Queued);
                    }
                    Err(e) => {
                        delivery(Delivery::Failed(omemo_error(&e)));
                    }
                }
            } else {
                let mut message = if room {
                    Message::groupchat(Some(to.clone().into()))
                } else {
                    Message::chat(Some(to.clone().into()))
                }
                .with_body(Lang::default(), text.clone());
                message.payloads.extend(extra);
                receipts::mark_outgoing(&mut message, &id);
                match xmpp.send(message).await {
                    Ok(()) => {
                        // A room's copy back goes into the history when it comes.
                        if !room {
                            save_own(
                                &shared,
                                &account,
                                &to,
                                &text,
                                false,
                                &id,
                                Some(omtalk_store::Delivery::Sent),
                            );
                        }
                        delivery(Delivery::Sent);
                    }
                    Err(omtalk_xmpp::Error::Queued) => {
                        if !room {
                            save_own(&shared, &account, &to, &text, false, &id, None);
                        }
                        delivery(Delivery::Queued);
                    }
                    Err(e) => delivery(Delivery::Failed(xmpp_error(&e))),
                }
            }
        }
        Action::ChatState {
            account,
            to,
            room,
            state,
        } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let _ = xmpp.send(omtalk_xmpp::chatstates::notice(&to, room, state)).await;
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
                    tell(Msg::Status(trf!(
                        "the read mark for your other devices: {e}",
                        "отметка «прочитано» для других устройств: {e}"
                    )));
                }
            }
        }
        Action::Join { account, room, nick } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            join(&account, &xmpp, &omemo, &shared, room, nick, true).await;
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
                Ok(()) => tr!(
                    "added; asked to see their status",
                    "добавлен; запрошен его статус"
                )
                .to_owned(),
                Err(e) => trf!("not added: {e}", "не добавлен: {e}"),
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
                Ok(()) => tr!("sees your status", "видит ваш статус").to_owned(),
                Err(e) => trf!("did not work: {e}", "не получилось: {e}"),
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
                text: tr!("does not see your status", "не видит ваш статус").into(),
            });
        }
        Action::Upload {
            account,
            to,
            room,
            path,
            encrypted,
            id,
        } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            let failed = |reason: String| {
                tell(Msg::Delivery {
                    account: account.clone(),
                    id: id.clone(),
                    state: Delivery::Failed(reason),
                })
            };
            let link = match upload(&xmpp, &shared, &account, &path, encrypted, &id).await {
                Ok(link) => link,
                Err(reason) => return failed(reason),
            };
            tell(Msg::Uploaded {
                account: account.clone(),
                id: id.clone(),
                link: link.clone(),
            });
            // The file is here already: put it where the fetched one would go, so the
            // picture shows without coming down again.
            if let Some(media) = omtalk_media::Media::from_text(&link) {
                let cached = media.cache_path(&shared.cache);
                let _ = tokio::fs::create_dir_all(&shared.cache).await;
                let _ = tokio::fs::copy(&path, &cached).await;
            }
            fetch_if_media(&link, &shared);
            let sent = if encrypted {
                let report = if room {
                    omemo.send_group_with_id(&to, &link, &id).await
                } else {
                    omemo.send_with_id(&to, &link, &id).await
                };
                report.map(drop).map_err(|e| e.to_string())
            } else {
                let mut message = if room {
                    Message::groupchat(Some(to.clone().into()))
                } else {
                    Message::chat(Some(to.clone().into()))
                }
                .with_body(Lang::default(), link.clone());
                // Out-of-band data (XEP-0066): what Gajim and others show as the file.
                message.payloads.push(
                    xmpp_parsers::minidom::Element::builder("x", "jabber:x:oob")
                        .append(
                            xmpp_parsers::minidom::Element::builder("url", "jabber:x:oob")
                                .append(link.as_str())
                                .build(),
                        )
                        .build(),
                );
                receipts::mark_outgoing(&mut message, &id);
                xmpp.send(message).await.map_err(|e| e.to_string())
            };
            match sent {
                Ok(()) => {
                    // A room's copy back goes into the history when it comes.
                    if !(room && !encrypted) {
                        save_own(
                            &shared,
                            &account,
                            &to,
                            &link,
                            encrypted,
                            &id,
                            Some(omtalk_store::Delivery::Sent),
                        );
                    }
                    tell(Msg::Delivery {
                        account: account.clone(),
                        id: id.clone(),
                        state: Delivery::Sent,
                    });
                }
                Err(e) => failed(e),
            }
        }
        Action::CloseChat { account, jid } => {
            if let Err(e) = shared.store.close_chat(&account, &jid) {
                tell(Msg::Status(trf!(
                    "the chat {jid} did not close for good: {e}",
                    "чат {jid} не закрылся насовсем: {e}"
                )));
            }
        }
        Action::SaveFolders(folders) => {
            if let Err(e) = shared.store.save_folders(&folders) {
                tell(Msg::Status(trf!(
                    "the folders were not saved: {e}",
                    "папки не сохранились: {e}"
                )));
            }
        }
        Action::Devices { account, jid } => {
            let Some((_, omemo)) = get(&account) else { return };
            match omemo.devices(&jid).await {
                Ok(devices) => {
                    let seen = shared.store.devices_seen(&account, &jid).unwrap_or_default();
                    let rows = devices
                        .into_iter()
                        .map(|d| DeviceRow {
                            seen: seen.get(&d.device).copied(),
                            device: d.device,
                            fingerprint: d.identity_key.map(|k| omtalk_omemo::fingerprint(&k)),
                            has_session: d.has_session,
                            this_device: d.this_device,
                            trust: d.trust,
                        })
                        .collect();
                    tell(Msg::Devices { account, jid, rows });
                }
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(jid),
                    text: trf!("devices: {e}", "устройства: {e}"),
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
        Action::RoomName { account, room } => {
            let Some((xmpp, _)) = get(&account) else { return };
            if let Ok(name) = xmpp.room_name(&room).await {
                tell(Msg::RoomName { account, room, name });
            }
        }
        Action::SetTopic { account, room, topic } => {
            let Some((xmpp, _)) = get(&account) else { return };
            if let Err(e) = xmpp.set_subject(&room, &topic).await {
                tell(Msg::Notice {
                    account,
                    chat: Some(room),
                    text: trf!(
                        "the topic did not change: {}",
                        "тема не сменилась: {}",
                        service_error(&e)
                    ),
                });
            }
        }
        Action::SetRoomName { account, room, name } => {
            let Some((xmpp, _)) = get(&account) else { return };
            match xmpp.set_room_name(&room, &name).await {
                Ok(()) => tell(Msg::RoomName {
                    account,
                    room,
                    name: Some(name),
                }),
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(room),
                    text: trf!(
                        "the title did not change: {}",
                        "название не сменилось: {}",
                        service_error(&e)
                    ),
                }),
            }
        }
        Action::CreateRoom {
            account,
            room,
            name,
            nick,
        } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            // A bare name: on the account's own room service.
            let room = if room.domain().as_str() == "rooms.invalid" {
                match xmpp.rooms_service().await {
                    Ok(Some(service)) => match format!(
                        "{}@{service}",
                        room.node().map(|n| n.as_str()).unwrap_or_default()
                    )
                    .parse()
                    {
                        Ok(room) => room,
                        Err(_) => return,
                    },
                    _ => {
                        tell(Msg::Notice {
                            account,
                            chat: None,
                            text: tr!(
                                "the server has no room service: give name@service",
                                "у сервера нет сервиса комнат: укажите имя@сервис"
                            )
                            .into(),
                        });
                        return;
                    }
                }
            } else {
                room
            };
            join(&account, &xmpp, &omemo, &shared, room.clone(), nick, true).await;
            // The room was just made and is locked until configured: private, as OMEMO
            // in a room needs, kept, and named.
            match xmpp.make_private_named(&room, name.as_deref()).await {
                Ok(()) => {
                    tell(Msg::Log {
                        account: account.clone(),
                        text: trf!("room {room} created", "создана комната {room}"),
                    });
                    tell(Msg::RoomName { account, room, name });
                }
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(room),
                    text: trf!(
                        "the room was not set up (it may have been someone else's): {}",
                        "комната не настроилась (может, она уже была чужой): {}",
                        service_error(&e)
                    ),
                }),
            }
        }
        Action::Invite { account, room, jid } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let made = xmpp
                .set_affiliation(&room, &jid, omtalk_xmpp::Affiliation::Member)
                .await;
            let result = match made {
                Ok(()) => xmpp.invite(&room, &jid).await,
                Err(e) => Err(e),
            };
            let text = match result {
                Ok(()) => trf!("{jid} invited to the room", "{jid} позван(а) в комнату"),
                Err(e) => trf!(
                    "{jid} could not be invited: {}",
                    "{jid} не позвать: {}",
                    service_error(&e)
                ),
            };
            tell(Msg::Notice {
                account: account.clone(),
                chat: Some(room.clone()),
                text,
            });
            let members = read_members(&xmpp, &room).await;
            tell(Msg::Members {
                account,
                room,
                members,
            });
        }
        Action::Nick { account, room, nick } => {
            let Some((xmpp, _)) = get(&account) else { return };
            // Entering again under another nick is how a nick changes (XEP-0045, 7.6).
            match xmpp.join(&room, &nick).await {
                Ok(()) => {
                    let _ = shared.store.remember_room(&account, &room, &nick);
                    // The account's other clients keep the room under the new nick too.
                    let bookmarks = xmpp.bookmarks().await.unwrap_or_default();
                    let mut mark = bookmarks.into_iter().find(|b| b.room == room).unwrap_or(
                        omtalk_xmpp::bookmarks::Bookmark {
                            room: room.clone(),
                            name: None,
                            nick: None,
                            autojoin: true,
                        },
                    );
                    mark.nick = Some(nick);
                    let _ = xmpp.bookmark(&mark).await;
                }
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(room),
                    text: trf!("the nick did not change: {}", "ник не сменился: {}", refusal(&e)),
                }),
            }
        }
        Action::History { account, peer } => {
            let Some((_, omemo)) = get(&account) else { return };
            match omemo.history_with(&peer, HISTORY_FROM_ARCHIVE).await {
                Ok(history) => {
                    for message in history.messages {
                        deliver(&shared, &account, message);
                    }
                    if history.unreadable > 0 {
                        tell(Msg::Notice {
                            account,
                            chat: Some(peer),
                            text: unreadable_words(history.unreadable),
                        });
                    }
                }
                Err(e) => tell(Msg::Notice {
                    account,
                    chat: Some(peer),
                    text: trf!("history from the server: {e}", "история с сервера: {e}"),
                }),
            }
        }
        Action::Avatar { account, jid, room } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let from = if room {
                AvatarFrom::VCard(None)
            } else {
                AvatarFrom::Ask
            };
            load_avatar(shared.clone(), xmpp, jid, from).await;
        }
        Action::Members { account, room } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let members = read_members(&xmpp, &room).await;
            tell(Msg::Members {
                account,
                room,
                members,
            });
        }
        Action::Keys(request) => {
            let account = match &request {
                crate::keys::Request::Load { account, .. }
                | crate::keys::Request::Trust { account, .. }
                | crate::keys::Request::Remove { account, .. } => account.clone(),
            };
            let Some((_, omemo)) = get(&account) else { return };
            let peer = match request {
                crate::keys::Request::Load { peer, .. } => peer,
                crate::keys::Request::Trust {
                    owner, device, trust, ..
                } => {
                    if let Err(e) = omemo.set_trust(&owner, device, trust).await {
                        tell(Msg::Keys(Err(e.to_string())));
                        return;
                    }
                    // The pane beside the chat shows it too.
                    tell(Msg::DevicesStale {
                        account: account.clone(),
                        jid: owner.clone(),
                    });
                    (owner != account).then_some(owner)
                }
                crate::keys::Request::Remove { device, .. } => {
                    if let Err(e) = omemo.remove_own_device(device).await {
                        tell(Msg::Keys(Err(e.to_string())));
                        return;
                    }
                    tell(Msg::Log {
                        account: account.clone(),
                        text: trf!(
                            "device {device} removed from the list",
                            "устройство {device} удалено из списка"
                        ),
                    });
                    None
                }
            };
            tell(Msg::Keys(
                key_rows(&omemo, &shared.store, &account, peer.as_ref()).await,
            ));
        }
        Action::Copy(text) => {
            // wl-copy is quick; OSC 52 writes to the terminal, which the drawing also does,
            // but one whole sequence between frames is harmless.
            let lines = text.lines().count();
            match clipboard::copy(&text) {
                Ok(how) => tell(Msg::Status(trf!(
                    "copied ({lines} lines, {how})",
                    "скопировано ({lines} стр., {how})"
                ))),
                Err(e) => tell(Msg::Status(trf!("not copied: {e}", "не скопировалось: {e}"))),
            }
        }
        Action::PastePrimary | Action::PasteClipboard => {
            let primary = action_is_primary(&action);
            let folder = shared.cache.join("pasted");
            let pasted = tokio::task::spawn_blocking(move || {
                // A picture in the clipboard is kept as a file and offered for sending.
                if !primary && clipboard::holds_image() {
                    return clipboard::image_to(&folder).map(Err);
                }
                if primary {
                    clipboard::primary()
                } else {
                    clipboard::clipboard()
                }
                .map(Ok)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
            match pasted {
                Ok(Ok(text)) => tell(Msg::Paste(text)),
                Ok(Err(picture)) => tell(Msg::Offer(picture)),
                Err(e) => tell(Msg::Status(e)),
            }
        }
        Action::Fetch(link) => {
            if let Some(media) = omtalk_media::Media::from_text(&link) {
                tell(Msg::Media {
                    url: media.url().to_owned(),
                    state: MediaState::Loading,
                });
            }
            fetch_media(link, shared.clone()).await;
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

/// Keeps a message sent from here, as on the server: its ✓ is kept with it.
fn save_own(
    shared: &Shared,
    account: &BareJid,
    chat: &BareJid,
    text: &str,
    encrypted: bool,
    id: &str,
    delivery: Option<omtalk_store::Delivery>,
) {
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
            id: Some(id.to_owned()),
            delivery,
        },
    );
}

fn fetch_if_media(text: &str, shared: &Shared) {
    let Some(media) = omtalk_media::Media::from_text(text) else {
        return;
    };
    // Not by itself: what is here already shows, the rest waits for a click.
    if !shared.auto_download && !media.cache_path(&shared.cache).exists() {
        let _ = shared.tx.send(Msg::Media {
            url: media.url().to_owned(),
            state: MediaState::Offered,
        });
        return;
    }
    tokio::spawn(fetch_media(text.trim().to_owned(), shared.clone()));
}

/// Fetches (and decrypts) a shared file into the cache, and tells the interface.
/// Where an avatar is to be read from.
enum AvatarFrom {
    /// PEP, this one (from a notification).
    Pep(String),
    /// The vCard, whose hash, if known, may already be in the cache.
    VCard(Option<String>),
    /// Whatever there is: PEP, else the vCard.
    Ask,
}

/// Reads the avatar of `jid` (from the cache if it is there), decodes it and hands it to
/// the view; tells "none" when there is none.
async fn load_avatar(shared: Shared, xmpp: Account, jid: BareJid, from: AvatarFrom) {
    use std::sync::atomic::Ordering;
    if !shared.avatars.load(Ordering::Relaxed) {
        return;
    }
    let dir = config::cache_dir().join("avatars");
    let cached = |hash: &str| dir.join(hash).is_file();
    // One fetch at a time per picture (or, while it is not known, per person).
    let busy = match &from {
        AvatarFrom::Pep(id) | AvatarFrom::VCard(Some(id)) => id.clone(),
        _ => jid.to_string(),
    };
    if !shared.avatar_loads.lock().unwrap().insert(busy.clone()) {
        return;
    }
    let from = match from {
        AvatarFrom::Ask => match xmpp.avatar_id(&jid).await {
            Ok(Some(id)) => AvatarFrom::Pep(id),
            _ => AvatarFrom::VCard(None),
        },
        from => from,
    };
    let fetched: Result<Option<String>, String> = async {
        match from {
            AvatarFrom::Pep(id) | AvatarFrom::VCard(Some(id)) if cached(&id) => Ok(Some(id)),
            AvatarFrom::Pep(id) => {
                let bytes = xmpp.avatar(&jid, &id).await.map_err(|e| e.to_string())?;
                save_avatar(&dir, &bytes).map(Some)
            }
            _ => match xmpp.vcard_avatar(&jid).await.map_err(|e| e.to_string())? {
                Some(bytes) => save_avatar(&dir, &bytes).map(Some),
                None => Ok(None),
            },
        }
    }
    .await;
    shared.avatar_loads.lock().unwrap().remove(&busy);
    let hash = match fetched {
        Ok(Some(hash)) => hash,
        // None now: nothing is drawn. Not to be had just now: what was drawn stays.
        Ok(None) => {
            let _ = shared.tx.send(Msg::Avatar { jid, hash: None });
            return;
        }
        Err(_) => return,
    };
    let path = dir.join(&hash);
    let tx = shared.tx.clone();
    tokio::task::spawn_blocking(move || {
        let reader = image::ImageReader::open(&path).and_then(|r| r.with_guessed_format());
        if let Ok(Ok(image)) = reader.map(|r| r.decode()) {
            let _ = tx.send(Msg::Picture {
                url: format!("avatar:{hash}"),
                image,
            });
            let _ = tx.send(Msg::Avatar {
                jid,
                hash: Some(hash),
            });
        }
    });
}

/// Keeps an avatar under its hash; returns the hash.
fn save_avatar(dir: &std::path::Path, bytes: &[u8]) -> Result<String, String> {
    let hash = avatar::hash(bytes);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(&hash), bytes).map_err(|e| e.to_string())?;
    Ok(hash)
}

async fn fetch_media(link: String, shared: Shared) {
    let Some(media) = omtalk_media::Media::from_text(&link) else {
        return;
    };
    let (tx, url) = (shared.tx.clone(), media.url().to_owned());
    let last = std::sync::atomic::AtomicU8::new(0);
    let progress = move |got: u64, total: Option<u64>| {
        let Some(total) = total.filter(|t| *t > 0) else {
            return;
        };
        let percent = (got * 100 / total).min(99) as u8;
        // Only when the number changes: a hundred redraws at most.
        if last.swap(percent, std::sync::atomic::Ordering::Relaxed) != percent {
            let _ = tx.send(Msg::Media {
                url: url.clone(),
                state: MediaState::Downloading(percent),
            });
        }
    };
    let state = match media.fetch_with(&shared.cache, progress).await {
        Ok(path) => MediaState::Ready(path),
        Err(e) => MediaState::Failed(e.to_string()),
    };
    let _ = shared.tx.send(Msg::Media {
        url: media.url().to_owned(),
        state,
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn unreadable_messages_are_counted_in_proper_russian() {
        assert!(super::unreadable_words(1).starts_with("ещё 1 старое сообщение зашифровано"));
        assert!(super::unreadable_words(3).starts_with("ещё 3 старых сообщения зашифрованы"));
        assert!(super::unreadable_words(11).starts_with("ещё 11 старых сообщений"));
        assert!(super::unreadable_words(21).contains("здесь его не прочитать"));
    }
}
