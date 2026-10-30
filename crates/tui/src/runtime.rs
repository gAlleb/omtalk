//! The running program: accounts, the event pump, the interface loop, and the actions.
//!
//! Three kinds of tasks, so the interface never waits on the network: the pump turns
//! account events into messages for the interface, in order; the interface loop draws
//! and turns keys and mouse into actions; each action runs on its own and reports back.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::terminal_backend::StableBackend;
use crossterm::event::{Event as TermEvent, EventStream, KeyEventKind};
use futures::StreamExt;
use omemo::OwnKeys;
use omtalk_omemo::{Direction, Incoming, Omemo, State};
use omtalk_store::{Store, StoredMessage};
use omtalk_xmpp::services::{CommandAction, CommandStep};
use omtalk_xmpp::{Account, AccountConfig, AccountEvent, Accounts, BareJid, Event, Jid, Server, muc};
use omtalk_xmpp::{avatar, receipts};
use ratatui::Terminal;
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

/// Holds this process's accounts in the database, as long as the returned files live.
/// An account's keys there are one OMEMO device: two omtalks with it would be that
/// device twice, sharing a resource the server gives to the last one in, each pushing
/// the other out. Other accounts on the same database run side by side, as before.
pub fn lock_accounts(options: &Options) -> Result<Vec<std::fs::File>, String> {
    let stem = options
        .db
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = options.db.parent().map(PathBuf::from).unwrap_or_default();
    let _ = std::fs::create_dir_all(&dir);
    options
        .config
        .accounts
        .iter()
        .map(|account| {
            let path = dir.join(format!("{stem}.{}.lock", account.jid));
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            match file.try_lock() {
                Ok(()) => Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => Err(trf!(
                    "{} is open in another omtalk already: a second client of the same account here needs a database of its own, as: omtalk --db ~/.local/share/omtalk/second.db",
                    "{} уже открыт в другом omtalk: второму клиенту того же аккаунта на этой машине нужна своя база, например: omtalk --db ~/.local/share/omtalk/second.db",
                    account.jid
                )),
                Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
            }
        })
        .collect()
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
    Saved {
        id: String,
        target: PathBuf,
        result: Result<(), String>,
    },
    Audio(crate::audio::Event),
    AudioInfo(PathBuf, Result<crate::audio::Info, String>),
    RoomStep {
        token: String,
        account: BareJid,
        room: BareJid,
        node: String,
        title: String,
        step: Result<CommandStep, String>,
    },
    NowPlaying {
        account: BareJid,
        chat: BareJid,
        text: Result<String, String>,
    },
    Incoming {
        account: BareJid,
        message: Incoming,
        at: i64,
    },
    /// A chat's history from the store, for a chat opened again: before what it has.
    Restored {
        account: BareJid,
        chat: BareJid,
        lines: Vec<Line>,
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
    /// Chats the account had, found in its archive by a new device: listed, not opened.
    Chats {
        account: BareJid,
        peers: Vec<BareJid>,
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
        /// Their status, as the room passes it on.
        status: omtalk_xmpp::presence::OwnStatus,
        /// Their standing there, as IRC marks it.
        mark: Option<&'static str>,
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
    Bot {
        account: BareJid,
        peer: BareJid,
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
    /// What `/search` found.
    Found {
        account: BareJid,
        chat: BareJid,
        words: String,
        found: Vec<crate::app::Found>,
    },
    /// A chat from a search result on (see [`Action::Jump`]).
    Jumped {
        account: BareJid,
        chat: BareJid,
        lines: Vec<Line>,
        at: i64,
    },
    /// A chat's messages before those shown (see [`Action::Older`]).
    Older {
        account: BareJid,
        chat: BareJid,
        lines: Vec<Line>,
        before: Option<String>,
        done: bool,
    },
    /// Someone's reactions to a message (XEP-0444).
    Reactions {
        account: BareJid,
        reactions: omtalk_xmpp::reactions::Reactions,
    },
    /// The name `jid` gave themselves (XEP-0172), or none any more.
    Nickname {
        account: BareJid,
        jid: BareJid,
        nick: Option<String>,
    },
    /// Names the account's contacts have in its roster (none: the address shows).
    Names {
        account: BareJid,
        names: Vec<(BareJid, Option<String>)>,
        /// The whole roster (as read at login): whoever is not in it has no name there.
        whole: bool,
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
    /// Server read positions (XEP-0490). Count only when applied to the UI:
    /// the user may read further while this event waits in the queue.
    ReadMarks {
        account: BareJid,
        marks: Vec<(BareJid, String)>,
    },
    /// How far one's message `id` got.
    Delivery {
        account: BareJid,
        id: String,
        state: Delivery,
    },
    /// How far one's message got, as `from` says over the network: a receipt, a read
    /// marker, a refusal. Taken only for a message in the chat with `from`.
    Marked {
        account: BareJid,
        from: BareJid,
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
    read_sync: HashMap<BareJid, crate::read_sync::ReadSync>,
    /// Where fetched files are kept.
    cache: PathBuf,
    /// The program that shows a file outside the terminal.
    viewer: String,
    /// Fetch shared files as they come; else on a click.
    auto_download: bool,
    image_links: bool,
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

/// Optional navigation-only trace: no message bodies, pasted text or typed letters.
fn trace_scroll(app: &App, event: &str) {
    use std::io::Write;
    let Some(path) = std::env::var_os("OMTALK_SCROLL_LOG") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let chat = app
        .current()
        .map(|c| (&c.account, &c.jid, c.scroll, c.lines.len(), c.older_loading));
    let _ = writeln!(
        file,
        "{} {event} selected={:?} chat={chat:?} focus={:?} messages={:?} left={:?}",
        chrono::Utc::now().format("%H:%M:%S%.3f"),
        app.selected,
        app.focus,
        app.areas.messages,
        app.areas.left
    );
}

/// Makes `dir` (and its parents) if need be, readable by the user alone (0700).
fn private_dir(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

/// How long a message stays in the status line.
const STATUS_FADE: std::time::Duration = std::time::Duration::from_secs(10);

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub async fn run(options: Options, configs: Vec<AccountConfig>) -> Result<(), String> {
    let store = Store::open(&options.db).map_err(|e| format!("{}: {e}", options.db.display()))?;
    // The cache holds files of encrypted chats, decrypted, and avatars: one's own only,
    // as the database is.
    private_dir(&config::cache_dir());
    let (mut accounts, events) = Accounts::new();
    let kept_status = store
        .kept(KEPT_STATUS)
        .ok()
        .flatten()
        .map(|kept| omtalk_xmpp::presence::OwnStatus::from_kept(&kept))
        .unwrap_or_default();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut shared = Shared {
        accounts: HashMap::new(),
        read_sync: HashMap::new(),
        cache: config::cache_dir().join("media"),
        viewer: options.config.ui.viewer.clone(),
        auto_download: options.config.ui.auto_download,
        image_links: options.config.ui.image_links,
        avatars: Arc::new(options.config.ui.avatars.into()),
        avatar_loads: Arc::default(),
        store: store.clone(),
        tx,
    };
    let audio_tx = shared.tx.clone();
    let audio = crate::audio::Engine::new(move |event| {
        let _ = audio_tx.send(Msg::Audio(event));
    });
    let mut jids = Vec::new();
    for mut config in configs {
        let jid = config.jid.clone();
        let state = match store.load_omemo(&jid).map_err(|e| e.to_string())? {
            Some(state) => state,
            None => State::new(OwnKeys::generate(now() as u64).map_err(|e| e.to_string())?),
        };
        config.resource = omtalk_xmpp::device_resource(state.keys.device_id);
        let account = accounts.add(config).await;
        // One's status as left last time, said by the login.
        account.keep_status(kept_status.clone());
        let omemo = Arc::new(Omemo::persisting(account.clone(), state, store.omemo(&jid)));
        let writer = account.clone();
        let errors = shared.tx.clone();
        let read_sync = crate::read_sync::ReadSync::spawn(
            move |mark| {
                let writer = writer.clone();
                async move { writer.read_up_to(&mark.chat, &mark.by, &mark.id).await }
            },
            move |mark, error, retry| {
                let next = if retry {
                    tr!("; will retry", "; повторим")
                } else {
                    ""
                };
                let _ = errors.send(Msg::Status(trf!(
                    "read state for {} was not synchronized: {error}{next}",
                    "прочитанное в {} не синхронизировано: {error}{next}",
                    mark.chat
                )));
            },
        );
        shared.read_sync.insert(jid.clone(), read_sync);
        shared.accounts.insert(jid.clone(), (account, omemo));
        jids.push(jid);
    }

    let ui = &options.config.ui;
    // Now and then, whether the rooms still have us.
    for (jid, (xmpp, omemo)) in &shared.accounts {
        let (account, xmpp, omemo, shared) = (jid.clone(), xmpp.clone(), omemo.clone(), shared.clone());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(ROOM_CHECK);
            every.tick().await;
            loop {
                every.tick().await;
                still_in_rooms(account.clone(), xmpp.clone(), omemo.clone(), shared.clone()).await;
            }
        });
    }
    let mut app = App::new(jids.clone(), ui.modifier, ui.left, ui.right);
    app.leader = config::parse_key(&ui.leader)?;
    ui.unread_binding()?;
    app.own_status = kept_status;
    // Contacts' names as last told, before the chats are made from the history.
    for account in &jids {
        let kept = |prefix: &str| {
            store
                .kept_under(&format!("{prefix}{account}/"))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(jid, name)| Some((jid.parse::<BareJid>().ok()?, name)))
                .collect::<Vec<_>>()
        };
        for (jid, name) in kept(KEPT_CONTACT_NAME) {
            app.names.insert((account.clone(), jid), name);
        }
        for (jid, name) in kept(KEPT_NICKNAME) {
            app.nicknames.insert((account.clone(), jid), name);
        }
    }
    if let Some(kept) = store.kept(KEPT_FOLDED).ok().flatten() {
        app.collapsed = folded_from_kept(&kept);
    }
    // OMEMO turned off (or on) by hand in a chat stays so.
    for (key, on) in store.kept_under(KEPT_OMEMO).unwrap_or_default() {
        if let Some((account, chat)) = key.split_once('/')
            && let (Ok(account), Ok(chat)) = (account.parse::<BareJid>(), chat.parse::<BareJid>())
        {
            app.omemo_choice.insert((account, chat), on == "1");
        }
    }
    restore_mutes(&store, &mut app);
    restore_chat_notify(&store, &mut app);
    restore_bots(&store, &mut app);
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
    app.compact = ui.compact;
    app.config_notify = options.config.notify.clone();
    app.icons = match ui.icons {
        config::IconSet::Nerd => crate::theme::nerd(),
        config::IconSet::Text => crate::theme::text_icons(),
    };
    load_history(&mut app, &store, &jids);
    for account in &jids {
        restore_local_unread(&mut app, &store, account);
    }
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
    for chat in &mut app.chats {
        prepare_media(&mut chat.lines, &shared);
    }
    tokio::spawn(pump(events, shared.clone()));
    // The chat open when omtalk was left, else the first account's server channel.
    // Opening it asks for what its panes show (the peer's devices).
    let last_open = store.kept(KEPT_OPEN).ok().flatten().and_then(|kept| {
        let (account, jid) = kept.split_once('\t')?;
        let (account, jid): (BareJid, BareJid) = (account.parse().ok()?, jid.parse().ok()?);
        app.chats
            .iter()
            .position(|c| c.account == account && c.jid == jid)
    });
    let start = last_open.or_else(|| jids.first().map(|account| app.server_chat(account)));
    if let Some(index) = start {
        for action in app.select(index) {
            tokio::spawn(perform(action, shared.clone()));
        }
    }

    // Asks the terminal what graphics it speaks (kitty, sixel), before anything else reads
    // its answers; plain block characters where it speaks none, as Alacritty.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let mut pictures = Pictures::new();
    let mut terminal = Terminal::new(StableBackend::new(io::stdout())).map_err(|e| e.to_string())?;
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
    // Folded folders and accounts, kept as they change.
    let mut folded = app.collapsed.clone();
    let mut opened = app.current().map(|c| format!("{}\t{}", c.account, c.jid));
    loop {
        flush_local_reads(&mut app, &shared.store);
        terminal
            .draw(|frame| view::draw(frame, &mut app, &mut pictures))
            .map_err(|e| e.to_string())?;
        // The chat open now, for the next start.
        let open = app.current().map(|c| format!("{}\t{}", c.account, c.jid));
        if open != opened {
            opened = open;
            let _ = shared.store.keep(KEPT_OPEN, opened.as_deref());
        }
        if app.collapsed != folded {
            folded = app.collapsed.clone();
            let _ = shared.store.keep(KEPT_FOLDED, Some(&folded_to_kept(&folded)));
        }
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
        let selected_before = app.selected;
        let actions = tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)), if typing => app.typing_tick(now()),
            _ = tokio::time::sleep_until(status.1 + STATUS_FADE), if fades => {
                app.status.clear();
                Vec::new()
            }
            event = input.next() => match log_event(&journal, event) {
                Some(Ok(TermEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                    if matches!(key.code, crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Down | crossterm::event::KeyCode::PageUp | crossterm::event::KeyCode::PageDown | crossterm::event::KeyCode::Home | crossterm::event::KeyCode::End) { trace_scroll(&app, &format!("navigation key {key:?}")); }
                    app.on_key(key)
                }
                Some(Ok(TermEvent::Mouse(mouse))) => {
                    trace_scroll(&app, &format!("mouse {mouse:?}"));
                    app.on_mouse(mouse)
                },
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
                let mut remaining = 64;
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
                    remaining -= 1;
                    if remaining == 0 {
                        break; // Give keyboard input and quit a turn even during a flood.
                    }
                    next = rx.try_recv().ok();
                }
                Vec::new()
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = hangup.recv() => break,
            _ = terminate.recv() => break,
        };
        if app.selected != selected_before {
            trace_scroll(&app, &format!("selection changed from {selected_before:?}"));
        }

        for action in actions {
            if let Action::Upload { path, .. } = &action {
                analyse_audio(&mut app, path, &shared);
            }
            if let Action::Audio(command) = &action {
                audio.send(command.clone());
                continue;
            }
            if action == Action::Quit {
                break;
            }
            if let Action::SetMuted { account, chat, muted } = &action {
                // Write in action order, before quit or another toggle can overtake it.
                if let Err(problem) = save_mute(&shared.store, account, chat, *muted) {
                    app.status = trf!(
                        "mute setting was not saved: {problem}",
                        "мьют не сохранился: {problem}"
                    );
                }
                continue;
            }
            if let Action::SetChatNotify { account, chat, prefs } = &action {
                if let Err(problem) = save_chat_notify(&shared.store, account, chat, prefs) {
                    app.status = trf!(
                        "chat notifications were not saved: {problem}",
                        "уведомления чата не сохранились: {problem}"
                    );
                }
                continue;
            }
            if let Action::Setting { section, key, value } = action {
                // What the runtime itself keeps is used from now on; the file is written aside.
                match (key, &value) {
                    ("auto_download", Written::Bool(on)) => shared.auto_download = *on,
                    ("image_links", Written::Bool(on)) => {
                        shared.image_links = *on;
                        for chat in &mut app.chats {
                            prepare_media(&mut chat.lines, &shared);
                        }
                    }
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
    audio.send(crate::audio::Command::Quit);
    // Leave cleanly, or rooms keep showing us online for minutes (XEP-0198 parking).
    futures::future::join_all(shared.accounts.values().map(|(account, _)| account.stop())).await;
    drop(accounts);
    Ok(())
}

fn load_history(app: &mut App, store: &Store, accounts: &[BareJid]) {
    for account in accounts {
        let closed = store.closed_chats(account).unwrap_or_default();
        let rooms: HashMap<BareJid, String> = store.rooms(account).unwrap_or_default().into_iter().collect();
        // Rooms' names as last told: shown at once, asked again on entering.
        let names: HashMap<String, String> = store
            .kept_under(&format!("{KEPT_ROOM_NAME}{account}/"))
            .unwrap_or_default()
            .into_iter()
            .collect();
        for (room, nick) in &rooms {
            let index = app.chat(account, room, ChatKind::Room { nick: nick.clone() });
            app.chats[index].name = names.get(room.as_str()).cloned();
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
            let messages = store
                .recent_messages(account, &summary.chat, HISTORY)
                .unwrap_or_default();
            let mut lines = stored_lines(messages);
            with_reactions(
                store,
                account,
                &summary.chat,
                rooms.contains_key(&summary.chat),
                &mut lines,
            );
            app.chats[index].lines.extend(lines);
        }
    }
}

/// The reactions kept for `chat`, onto its messages: by the room's id in a room, the
/// sender's in a chat.
fn with_reactions(store: &Store, account: &BareJid, chat: &BareJid, room: bool, lines: &mut [Line]) {
    for kept in store.reactions(account, chat).unwrap_or_default() {
        let line = lines.iter_mut().find(|l| {
            let id = if room { &l.archive_id } else { &l.id };
            id.as_deref() == Some(kept.message.as_str())
        });
        if let Some(line) = line {
            line.reactions.insert(kept.by, kept.reactions);
        }
    }
}

/// Messages from the store as the chat shows them.
fn stored_lines(messages: Vec<StoredMessage>) -> Vec<Line> {
    messages
        .into_iter()
        .map(|message| {
            let who = match (&message.nick, message.outgoing) {
                (Some(nick), _) => nick.clone(),
                (None, true) => tr!("me", "я").to_owned(),
                (None, false) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
            };
            let mut line = Line::message_sharing(
                message.at,
                who,
                message.body,
                message.encrypted,
                message.outgoing,
                message.shared_file,
            );
            line.id = message.id;
            line.reply_to = message.reply_to;
            line.archive_id = message.archive_id;
            line.untrusted = message.untrusted;
            line.edited = message.edited;
            line.delivery = message.delivery.map(|d| match d {
                omtalk_store::Delivery::Sent => Delivery::Sent,
                omtalk_store::Delivery::Delivered => Delivery::Delivered,
                omtalk_store::Delivery::Read => Delivery::Read,
                omtalk_store::Delivery::Failed(why) => Delivery::Failed(why),
            });
            line
        })
        .collect()
}

fn apply(app: &mut App, msg: Msg, shared: &Shared) {
    match msg {
        Msg::Saved { id, target, result } => match result {
            Ok(()) => {
                app.status = trf!("saved: {}", "сохранено: {}", target.display());
                if app.file_save.as_ref().is_some_and(|s| s.id == id) {
                    app.file_save = None;
                }
            }
            Err(error) => {
                app.status = trf!("not saved: {error}", "не сохранено: {error}");
                if let Some(s) = app.file_save.as_mut().filter(|s| s.id == id) {
                    s.busy = false;
                    s.error = Some(error);
                }
            }
        },

        Msg::AudioInfo(path, result) => {
            app.audio_pending.remove(&path);
            app.audio_info.insert(path, result);
        }
        Msg::Audio(crate::audio::Event::Playback(state)) => app.playback = state,
        Msg::Audio(crate::audio::Event::Error(error)) => {
            app.playback.playing = false;
            app.playback.stopped = true;
            app.status = trf!("audio: {error}", "аудио: {error}");
            if let Some(r) = app.recorder.as_mut() {
                r.error = Some(error);
                r.recording = false;
                r.busy = false;
            }
        }
        Msg::Audio(crate::audio::Event::Recorded {
            path,
            seconds,
            level,
            done,
        }) => {
            if let Some(r) = app.recorder.as_mut().filter(|r| r.path == path) {
                r.seconds = seconds;
                r.level = level;
                r.recording = !done;
                r.ready = done;
                r.busy = false;
                if done {
                    analyse_audio(app, &path, shared);
                }
            }
        }

        Msg::NowPlaying { account, chat, text } => match text {
            Ok(text) => {
                if let Some(index) = app
                    .chats
                    .iter()
                    .position(|c| c.account == account && c.jid == chat)
                {
                    // /np is a standalone message. A reply or edit started while
                    // playerctl was running belongs to the user's draft.
                    let replying = app.replying.take();
                    let editing = app.editing.take();
                    let actions = app.send_to(index, text);
                    app.replying = replying;
                    app.editing = editing;
                    for action in actions {
                        tokio::spawn(perform(action, shared.clone()));
                    }
                }
            }
            Err(error) => app.status = error,
        },
        Msg::Incoming { account, message, at } => {
            // The database already keeps repeated MAM/offline copies once. The view
            // must merge them too, before an alert or another unread is generated.
            if message.direction == omtalk_omemo::Direction::Incoming
                && app.merge_duplicate(&account, &message)
            {
                return;
            }
            // A correction changes the message it names, where it is: no alert.
            if app.correct(&account, &message) {
                return;
            }
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
            // Catch-up can bring older history after a local or remote read
            // frontier was already applied. Inserting it must not unread it again.
            flush_local_reads(app, &shared.store);
            if let Some(chat) = app
                .chats
                .iter_mut()
                .find(|c| c.account == account && c.jid == message.peer)
            {
                if let Some(id) = &chat.read_mark {
                    let _ = shared.store.read_remotely(&account, &chat.jid, id);
                }
                if let Ok(Some(unread)) = shared.store.unread_after_mark(&account, &chat.jid, None) {
                    chat.unread = unread;
                }
            }
            if let Some(text) = &message.text {
                fetch_if_media(text, message.shared_file, shared);
            }
        }
        Msg::Media { url, state } => {
            if let MediaState::Ready(path) = &state {
                analyse_audio(app, path, shared);
            }
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
            if let Some(kept) = kept_delivery(&state) {
                let _ = shared.store.set_delivery(&account, &id, &kept);
            }
            app.delivery(&account, &id, state)
        }
        Msg::Marked {
            account,
            from,
            id,
            state,
        } => {
            let kept = kept_delivery(&state);
            let read = state == Delivery::Read;
            if let Some(chat) = app.delivery_from(&account, &from, &id, state)
                && let Some(kept) = kept
            {
                let _ = shared.store.set_delivery_in(&account, &chat, &id, &kept);
                if read {
                    let _ = shared.store.set_read_up_to(&account, &chat, &id);
                }
            }
        }
        Msg::ReadMarks { account, marks } => {
            flush_local_reads(app, &shared.store);
            restore_local_unread(app, &shared.store, &account);
            // The user may have read further since a background task counted this.
            let marks = counted(&shared.store, &account, marks);
            for action in app.read_marks(&account, marks) {
                tokio::spawn(perform(action, shared.clone()));
            }
        }
        Msg::Restored {
            account,
            chat,
            mut lines,
        } => {
            prepare_media(&mut lines, shared);
            let index = app.chat(&account, &chat, ChatKind::Direct);
            let newer = std::mem::take(&mut app.chats[index].lines);
            app.chats[index].lines = lines;
            app.chats[index].lines.extend(newer);
        }
        Msg::Avatar { jid, hash } => match hash {
            Some(hash) => drop(app.avatars.insert(jid, hash)),
            None => drop(app.avatars.remove(&jid)),
        },
        // Kept for the next start, which shows them before the roster comes.
        Msg::Names {
            account,
            names,
            whole,
        } => {
            if whole {
                app.contacts.retain(|(a, _)| a != &account);
            }
            app.contacts
                .extend(names.iter().map(|(jid, _)| (account.clone(), jid.clone())));
            for (jid, name) in &names {
                let _ = shared
                    .store
                    .keep(&format!("{KEPT_CONTACT_NAME}{account}/{jid}"), name.as_deref());
            }
            // The whole roster: those kept from before who are no longer in it go.
            if whole {
                let listed: std::collections::BTreeSet<&BareJid> = names.iter().map(|(jid, _)| jid).collect();
                let gone: Vec<BareJid> = app
                    .names
                    .keys()
                    .filter(|(a, jid)| *a == account && !listed.contains(jid))
                    .map(|(_, jid)| jid.clone())
                    .collect();
                for jid in &gone {
                    let _ = shared
                        .store
                        .keep(&format!("{KEPT_CONTACT_NAME}{account}/{jid}"), None);
                }
                app.contact_names(&account, gone.into_iter().map(|jid| (jid, None)).collect());
            }
            app.contact_names(&account, names)
        }
        Msg::Nickname { account, jid, nick } => {
            let _ = shared
                .store
                .keep(&format!("{KEPT_NICKNAME}{account}/{jid}"), nick.as_deref());
            app.nickname(&account, &jid, nick)
        }
        Msg::Reactions { account, reactions } => drop(app.reacted(&account, &reactions)),
        Msg::Found {
            account,
            chat,
            words,
            found,
        } => app.found(&account, &chat, &words, found),
        Msg::Jumped {
            account,
            chat,
            lines,
            at,
        } => {
            let mut lines = lines;
            prepare_media(&mut lines, shared);
            app.jumped(&account, &chat, lines, at);
        }
        Msg::Older {
            account,
            chat,
            lines,
            before,
            done,
        } => {
            let mut lines = lines;
            prepare_media(&mut lines, shared);
            trace_scroll(
                app,
                &format!(
                    "older received account={account} chat={chat} lines={} done={done}",
                    lines.len()
                ),
            );
            app.older(&account, &chat, lines, before, done);
        }
        Msg::RoomName { account, room, name } => {
            // Kept for the next start, which shows it before anything is asked.
            let _ = shared
                .store
                .keep(&format!("{KEPT_ROOM_NAME}{account}/{room}"), name.as_deref());
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
        Msg::Online { account, online } => {
            let peer = online.account.clone();
            app.presence(&account, online, now());
            if app
                .current()
                .is_some_and(|c| c.kind == ChatKind::Direct && c.account == account && c.jid == peer)
            {
                for action in app.ask_bot_info(&account, &peer) {
                    tokio::spawn(perform(action, shared.clone()));
                }
            }
        }
        Msg::Bot { account, peer } => {
            app.mark_bot(&account, &peer);
            let _ = shared
                .store
                .keep(&format!("{KEPT_BOT}{account}/{peer}"), Some("1"));
        }
        Msg::Log { account, text } => app.server_log(&account, text, now()),
        Msg::Bound {
            account,
            resource,
            fresh,
        } => {
            // A new session hears every presence again; what was known is stale.
            if fresh {
                app.online.retain(|(a, _), _| *a != account);
                app.bot_checks.retain(|(a, _)| *a != account);
                // A new session gets a complete room roster. A nick absent from it
                // has left while we were away; keeping the old map would retain it.
                for chat in app.chats.iter_mut().filter(|c| c.account == account) {
                    chat.occupants.clear();
                    chat.occupant_status.clear();
                    chat.occupant_mark.clear();
                }
                if let Some(peer) = app
                    .current()
                    .filter(|c| c.kind == ChatKind::Direct && c.account == account)
                    .map(|c| c.jid.clone())
                {
                    for action in app.ask_bot_info(&account, &peer) {
                        tokio::spawn(perform(action, shared.clone()));
                    }
                }
            }
            app.own_resource.insert(account, resource);
        }
        Msg::Listed { jid, node, listing } => {
            if let Some(services) = app.services.as_mut().filter(|s| !s.room) {
                services.listed(&jid, &node, listing);
            }
        }
        Msg::RoomStep {
            token,
            account,
            room,
            node,
            title,
            mut step,
        } => {
            let topic = app
                .chats
                .iter()
                .find(|c| c.account == account && c.jid == room)
                .and_then(|c| c.topic.clone());
            if let Some(services) = app.services.as_mut().filter(|s| {
                s.room
                    && s.room_request.as_deref() == Some(&token)
                    && s.account == account
                    && s.jid == room
                    && s.node.as_deref() == Some(&node)
            }) {
                if let Ok(step) = &mut step
                    && let Some(form) = &mut step.form
                {
                    for field in &mut form.fields {
                        if node == "subject"
                            && field.var.as_deref() == Some("subject")
                            && let Some(topic) = &topic
                        {
                            field.values = vec![topic.clone()];
                        }
                        if matches!(node.as_str(), "owner" | "admin" | "member" | "outcast")
                            && field.var.as_deref() == Some("jid")
                            && let Some(jid) = &services.room_target
                        {
                            field.values = vec![jid.to_string()];
                        }
                        if node == "role"
                            && field.var.as_deref() == Some("nick")
                            && let Some(nick) = &services.room_nick
                        {
                            field.values = vec![nick.clone()];
                        }
                    }
                }
                services.stepped(room.into(), title, step);
            }
        }
        Msg::Stepped { jid, title, step } => {
            if let Some(services) = app.services.as_mut().filter(|s| !s.room) {
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
        Msg::Chats { account, peers } => {
            let closed = shared.store.closed_chats(&account).unwrap_or_default();
            for peer in peers.iter().filter(|p| !closed.contains(p)) {
                app.chat(&account, peer, ChatKind::Direct);
                let _ = shared.store.keep_chat(&account, peer);
            }
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
            // Its name as last told, until the room tells it again.
            if app.chats[index].name.is_none() {
                app.chats[index].name = shared
                    .store
                    .kept(&format!("{KEPT_ROOM_NAME}{account}/{room}"))
                    .ok()
                    .flatten();
            }
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
            status,
            mark,
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
                    if status == Default::default() {
                        chat.occupant_status.remove(&nick);
                    } else {
                        chat.occupant_status.insert(nick.clone(), status);
                    }
                    match mark {
                        Some(mark) => chat.occupant_mark.insert(nick.clone(), mark),
                        None => chat.occupant_mark.remove(&nick),
                    };
                    chat.occupants.insert(nick, real);
                } else {
                    chat.occupant_status.remove(&nick);
                    chat.occupant_mark.remove(&nick);
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
        if let Some(sync) = shared.read_sync.get(&account) {
            match &event {
                Event::Online { .. } | Event::Reachable => sync.online(true),
                Event::Disconnected { .. } | Event::AuthFailed { .. } => sync.online(false),
                _ => {}
            }
        }
        match &event {
            Event::Online { resumed, jid } => {
                // Clear the nickname cache before catch-up or room entry can use it.
                if let Err(e) = omemo.handle(&event).await {
                    tell(Msg::Notice {
                        account: account.clone(),
                        chat: None,
                        text: e.to_string(),
                    });
                }
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
                } else {
                    // A stream resumed after a while: a room may have let one go meanwhile.
                    tokio::spawn(still_in_rooms(
                        account.clone(),
                        xmpp.clone(),
                        omemo.clone(),
                        shared.clone(),
                    ));
                }
                continue;
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
                if let Some((room, by, topic)) =
                    muc::subject_of(message).filter(|(room, ..)| xmpp.in_room(room))
                {
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
                if let Some(room) = muc::config_changed(message).filter(|room| xmpp.in_room(room)) {
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
                // Reactions, kept whether or not their message is on screen; in a room
                // only from a room one is in.
                if let Some(reactions) = omtalk_xmpp::reactions::reactions(&account, message)
                    && (!reactions.room || xmpp.in_room(&reactions.chat))
                {
                    let _ = shared.store.set_reactions(
                        &account,
                        &reactions.chat,
                        &reactions.id,
                        &reactions.by,
                        &reactions.reactions,
                    );
                    tell(Msg::Reactions {
                        account: account.clone(),
                        reactions,
                    });
                }
                for (mark, from) in receipts::marks(&account, message) {
                    let (id, state) = match mark {
                        receipts::Mark::Received(id) => (id, Delivery::Delivered),
                        receipts::Mark::Displayed(id) => (id, Delivery::Read),
                    };
                    tell(Msg::Marked {
                        account: account.clone(),
                        from,
                        id,
                        state,
                    });
                }
                // One's message came back: the recipient's server (or one's own) refused it.
                if let Some((id, condition, text)) = receipts::bounce(message)
                    && let Some(from) = message.from.as_ref().map(|f| f.to_bare())
                {
                    tell(Msg::Marked {
                        account: account.clone(),
                        from,
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
                // Read on another device (or here, coming back): what came after is
                // all that is unread.
                let marks = receipts::read_marks(items);
                tell(Msg::ReadMarks {
                    account: account.clone(),
                    marks,
                });
            }
            // The name someone gave themselves (XEP-0172); one's own too, which one's
            // messages in direct chats go by.
            Event::Pep {
                from, node, items, ..
            } if node == omtalk_xmpp::nick::NODE => {
                if let Some(nick) = omtalk_xmpp::nick::of(items) {
                    tell(Msg::Nickname {
                        account: account.clone(),
                        jid: from.clone(),
                        nick,
                    });
                }
            }
            // A contact renamed (or added) by one of the account's clients.
            // One removed has no name there any more.
            Event::Roster(contacts) => tell(Msg::Names {
                account: account.clone(),
                names: contacts
                    .iter()
                    .map(|c| (c.jid.clone(), c.name.clone().filter(|_| !c.removed)))
                    .collect(),
                whole: false,
            }),
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
                trace_presence(&account, presence, &xmpp);
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
                // Only a room one is in says who is in it (anyone can send a presence
                // that looks like a room's).
                if let Some(occupant) = muc::occupant(presence).filter(|o| xmpp.in_room(&o.room)) {
                    tell(Msg::Occupant {
                        account: account.clone(),
                        room: occupant.room,
                        nick: occupant.nick,
                        real: occupant.real_jid,
                        present: occupant.present,
                        new_nick: occupant.new_nick,
                        mark: crate::app::standing_mark(&occupant.affiliation, &occupant.role),
                        status: occupant.status,
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
    let at = keep(shared, account, &message);
    let _ = shared.tx.send(Msg::Incoming {
        account: account.clone(),
        message,
        at,
    });
}

/// Keeps a message in the store (a correction in the message it corrects); when it was.
fn keep(shared: &Shared, account: &BareJid, message: &Incoming) -> i64 {
    let at = message.at.unwrap_or_else(now);
    if let (Some(id), Some(archive_id)) = (&message.id, &message.archive_id) {
        let _ = shared.store.set_archive_id(
            account,
            &message.peer,
            id,
            message.direction == Direction::Outgoing,
            archive_id,
        );
    }
    // The device wrote: when, for the keys window ("писало вчера 14:05").
    if let Some(device) = message.device
        && message.text.is_some()
    {
        let _ = shared.store.device_seen(account, &message.sender, device, at);
    }
    // A correction goes into the message it corrects; only one whose original is not
    // here is kept as a message of its own.
    let corrected = match (&message.replaces, &message.text) {
        (Some(id), Some(text)) => shared
            .store
            .correct_message(
                account,
                &message.peer,
                id,
                message.direction == Direction::Outgoing,
                text,
            )
            .unwrap_or(false),
        _ => false,
    };
    if let Some(text) = message.text.as_ref().filter(|_| !corrected) {
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
                shared_file: message.shared_file,
                reply_to: message.reply_to.clone(),
                archive_id: message.archive_id.clone(),
                untrusted: message.untrusted,
                edited: false,
                // One's own, come back from the room: it got there.
                delivery: (message.direction == Direction::Outgoing && message.nick.is_some())
                    .then_some(omtalk_store::Delivery::Sent),
            },
        );
    }
    at
}

/// Asks each room entered whether one is still in it (XEP-0410), and enters again the
/// ones that let one go (a network that changed, a server restarted): what was missed
/// comes with it. Silently, a room that let one go looks the same, but nothing comes.
async fn still_in_rooms(account: BareJid, xmpp: Account, omemo: Arc<Omemo>, shared: Shared) {
    for (room, nick) in xmpp.rooms() {
        if xmpp.self_ping(&room).await == omtalk_xmpp::muc::InRoom::No {
            shared
                .tx
                .send(Msg::Log {
                    account: account.clone(),
                    text: trf!(
                        "{room} let us go; entering again",
                        "{room} нас отпустила; входим снова"
                    ),
                })
                .ok();
            let entry = Entry {
                nick: Some(nick),
                ..Entry::default()
            };
            join(&account, &xmpp, &omemo, &shared, room, entry).await;
        }
    }
}

/// How often the rooms are asked whether one is still in them.
const ROOM_CHECK: std::time::Duration = std::time::Duration::from_secs(300);

/// Local progress survives a failed/missing server publication and a restart.
fn flush_local_reads(app: &mut App, store: &Store) {
    for ((account, chat), point) in std::mem::take(&mut app.local_reads) {
        if let Err(problem) = store.read_locally(&account, &chat, &point) {
            app.status = trf!(
                "read position was not saved: {problem}",
                "позиция прочитанного не сохранилась: {problem}"
            );
            app.local_reads.insert((account, chat), point);
        }
    }
}

fn restore_local_unread(app: &mut App, store: &Store, account: &BareJid) {
    for chat in app.chats.iter_mut().filter(|c| &c.account == account) {
        if let Ok(Some(unread)) = store.unread_after_mark(account, &chat.jid, None) {
            chat.unread = unread;
        }
    }
}

/// Server marks with an unread count respecting the durable local frontier too.
/// An unknown server ID stays unknown: it may be beyond the history kept here.
fn counted(
    store: &Store,
    account: &BareJid,
    marks: Vec<(BareJid, String)>,
) -> Vec<(BareJid, String, Option<usize>)> {
    marks
        .into_iter()
        .map(|(chat, id)| {
            let _ = store.read_remotely(account, &chat, &id);
            // Unknown may mean read further on another device than our history goes.
            // Local progress can restore the count, but must not make that ID "known"
            // and allow publishing an older server marker over it.
            let after = store
                .archive_time(account, &chat, &id)
                .ok()
                .flatten()
                .and_then(|_| store.unread_after_mark(account, &chat, Some(&id)).ok().flatten());
            (chat, id, after)
        })
        .collect()
}

/// With OMTALK_LOG set, each presence that comes into that file too, with what omtalk
/// makes of it: for finding out why a room's participants do not show.
fn trace_presence(account: &BareJid, presence: &xmpp_parsers::presence::Presence, xmpp: &Account) {
    use std::io::Write;
    let Some(path) = std::env::var_os("OMTALK_LOG") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let occupant = muc::occupant(presence);
    let in_room = occupant.as_ref().map(|o| xmpp.in_room(&o.room));
    let payloads: Vec<String> = presence.payloads.iter().map(String::from).collect();
    let _ = writeln!(
        file,
        "{} {account} presence from={:?} type={:?} occupant={:?} in_room={in_room:?} payloads={payloads:?}",
        chrono::Utc::now().format("%H:%M:%S%.3f"),
        presence.from.as_ref().map(|f| f.to_string()),
        presence.type_,
        occupant.map(|o| (o.nick, o.present)),
    );
}

/// After every new login: personal catch-up, room entry and read positions proceed
/// independently, so a slow archive or publication cannot hold the rooms up.
async fn come_online(account: BareJid, xmpp: Account, omemo: Arc<Omemo>, shared: Shared) {
    // What is read first, so that the archive coming next counts only what came after
    // it; but a slow answer must not hold the rooms up (READ_MARKS_FIRST), and what it
    // missed the second reading at the end puts right.
    let _ = tokio::time::timeout(READ_MARKS_FIRST, sync_read_marks(&account, &xmpp, &shared)).await;
    tokio::join!(
        come_online_messages(account.clone(), xmpp.clone(), omemo.clone(), shared.clone()),
        come_online_rooms(&account, &xmpp, &omemo, &shared),
    );
    sync_read_marks(&account, &xmpp, &shared).await;
}

/// How long the archive waits on login for what is read.
const READ_MARKS_FIRST: std::time::Duration = std::time::Duration::from_secs(3);

async fn come_online_messages(account: BareJid, xmpp: Account, omemo: Arc<Omemo>, shared: Shared) {
    let tell = |msg| drop(shared.tx.send(msg));
    // The first requests of a session sometimes go unanswered: tried again, so that
    // the device is in its list and its keys out for this session, not the next.
    let mut announced = omemo.announce().await;
    for wait in [3, 10] {
        match &announced {
            Err(e) if e.is_transient() => {
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                announced = omemo.announce().await;
            }
            _ => break,
        }
    }
    match announced {
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
    // One's own name (XEP-0172), which one's messages in direct chats go by; none
    // published is none.
    match xmpp.items(&account, omtalk_xmpp::nick::NODE).await {
        Ok(items) => tell(Msg::Nickname {
            account: account.clone(),
            jid: account.clone(),
            nick: omtalk_xmpp::nick::of(&items).flatten(),
        }),
        Err(omtalk_xmpp::Error::Stanza(_)) => tell(Msg::Nickname {
            account: account.clone(),
            jid: account.clone(),
            nick: None,
        }),
        Err(_) => {}
    }
    // The contacts' names, for the chats with them.
    if let Ok(contacts) = xmpp.roster().await {
        tell(Msg::Names {
            account: account.clone(),
            names: contacts.into_iter().map(|c| (c.jid, c.name)).collect(),
            whole: true,
        });
    }
    // Shown and kept page by page, as they are read.
    let mut got = 0;
    let caught = omemo
        .catch_up_each(
            |message| {
                got += usize::from(message.text.is_some());
                deliver(&shared, &account, message);
            },
            // Read or received while this client was closed: as if it had been open.
            |mark, from| {
                let (id, state) = match mark {
                    receipts::Mark::Received(id) => (id, Delivery::Delivered),
                    receipts::Mark::Displayed(id) => (id, Delivery::Read),
                };
                tell(Msg::Marked {
                    account: account.clone(),
                    from,
                    id,
                    state,
                });
            },
        )
        .await;
    match caught {
        Ok(caught) => {
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
            if !caught.peers.is_empty() {
                tell(Msg::Chats {
                    account: account.clone(),
                    peers: caught.peers.into_iter().collect(),
                });
            }
        }
        Err(e) => tell(Msg::Notice {
            account: account.clone(),
            chat: None,
            text: trf!("archive: {e}", "архив: {e}"),
        }),
    }
}

/// Read positions are useful before catch-up too: old archive pages must not
/// temporarily unread messages already read on another device.
async fn sync_read_marks(account: &BareJid, xmpp: &Account, shared: &Shared) {
    let tell = |msg| drop(shared.tx.send(msg));
    match xmpp.read_marks().await {
        Ok(marks) => tell(Msg::ReadMarks {
            account: account.clone(),
            marks,
        }),
        Err(e) => {
            tell(Msg::ReadMarks {
                account: account.clone(),
                marks: Vec::new(),
            });
            tell(Msg::Status(trf!(
                "what is read was not learnt from the server: {e}",
                "что прочитано, с сервера не узнать: {e}"
            )));
        }
    }
}

/// Enter rooms independently of personal catch-up and of each other's archives.
async fn come_online_rooms(account: &BareJid, xmpp: &Account, omemo: &Omemo, shared: &Shared) {
    // The rooms entered here before, and those the account's clients enter by themselves
    // (autojoin in its bookmarks, as Gajim and Conversations keep them): on a new device
    // the bookmarks are all there is.
    let mut rooms: Vec<BareJid> = shared
        .store
        .rooms(account)
        .unwrap_or_default()
        .into_iter()
        .map(|(room, _)| room)
        .collect();
    // Read once for all of them; unread, each room asks again. One closed here (/close,
    // /leave) stays closed here, though the account's other clients still enter it.
    let marks = xmpp.bookmarks().await.ok();
    let closed = shared.store.closed_chats(account).unwrap_or_default();
    for mark in marks.iter().flatten() {
        if mark.autojoin && !rooms.contains(&mark.room) && !closed.contains(&mark.room) {
            rooms.push(mark.room.clone());
        }
    }
    futures::future::join_all(rooms.into_iter().map(|room| async {
        let entry = Entry {
            bookmarks: marks.as_deref(),
            ..Entry::default()
        };
        join(account, xmpp, omemo, shared, room, entry).await;
    }))
    .await;
}

/// How a room is entered.
#[derive(Default)]
struct Entry<'a> {
    nick: Option<String>,
    /// Entered by the user just now: the room opens. Rooms entered again on start do not
    /// take the view.
    open: bool,
    /// The account's bookmarks, if read already; `None` reads them.
    bookmarks: Option<&'a [omtalk_xmpp::bookmarks::Bookmark]>,
}

async fn join(
    account: &BareJid,
    xmpp: &Account,
    omemo: &Omemo,
    shared: &Shared,
    room: BareJid,
    entry: Entry<'_>,
) {
    let Entry {
        nick,
        open,
        bookmarks,
    } = entry;
    let tell = |msg| drop(shared.tx.send(msg));
    // The nick: as asked, else the one the account's other clients use in this room (its
    // bookmark, so they all count as one occupant), else the last one used here, else,
    // as Gajim has it, the name one gave oneself (XEP-0172), else the address's name part.
    let fetched;
    let bookmarks = match bookmarks {
        Some(known) => Some(known),
        None => {
            fetched = xmpp.bookmarks().await.ok();
            fetched.as_deref()
        }
    };
    let bookmark = bookmarks.into_iter().flatten().find(|b| b.room == room);
    let known = nick
        .or_else(|| bookmark.and_then(|b| b.nick.clone()))
        .or_else(|| {
            let rooms = shared.store.rooms(account).unwrap_or_default();
            rooms.into_iter().find(|(r, _)| *r == room).map(|(_, nick)| nick)
        });
    let nick = match known {
        Some(nick) => nick,
        None => match xmpp.items(account, omtalk_xmpp::nick::NODE).await {
            Ok(items) => omtalk_xmpp::nick::of(&items).flatten(),
            Err(_) => None,
        }
        .unwrap_or_else(|| account.node().map(|n| n.to_string()).unwrap_or_default()),
    };
    match xmpp.join(&room, &nick).await {
        // Kept under the nick asked for (a gateway gives its own again); shown under the
        // one the room gave, which its messages come back under.
        Ok(given) => {
            let _ = shared.store.remember_room(account, &room, &nick);
            // Entered by hand: a room closed here before is open again.
            if open {
                let _ = shared.store.reopen_chat(account, &room);
            }
            tell(Msg::Log {
                account: account.clone(),
                text: trf!("joined {room} as {nick}", "вход в {room} как {nick}"),
            });
            // A room new to the account goes into its bookmarks, as Gajim does, so its
            // other clients know it and the nick. Not when they could not be read: one
            // there would be written over, its name lost.
            if bookmarks.is_some() && bookmark.is_none() {
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
                nick: given,
                open,
            });
            // Its name for the list at once, not only once it is opened.
            if let Ok(name) = xmpp.room_name(&room).await {
                tell(Msg::RoomName {
                    account: account.clone(),
                    room: room.clone(),
                    name,
                });
            }
            // Kept page by page; the room shows its newest, the rest comes on scrolling up.
            // A long absence says how far it has got.
            let name = room
                .node()
                .map(|n| n.to_string())
                .unwrap_or_else(|| room.to_string());
            let mut read = 0;
            let caught = omemo
                .catch_up_room_each(&room, |message| {
                    read += 1;
                    if read % 100 == 0 {
                        tell(Msg::Status(trf!(
                            "{name}: {read} from the archive…",
                            "{name}: из архива {read}…"
                        )));
                    }
                    deliver(shared, account, message);
                })
                .await;
            if read >= 100 {
                tell(Msg::Status(trf!(
                    "{name}: {read} from the archive",
                    "{name}: из архива {read}"
                )));
            }
            match caught {
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
                            text: trf!(
                                "{} encrypted messages from the archive cannot be read: written before this \
                                 device existed, or not for it",
                                "{} зашифр. сообщений из архива не прочитать: они написаны до появления \
                                 этого устройства или не для него",
                                caught.unreadable
                            ),
                        });
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
        "mp3" => "audio/mpeg",
        "wav" | "wave" => "audio/wav",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
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

/// How far a message got, as the store keeps it; `None` for a state not kept.
fn kept_delivery(state: &Delivery) -> Option<omtalk_store::Delivery> {
    match state {
        Delivery::Sent => Some(omtalk_store::Delivery::Sent),
        Delivery::Delivered => Some(omtalk_store::Delivery::Delivered),
        Delivery::Read => Some(omtalk_store::Delivery::Read),
        Delivery::Failed(why) => Some(omtalk_store::Delivery::Failed(why.clone())),
        Delivery::Uploading(_) | Delivery::Sending | Delivery::Queued => None,
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
        let they = if count == 1 { "it" } else { "they" };
        return format!(
            "{count} more {what} encrypted before this device existed: {they} cannot be read here"
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
        Action::SaveFile {
            id,
            source,
            target,
            overwrite,
        } => {
            let result = crate::file_save::copy(&source, &target, overwrite).await;
            tell(Msg::Saved { id, target, result });
        }

        Action::NowPlaying { account, chat } => {
            let text = crate::mpris::now_playing().await;
            tell(Msg::NowPlaying { account, chat, text });
        }
        // Written by the loop, which keeps what the runtime uses.
        Action::Audio(_)
        | Action::Setting { .. }
        | Action::SetMuted { .. }
        | Action::SetChatNotify { .. } => {}
        Action::Send {
            account,
            to,
            room,
            text,
            encrypted,
            id,
            reply,
            replace,
        } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            fetch_if_media(&text, false, &shared);
            let reply_to = reply.as_ref().map(|(to, _)| to.id.clone());
            // A reply names what it answers, a correction what it corrects, beside the
            // body (outside the encryption).
            let mut extra = reply
                .map(|(to, length)| omtalk_xmpp::replies::elements(&to, length))
                .unwrap_or_default();
            if let Some(original) = &replace {
                extra.push(omtalk_xmpp::corrections::element(original));
            }
            let delivery = |state| {
                tell(Msg::Delivery {
                    account: account.clone(),
                    id: id.clone(),
                    state,
                })
            };
            if encrypted {
                // This device's room echo has no decryptable body. Keep its known text
                // before sending, so that echo can attach the room id even before ACK.
                if room {
                    save_own(
                        &shared,
                        &account,
                        &to,
                        &text,
                        &id,
                        Own {
                            encrypted: true,
                            delivery: None,
                            shared_file: false,
                            reply_to: reply_to.clone(),
                            replaces: replace.clone(),
                        },
                    );
                }
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
                            &id,
                            Own {
                                encrypted: true,
                                delivery: Some(omtalk_store::Delivery::Sent),
                                shared_file: false,
                                reply_to: reply_to.clone(),
                                replaces: replace.clone(),
                            },
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
                        save_own(
                            &shared,
                            &account,
                            &to,
                            &text,
                            &id,
                            Own {
                                encrypted: true,
                                delivery: None,
                                shared_file: false,
                                reply_to: reply_to.clone(),
                                replaces: replace.clone(),
                            },
                        );
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
                                &id,
                                Own {
                                    encrypted: false,
                                    delivery: Some(omtalk_store::Delivery::Sent),
                                    shared_file: false,
                                    reply_to: reply_to.clone(),
                                    replaces: replace.clone(),
                                },
                            );
                        }
                        delivery(Delivery::Sent);
                    }
                    Err(omtalk_xmpp::Error::Queued) => {
                        if !room {
                            save_own(
                                &shared,
                                &account,
                                &to,
                                &text,
                                &id,
                                Own {
                                    encrypted: false,
                                    delivery: None,
                                    shared_file: false,
                                    reply_to: reply_to.clone(),
                                    replaces: replace.clone(),
                                },
                            );
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
            // In a room the room's archive names the message; in a chat, one's own.
            if let Some(id) = archive_id {
                let by = if room { to.clone() } else { account.clone() };
                if let Some(sync) = shared.read_sync.get(&account) {
                    sync.queue(crate::read_sync::ReadMark {
                        chat: to.clone(),
                        by,
                        id,
                    });
                }
            }
            if let Some(id) = marker {
                let _ = xmpp.send(receipts::displayed(to.clone().into(), &id)).await;
            }
        }
        Action::Older {
            account,
            chat,
            room,
            have,
            before,
        } => {
            // From the store first: what this device read, decrypted. Then the archive,
            // what is kept going into the store and coming from there in order.
            let page = |have| {
                let mut lines = stored_lines(
                    shared
                        .store
                        .older_messages(&account, &chat, have, OLDER_PAGE)
                        .unwrap_or_default(),
                );
                with_reactions(&shared.store, &account, &chat, room, &mut lines);
                lines
            };
            let lines = page(have);
            if !lines.is_empty() {
                tell(Msg::Older {
                    account,
                    chat,
                    lines,
                    before: None,
                    done: false,
                });
                return;
            }
            let Some((_, omemo)) = get(&account) else { return };
            let (before, done) = match omemo
                .history_before(&chat, room, before.as_deref().unwrap_or_default(), OLDER_PAGE)
                .await
            {
                Ok((caught, oldest, complete)) => {
                    for message in caught.messages.iter().filter(|m| m.text.is_some()) {
                        keep(&shared, &account, message);
                    }
                    // Said in the status line: a notice would go to the bottom, far from
                    // the old messages it is about.
                    if caught.unreadable > 0 {
                        tell(Msg::Status(unreadable_words(caught.unreadable)));
                    }
                    (oldest, complete)
                }
                Err(e) => {
                    tell(Msg::Status(trf!("older history: {e}", "старая история: {e}")));
                    (None, false)
                }
            };
            let lines = page(have);
            let done = done && lines.is_empty();
            tell(Msg::Older {
                account,
                chat,
                lines,
                before,
                done,
            });
        }
        Action::Search { account, chat, words } => {
            let found = shared
                .store
                .search(&account, &chat, &words, SEARCH_LIMIT)
                .unwrap_or_default()
                .into_iter()
                .map(|m| crate::app::Found {
                    at: m.at,
                    who: match (&m.nick, m.outgoing) {
                        (Some(nick), _) => nick.clone(),
                        (None, true) => tr!("me", "я").to_owned(),
                        (None, false) => m.sender.node().map(|n| n.to_string()).unwrap_or_default(),
                    },
                    text: m.body,
                })
                .collect();
            tell(Msg::Found {
                account,
                chat,
                words,
                found,
            });
        }
        Action::Jump {
            account,
            chat,
            room,
            at,
        } => {
            let mut lines = stored_lines(
                shared
                    .store
                    .messages_since(&account, &chat, at, JUMP_LIMIT)
                    .unwrap_or_default(),
            );
            with_reactions(&shared.store, &account, &chat, room, &mut lines);
            tell(Msg::Jumped {
                account,
                chat,
                lines,
                at,
            });
        }
        Action::ReopenChat { account, jid } => {
            let _ = shared.store.reopen_chat(&account, &jid);
        }
        Action::NoAutojoin { account, room } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let marks = match xmpp.bookmarks().await {
                Ok(marks) => marks,
                Err(e) => {
                    tell(Msg::Status(trf!(
                        "the bookmark of {room} was not changed: {e}",
                        "закладка {room} не изменилась: {e}"
                    )));
                    return;
                }
            };
            if let Some(mut mark) = marks.into_iter().find(|m| m.room == room && m.autojoin) {
                mark.autojoin = false;
                if let Err(e) = xmpp.bookmark(&mark).await {
                    tell(Msg::Status(trf!(
                        "the bookmark of {room} was not changed: {e}",
                        "закладка {room} не изменилась: {e}"
                    )));
                }
            }
        }
        Action::SetOmemo { account, chat, on } => {
            let key = format!("{KEPT_OMEMO}{account}/{chat}");
            let _ = shared.store.keep(&key, Some(if on { "1" } else { "0" }));
        }
        Action::SetStatus(status) => {
            // For the next run too: away stays away after a restart.
            let kept = (status != Default::default()).then(|| status.to_kept());
            let _ = shared.store.keep(KEPT_STATUS, kept.as_deref());
            for (xmpp, _) in shared.accounts.values() {
                if let Err(e) = xmpp.set_status(status.clone()).await
                    && !matches!(e, omtalk_xmpp::Error::Queued)
                {
                    tell(Msg::Status(trf!("status not sent: {e}", "статус не ушёл: {e}")));
                }
            }
        }
        Action::React {
            account,
            chat,
            room,
            id,
            by,
            reactions,
        } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let message = omtalk_xmpp::reactions::message(&chat, room, &id, &reactions);
            match xmpp.send(message).await {
                Ok(()) | Err(omtalk_xmpp::Error::Queued) => {
                    let _ = shared.store.set_reactions(&account, &chat, &id, &by, &reactions);
                }
                Err(e) => tell(Msg::Status(trf!(
                    "reaction not sent: {e}",
                    "реакция не ушла: {e}"
                ))),
            }
        }
        Action::Join { account, room, nick } => {
            let Some((xmpp, omemo)) = get(&account) else {
                return;
            };
            join(
                &account,
                &xmpp,
                &omemo,
                &shared,
                room,
                Entry {
                    nick,
                    open: true,
                    ..Entry::default()
                },
            )
            .await;
        }
        Action::Leave { account, room, nick } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let _ = xmpp.leave(&room, &nick).await;
            let _ = shared.store.forget_room(&account, &room);
            tell(Msg::Left { account, room });
        }
        Action::RemoveContact { account, jid } => {
            let Some((xmpp, _)) = get(&account) else { return };
            // The roster push that follows takes its name away.
            let text = match xmpp.remove_contact(&jid).await {
                Ok(()) => tr!("removed from the contacts", "удалён из контактов").to_owned(),
                Err(e) => trf!("not removed: {e}", "не удалён: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: Some(jid),
                text,
            });
        }
        Action::RenameContact { account, jid, name } => {
            let Some((xmpp, _)) = get(&account) else { return };
            // The roster push that follows shows the name everywhere.
            let text = match (xmpp.rename_contact(&jid, name.as_deref()).await, &name) {
                (Ok(()), Some(name)) => trf!("named {name} in your contacts", "в контактах — {name}"),
                (Ok(()), None) => {
                    tr!("no name in your contacts any more", "имя в контактах убрано").to_owned()
                }
                (Err(e), _) => trf!("the name did not change: {e}", "имя не сменилось: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: Some(jid),
                text,
            });
        }
        Action::SetAvatar { account, path } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let set = match path {
                // Read and made small aside: a large photo takes a moment.
                Some(path) => {
                    match tokio::task::spawn_blocking(move || crate::avatar::own_avatar(&path)).await {
                        Ok(Ok((png, side))) => xmpp
                            .publish_avatar(&png, side as u16, side as u16)
                            .await
                            .map(|_| true)
                            .map_err(|e| e.to_string()),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(e.to_string()),
                    }
                }
                None => xmpp
                    .remove_avatar()
                    .await
                    .map(|_| false)
                    .map_err(|e| e.to_string()),
            };
            let text = match set {
                Ok(true) => tr!("your avatar is set", "аватарка поставлена").to_owned(),
                Ok(false) => tr!("your avatar is taken away", "аватарка убрана").to_owned(),
                Err(e) => trf!("the avatar did not change: {e}", "аватарка не сменилась: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: None,
                text,
            });
        }
        Action::SetNickname { account, nick } => {
            let Some((xmpp, _)) = get(&account) else { return };
            let set = xmpp.set_nickname(nick.as_deref()).await;
            if set.is_ok() {
                tell(Msg::Nickname {
                    account: account.clone(),
                    jid: account.clone(),
                    nick: nick.clone(),
                });
            }
            let text = match (set, &nick) {
                (Ok(()), Some(nick)) => trf!(
                    "your contacts now see you as {nick}",
                    "контакты теперь видят вас как {nick}"
                ),
                (Ok(()), None) => tr!(
                    "your name is taken away; contacts see your address",
                    "ваше имя убрано; контакты видят адрес"
                )
                .to_owned(),
                (Err(e), _) => trf!("the name did not change: {e}", "имя не сменилось: {e}"),
            };
            tell(Msg::Notice {
                account,
                chat: None,
                text,
            });
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
            if let Some(media) = omtalk_media::Media::shared(&link) {
                let cached = media.cache_path(&shared.cache);
                if let Some(dir) = cached.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                let _ = tokio::fs::copy(&path, &cached).await;
            }
            fetch_if_media(&link, true, &shared);
            if encrypted && room {
                save_own(
                    &shared,
                    &account,
                    &to,
                    &link,
                    &id,
                    Own {
                        encrypted: true,
                        delivery: None,
                        shared_file: true,
                        reply_to: None,
                        replaces: None,
                    },
                );
            }
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
                            &id,
                            Own {
                                encrypted,
                                delivery: Some(omtalk_store::Delivery::Sent),
                                shared_file: true,
                                reply_to: None,
                                replaces: None,
                            },
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
            let seen = shared.store.devices_seen(&account, &jid).unwrap_or_default();
            let rows = |devices: Vec<omtalk_omemo::DeviceInfo>| -> Vec<DeviceRow> {
                devices
                    .into_iter()
                    .map(|d| DeviceRow {
                        seen: seen.get(&d.device).copied(),
                        device: d.device,
                        fingerprint: d.identity_key.map(|k| omtalk_omemo::fingerprint(&k)),
                        has_session: d.has_session,
                        this_device: d.this_device,
                        trust: d.trust,
                    })
                    .collect()
            };
            // What is known shows at once; the server's list follows.
            let known = omemo.known_devices(&jid).await;
            if !known.is_empty() {
                tell(Msg::Devices {
                    account: account.clone(),
                    jid: jid.clone(),
                    rows: rows(known),
                });
            }
            match omemo.devices(&jid).await {
                Ok(devices) => {
                    let rows = rows(devices);
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
                Request::Browse { account, .. }
                | Request::Execute { account, .. }
                | Request::Room { account, .. } => account.clone(),
            };
            let Some((xmpp, _)) = get(&account) else { return };
            match request {
                Request::Room {
                    token,
                    room,
                    node,
                    form,
                    ..
                } => {
                    let step = crate::room_admin::step(&xmpp, &room, &node, form).await;
                    let saved = step
                        .as_ref()
                        .is_ok_and(|s| s.status == omtalk_xmpp::services::CommandStatus::Completed);
                    tell(Msg::RoomStep {
                        token,
                        account: account.clone(),
                        room: room.clone(),
                        node: node.clone(),
                        title,
                        step,
                    });
                    if saved {
                        if node == "avatar" {
                            tokio::spawn(load_avatar(
                                shared.clone(),
                                xmpp.clone(),
                                room.clone(),
                                AvatarFrom::VCard(None),
                            ));
                        }
                        tokio::spawn(async move {
                            if let Ok(name) = xmpp.room_name(&room).await {
                                let _ = shared.tx.send(Msg::RoomName {
                                    account: account.clone(),
                                    room: room.clone(),
                                    name,
                                });
                            }
                            let members = read_members(&xmpp, &room).await;
                            let _ = shared.tx.send(Msg::Members {
                                account,
                                room,
                                members,
                            });
                        });
                    }
                }
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
            join(
                &account,
                &xmpp,
                &omemo,
                &shared,
                room.clone(),
                Entry {
                    nick,
                    open: true,
                    ..Entry::default()
                },
            )
            .await;
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
                Ok(_) => {
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
            // A chat closed and opened again has its history here: that, not the archive
            // read a second time (and every message kept twice).
            let kept = shared
                .store
                .recent_messages(&account, &peer, HISTORY)
                .unwrap_or_default();
            if !kept.is_empty() {
                let mut lines = stored_lines(kept);
                with_reactions(&shared.store, &account, &peer, false, &mut lines);
                tell(Msg::Restored {
                    account,
                    chat: peer,
                    lines,
                });
                return;
            }
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
            fetch_media(link, shared.clone()).await;
        }
        Action::BotInfo { account, jid } => {
            let Some((xmpp, _)) = get(&account) else { return };
            if let Ok(info) = xmpp.disco_info(&jid, None).await
                && info.is_bot()
            {
                tell(Msg::Bot {
                    account,
                    peer: jid.to_bare(),
                });
            }
        }
        Action::OpenUrl(url) => {
            let opened = tokio::process::Command::new("xdg-open")
                .arg(&url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            match opened {
                Ok(mut child) => {
                    tokio::spawn(async move {
                        let _ = child.wait().await;
                    });
                }
                Err(e) => tell(Msg::Status(format!("xdg-open: {e}"))),
            }
        }
        Action::Open(path) => {
            // Detached and silent: the viewer must not write over the interface. Waited
            // for aside, so that it leaves no zombie behind.
            let opened = std::process::Command::new(&shared.viewer)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            match opened {
                Ok(mut child) => drop(tokio::task::spawn_blocking(move || child.wait())),
                Err(e) => tell(Msg::Status(format!("{}: {e}", shared.viewer))),
            }
        }
        Action::Quit => {}
    }
}

/// Where one's own status is kept between runs.
const KEPT_STATUS: &str = "status";

/// Where contacts' names in the roster are kept: this, then "account/contact".
const KEPT_CONTACT_NAME: &str = "contact-name/";

/// Where the names contacts gave themselves (XEP-0172) are kept, as the roster's.
const KEPT_NICKNAME: &str = "nickname/";

/// Where rooms' names are kept: this, then "account/room".
const KEPT_ROOM_NAME: &str = "room-name/";

/// Where the chat open when omtalk was left is kept: "account\tchat".
const KEPT_OPEN: &str = "open";

/// Where the folded folders and accounts of the chat list are kept.
const KEPT_FOLDED: &str = "folded";

/// The folded groups as kept: a line each, "account\tJID" or "folder\tJID\tname".
fn folded_to_kept(folded: &std::collections::BTreeSet<crate::app::Group>) -> String {
    folded
        .iter()
        .map(|group| match group {
            crate::app::Group::Account(account) => format!("account\t{account}"),
            crate::app::Group::Folder(account, name) => format!("folder\t{account}\t{name}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn folded_from_kept(kept: &str) -> std::collections::BTreeSet<crate::app::Group> {
    kept.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            match (parts.next()?, parts.next()?.parse().ok()?, parts.next()) {
                ("account", account, None) => Some(crate::app::Group::Account(account)),
                ("folder", account, Some(name)) => Some(crate::app::Group::Folder(account, name.to_owned())),
                _ => None,
            }
        })
        .collect()
}

/// Local notification mutes, by "account/chat".
const KEPT_MUTED: &str = "muted/";
const KEPT_BOT: &str = "bot/";
const KEPT_CHAT_NOTIFY: &str = "chat-notify/";

fn save_chat_notify(
    store: &Store,
    account: &BareJid,
    peer: &BareJid,
    prefs: &crate::notify::ChatNotify,
) -> Result<(), String> {
    let value = toml::to_string(prefs).map_err(|e| e.to_string())?;
    store
        .keep(
            &format!("{KEPT_CHAT_NOTIFY}{account}/{peer}"),
            (prefs != &crate::notify::ChatNotify::default()).then_some(value.as_str()),
        )
        .map_err(|e| e.to_string())
}

fn restore_chat_notify(store: &Store, app: &mut App) {
    for (key, value) in store.kept_under(KEPT_CHAT_NOTIFY).unwrap_or_default() {
        if let Some((account, peer)) = key.split_once('/')
            && let (Ok(account), Ok(peer), Ok(prefs)) =
                (account.parse(), peer.parse(), toml::from_str(&value))
        {
            app.chat_notify.insert((account, peer), prefs);
        }
    }
}

fn restore_bots(store: &Store, app: &mut App) {
    for (key, value) in store.kept_under(KEPT_BOT).unwrap_or_default() {
        if value == "1"
            && let Some((account, peer)) = key.split_once('/')
            && let (Ok(account), Ok(peer)) = (account.parse::<BareJid>(), peer.parse::<BareJid>())
        {
            app.mark_bot(&account, &peer);
        }
    }
}

fn save_mute(
    store: &Store,
    account: &BareJid,
    chat: &BareJid,
    muted: bool,
) -> Result<(), omtalk_store::Error> {
    store.keep(&format!("{KEPT_MUTED}{account}/{chat}"), muted.then_some("1"))
}

fn restore_mutes(store: &Store, app: &mut App) {
    for (key, value) in store.kept_under(KEPT_MUTED).unwrap_or_default() {
        if value == "1"
            && let Some((account, chat)) = key.split_once('/')
            && let (Ok(account), Ok(chat)) = (account.parse::<BareJid>(), chat.parse::<BareJid>())
        {
            app.muted_chats.insert((account, chat));
        }
    }
}

/// Where a chat's OMEMO on or off is kept: this, then "account/chat".
const KEPT_OMEMO: &str = "omemo/";

/// How many results `/search` shows.
const SEARCH_LIMIT: usize = 200;

/// How many messages a chat shown from a search result holds from it on.
const JUMP_LIMIT: usize = 5000;

/// How many older messages one scroll to the top brings.
const OLDER_PAGE: usize = 50;

/// How a message sent from here is kept.
struct Own {
    encrypted: bool,
    delivery: Option<omtalk_store::Delivery>,
    /// A file shared (one's own upload), not text.
    shared_file: bool,
    /// The id of the message it answers.
    reply_to: Option<String>,
    /// The id of one's own message it corrects: that one takes its text instead.
    replaces: Option<String>,
}

/// Keeps a message sent from here, as on the server: its ✓ is kept with it.
fn save_own(shared: &Shared, account: &BareJid, chat: &BareJid, text: &str, id: &str, own: Own) {
    if let Some(original) = &own.replaces {
        let _ = shared.store.correct_message(account, chat, original, true, text);
        return;
    }
    let _ = shared.store.save_message(
        account,
        &StoredMessage {
            chat: chat.clone(),
            sender: account.clone(),
            nick: None,
            outgoing: true,
            encrypted: own.encrypted,
            body: text.to_owned(),
            at: now(),
            id: Some(id.to_owned()),
            delivery: own.delivery,
            shared_file: own.shared_file,
            reply_to: own.reply_to,
            archive_id: None,
            untrusted: false,
            edited: false,
        },
    );
}

/// History and preference changes use the same recognition/download policy as live messages.
fn analyse_audio(app: &mut App, path: &std::path::Path, shared: &Shared) {
    if !crate::audio::supported(path)
        || app.audio_info.contains_key(path)
        || !app.audio_pending.insert(path.to_owned())
    {
        return;
    }
    let path = path.to_owned();
    let tx = shared.tx.clone();
    tokio::spawn(async move {
        static SCANS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
        let Ok(_permit) = SCANS.acquire().await else {
            return;
        };
        let scan = path.clone();
        let info = tokio::task::spawn_blocking(move || crate::audio::inspect(&scan))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let _ = tx.send(Msg::AudioInfo(path, info));
    });
}

fn prepare_media(lines: &mut [Line], shared: &Shared) {
    let mut links = Vec::new();
    for line in lines {
        line.set_image_links(shared.image_links);
        if let Some(media) = &line.media {
            links.push(media.link.clone());
        }
    }
    links.sort();
    links.dedup();
    for link in links {
        fetch_if_media(&link, true, shared);
    }
}

fn fetch_if_media(text: &str, shared_file: bool, shared: &Shared) {
    let media = if shared_file {
        omtalk_media::Media::shared(text)
    } else {
        omtalk_media::Media::from_text(text).or_else(|| {
            shared
                .image_links
                .then(|| omtalk_media::Media::image_link(text))
                .flatten()
        })
    };
    let Some(media) = media else {
        return;
    };
    // Not by itself: what is here already shows, the rest waits for a click.
    if !shared.auto_download && media.cached(&shared.cache).is_none() {
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
    // An id names a file in the cache: only an avatar's SHA-1, never a path. Anyone may
    // send one ("../", "/etc/…").
    let is_hash = |id: &str| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit());
    if matches!(&from, AvatarFrom::Pep(id) | AvatarFrom::VCard(Some(id)) if !is_hash(id)) {
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
            // No avatar in PEP (or no PEP to read): perhaps one in the vCard.
            Ok(None) | Err(omtalk_xmpp::Error::Stanza(_)) => AvatarFrom::VCard(None),
            // No answer is not "no avatar": the vCard of one who has it in PEP only
            // has none, and what was drawn would go. It stays.
            Err(_) => {
                shared.avatar_loads.lock().unwrap().remove(&busy);
                return;
            }
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
    let Some(media) = omtalk_media::Media::shared(&link) else {
        return;
    };
    static ACTIVE: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::LazyLock::new(Default::default);
    struct Flight(PathBuf);
    impl Drop for Flight {
        fn drop(&mut self) {
            ACTIVE.lock().unwrap().remove(&self.0);
        }
    }
    let key = media.cache_path(&shared.cache);
    if !ACTIVE.lock().unwrap().insert(key.clone()) {
        return;
    }
    let _flight = Flight(key);
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
    if media.cached(&shared.cache).is_none() {
        let _ = shared.tx.send(Msg::Media {
            url: media.url().to_owned(),
            state: MediaState::Loading,
        });
    }
    let retrying = |attempt| {
        let _ = shared.tx.send(Msg::Media {
            url: media.url().to_owned(),
            state: MediaState::Retrying(attempt),
        });
    };
    let state = match media.fetch_with_retry(&shared.cache, progress, retrying).await {
        Ok(path) => MediaState::Ready(path),
        Err(error) => MediaState::Failed(error.to_string()),
    };
    let _ = shared.tx.send(Msg::Media {
        url: media.url().to_owned(),
        state,
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_notification_overrides_survive_restart_and_reset_to_inherit() {
        use super::*;
        use crate::config::Modifier;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("prefs.db")).unwrap();
        let me: BareJid = "me@example.org".parse().unwrap();
        let other: BareJid = "other@example.org".parse().unwrap();
        let peer: BareJid = "#room%irc.example.org@bridge.example.org".parse().unwrap();
        let prefs = crate::notify::ChatNotify {
            mentions_only: Some(true),
            sound: Some(false),
        };
        save_chat_notify(&store, &me, &peer, &prefs).unwrap();
        let mut app = App::new(vec![me.clone(), other.clone()], Modifier::Ctrl, 26, 30);
        restore_chat_notify(&store, &mut app);
        assert_eq!(app.chat_notify.get(&(me.clone(), peer.clone())), Some(&prefs));
        assert!(!app.chat_notify.contains_key(&(other, peer.clone())));
        save_chat_notify(&store, &me, &peer, &Default::default()).unwrap();
        let mut reset = App::new(vec![me], Modifier::Ctrl, 26, 30);
        restore_chat_notify(&store, &mut reset);
        assert!(reset.chat_notify.is_empty());
    }

    #[test]
    fn a_new_session_rebuilds_room_rosters_but_a_resumed_one_keeps_them() {
        use super::*;
        use crate::config::Modifier;
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: Store::open(&dir.path().join("state.db")).unwrap(),
            tx,
        };
        let me: BareJid = "me@example.org".parse().unwrap();
        let other: BareJid = "other@example.org".parse().unwrap();
        let room: BareJid = "#usue%irc.example.org@bridge.example.org".parse().unwrap();
        let second: BareJid = "chat@conference.example.org".parse().unwrap();
        for fresh in [false, true] {
            let mut app = App::new(vec![me.clone(), other.clone()], Modifier::Ctrl, 26, 30);
            let mut chats = Vec::new();
            for (account, jid) in [(&me, &room), (&me, &second), (&other, &room)] {
                let index = app.chat(account, jid, ChatKind::Room { nick: "me".into() });
                let chat = &mut app.chats[index];
                chat.occupants
                    .insert("V_for_Vendetta".into(), Some(other.clone()));
                chat.occupant_status
                    .insert("V_for_Vendetta".into(), Default::default());
                chat.occupant_mark.insert("V_for_Vendetta".into(), "@");
                chat.members.push(other.clone());
                chat.lines
                    .push(Line::message(1, "nick".into(), "history".into(), false, false));
                chat.unread = 3;
                chat.read_mark = Some("read-before-sleep".into());
                chats.push(index);
            }
            apply(
                &mut app,
                Msg::Bound {
                    account: me.clone(),
                    resource: "same-device".into(),
                    fresh,
                },
                &shared,
            );
            for (i, index) in chats.iter().enumerate() {
                let chat = &app.chats[*index];
                let cleared = fresh && i < 2;
                assert_eq!(chat.occupants.is_empty(), cleared);
                assert_eq!(chat.occupant_status.is_empty(), cleared);
                assert_eq!(chat.occupant_mark.is_empty(), cleared);
                assert_eq!(chat.members, vec![other.clone()], "membership is not presence");
                assert_eq!(chat.lines.len(), 1);
                assert_eq!(chat.unread, 3);
                assert_eq!(chat.read_mark.as_deref(), Some("read-before-sleep"));
            }
            // The new snapshot contains only the replacement nick, with no leave
            // for the old one: exactly what happens after sleeping through a QUIT.
            apply(
                &mut app,
                Msg::Occupant {
                    account: me.clone(),
                    room: room.clone(),
                    nick: "V_for_Vendetta_".into(),
                    real: None,
                    present: true,
                    new_nick: None,
                    status: Default::default(),
                    mark: None,
                },
                &shared,
            );
            let occupants = &app.chats[chats[0]].occupants;
            assert!(occupants.contains_key("V_for_Vendetta_"));
            assert_eq!(occupants.contains_key("V_for_Vendetta"), !fresh);
            assert_eq!(occupants.len(), if fresh { 1 } else { 2 });
        }
    }

    #[test]
    fn discovered_bot_is_restored_before_its_chat_is_opened() {
        use super::*;
        use crate::config::Modifier;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state.db")).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        let me: BareJid = "me@example.org".parse().unwrap();
        let peer: BareJid = "helper@example.org".parse().unwrap();
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        apply(
            &mut app,
            Msg::Bot {
                account: me.clone(),
                peer: peer.clone(),
            },
            &shared,
        );
        let mut restarted = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        restore_bots(&store, &mut restarted);
        let chat = restarted.chat(&me, &peer, ChatKind::Direct);
        assert!(restarted.chats[chat].bot);
    }

    #[tokio::test]
    async fn image_link_history_respects_manual_downloads_and_uploaded_files() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: true,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: Store::open(&dir.path().join("history.db")).unwrap(),
            tx,
        };
        let mut lines = vec![
            Line::message(
                1,
                "bob".into(),
                "https://example.org/cat.jpg".into(),
                false,
                false,
            ),
            Line::message(
                2,
                "bob".into(),
                "https://example.org/report.pdf".into(),
                false,
                false,
            ),
            Line::message_sharing(
                3,
                "bob".into(),
                "https://upload.example.org/photo.png".into(),
                false,
                false,
                true,
            ),
        ];
        prepare_media(&mut lines, &shared);
        assert!(lines[0].media.is_some());
        assert!(lines[1].media.is_none(), "arbitrary links must not become files");
        let mut offered = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Media {
                    url,
                    state: MediaState::Offered,
                } => offered.push(url),
                _ => panic!("manual download must offer files without starting a fetch"),
            }
        }
        assert_eq!(offered.len(), 2);
        assert!(offered.contains(&"https://example.org/cat.jpg".into()));
        assert!(offered.contains(&"https://upload.example.org/photo.png".into()));
        shared.image_links = false;
        prepare_media(&mut lines, &shared);
        assert!(lines[0].media.is_none());
        assert!(
            lines[2].media.is_some(),
            "uploads remain attachments with previews off"
        );
        assert!(
            matches!(rx.try_recv(), Ok(Msg::Media { url, state: MediaState::Offered }) if url == "https://upload.example.org/photo.png")
        );
        assert!(
            rx.try_recv().is_err(),
            "disabled ordinary links never start a download"
        );
    }

    #[cfg(feature = "insecure-tcp")]
    async fn login_plain(
        user: &str,
    ) -> (
        super::Accounts,
        tokio::sync::mpsc::UnboundedReceiver<super::AccountEvent>,
        super::Account,
    ) {
        use super::*;
        use std::time::Duration;
        use tokio::time::timeout;
        let mut cfg = AccountConfig::new(format!("{user}@localhost").parse().unwrap(), "test");
        cfg.server = Server::Plaintext {
            addr: "127.0.0.1:15222".into(),
        };
        let (mut accounts, mut events) = Accounts::new();
        let account = accounts.add(cfg).await;
        timeout(Duration::from_secs(10), async {
            while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
        })
        .await
        .expect("local Prosody online");
        (accounts, events, account)
    }

    #[cfg(feature = "insecure-tcp")]
    #[tokio::test(flavor = "multi_thread")]
    async fn startup_enters_a_ready_room_while_another_room_does_not_answer() {
        use super::*;
        use std::time::Duration;
        use tokio::time::timeout;
        let (_accounts, events, account) = login_plain("u79").await;
        let (_silent_accounts, _silent_events, silent) = login_plain("u80").await;
        let me = account.jid().clone();
        let ready: BareJid = format!("zstartup-{}@muc.localhost", receipts::new_id())
            .parse()
            .unwrap();
        // A connected ordinary client receives presence but never sends the MUC
        // self-presence that completes join. It must not hold the real room up.
        let slow = silent.jid().clone();
        let omemo = Arc::new(Omemo::new(
            account.clone(),
            State::new(OwnKeys::generate(0).unwrap()),
        ));
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: [(me.clone(), (account.clone(), omemo.clone()))].into(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: Store::open(&dir.path().join("history.db")).unwrap(),
            tx,
        };
        for room in [&slow, &ready] {
            shared.store.remember_room(&me, room, "me").unwrap();
        }
        let pump = tokio::spawn(pump(events, shared.clone()));
        let startup = tokio::spawn(come_online(me, account.clone(), omemo, shared));
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(rx.recv().await, Some(Msg::Joined { room, .. }) if room == ready) {
                    break;
                }
            }
        })
        .await
        .expect("a silent room must not delay entry into a ready room");
        assert!(
            !startup.is_finished(),
            "the other room is still waiting for its self-presence"
        );
        startup.abort();
        pump.abort();
        account.leave(&ready, "me").await.unwrap();
        account
            .bookmark(&omtalk_xmpp::bookmarks::Bookmark {
                room: ready,
                name: None,
                nick: Some("me".into()),
                autojoin: false,
            })
            .await
            .unwrap();
    }

    #[cfg(feature = "insecure-tcp")]
    #[tokio::test(flavor = "multi_thread")]
    async fn plain_messages_to_self_echo_into_the_original_line_and_history() {
        use super::*;
        use crate::config::Modifier;
        use std::time::Duration;
        use tokio::time::timeout;
        let (_accounts, mut events, account) = login_plain("u76").await;
        let me = account.jid().clone();
        let omemo = Omemo::new(
            account.clone(),
            omtalk_omemo::State::new(OwnKeys::generate(0).unwrap()),
        );
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let chat = app.chat(&me, &me, ChatKind::Direct);
        app.select(chat);
        let unique = receipts::new_id();
        let mut received = Vec::new();
        for picture in [false, true] {
            let id = receipts::new_id();
            let text = if picture {
                format!("https://upload.example.org/{unique}.png")
            } else {
                format!("note to self {unique}")
            };
            app.sending(chat, &text, false, &id, now());
            if picture {
                app.uploaded(&me, &id, &text);
            }
            save_own(
                &shared,
                &me,
                &me,
                &text,
                &id,
                Own {
                    encrypted: false,
                    delivery: Some(omtalk_store::Delivery::Sent),
                    shared_file: picture,
                    reply_to: None,
                    replaces: None,
                },
            );
            let mut sent = Message::chat(Some(me.clone().into())).with_body(Lang::default(), text.clone());
            receipts::mark_outgoing(&mut sent, &id);
            if picture {
                sent.payloads.push(
                    xmpp_parsers::minidom::Element::builder("x", "jabber:x:oob")
                        .append(
                            xmpp_parsers::minidom::Element::builder("url", "jabber:x:oob")
                                .append(text.as_str())
                                .build(),
                        )
                        .build(),
                );
            }
            account.send(sent).await.unwrap();
            let message = timeout(Duration::from_secs(10), async {
                loop {
                    let event = events.recv().await.unwrap();
                    if let Some(message) = omemo.handle(&event.event).await.unwrap()
                        && message.id.as_deref() == Some(id.as_str())
                    {
                        break message;
                    }
                }
            })
            .await
            .unwrap();
            eprintln!(
                "self echo: {:?}, id={:?}, archive={:?}",
                message.direction, message.id, message.archive_id
            );
            received.push(message.clone());
            let at = keep(&shared, &me, &message);
            apply(
                &mut app,
                Msg::Incoming {
                    account: me.clone(),
                    message,
                    at,
                },
                &shared,
            );
        }
        let history = omemo.catch_up().await.unwrap();
        account.stop().await;
        assert_eq!(
            app.chats[chat].lines.len(),
            2,
            "one text and one upload, each echoed only once"
        );
        assert_eq!(app.chats[chat].unread, 0, "self echoes cannot create unread");
        assert!(
            received
                .iter()
                .all(|m| m.direction == omtalk_omemo::Direction::Outgoing)
        );
        for message in history
            .messages
            .into_iter()
            .filter(|m| m.text.as_ref().is_some_and(|t| t.contains(&unique)))
        {
            let at = keep(&shared, &me, &message);
            apply(
                &mut app,
                Msg::Incoming {
                    account: me.clone(),
                    message,
                    at,
                },
                &shared,
            );
        }
        assert_eq!(app.chats[chat].lines.len(), 2, "archive copies must merge too");
        assert!(app.chats[chat].lines.iter().all(|l| l.outgoing && !l.encrypted));
        assert!(app.chats[chat].lines[1].media.is_some());
        let mut restarted = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        load_history(&mut restarted, &store, std::slice::from_ref(&me));
        let chat = restarted.chats.iter().find(|c| c.jid == me).unwrap();
        assert_eq!(chat.lines.len(), 2);
        assert!(chat.lines.iter().all(|l| l.outgoing));
    }

    // Run with `cargo test -p omtalk --features insecure-tcp offline_plain_picture -- --nocapture`
    // against `mise run prosody`; no external upload service or real account is used.
    #[cfg(feature = "insecure-tcp")]
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_plain_picture_is_shown_once_when_offline_delivery_overlaps_mam() {
        use super::*;
        use crate::config::Modifier;
        use std::time::Duration;
        use tokio::time::timeout;
        let (_old, _, old) = login_plain("u78").await;
        old.stop().await;
        let (_sender_accounts, _, sender) = login_plain("u77").await;
        let recipient: BareJid = "u78@localhost".parse().unwrap();
        let link = format!("https://upload.example.org/{}.png", receipts::new_id());
        let mut sent = Message::chat(Some(recipient.clone().into())).with_body(Lang::default(), link.clone());
        sent.payloads.push(
            xmpp_parsers::minidom::Element::builder("x", "jabber:x:oob")
                .append(
                    xmpp_parsers::minidom::Element::builder("url", "jabber:x:oob")
                        .append(link.as_str())
                        .build(),
                )
                .build(),
        );
        receipts::mark_outgoing(&mut sent, &receipts::new_id());
        sender.send(sent).await.unwrap();
        let (_receiver_accounts, mut events, receiver) = login_plain("u78").await;
        let omemo = Omemo::new(
            receiver.clone(),
            omtalk_omemo::State::new(OwnKeys::generate(0).unwrap()),
        );
        let caught = omemo.catch_up().await.unwrap();
        let mut messages: Vec<_> = caught
            .messages
            .into_iter()
            .filter(|m| m.text.as_deref() == Some(&link))
            .collect();
        let archived = messages.len();
        // Login has already queued offline stanzas while MAM was being queried.
        while let Ok(Some(event)) = timeout(Duration::from_millis(500), events.recv()).await {
            if let Some(message) = omemo.handle(&event.event).await.unwrap()
                && message.text.as_deref() == Some(&link)
            {
                messages.push(message);
            }
        }
        sender.stop().await;
        receiver.stop().await;
        assert_eq!(archived, 1, "the uploaded link must be in MAM");
        assert!(
            messages.len() >= 2,
            "must reproduce both offline delivery and MAM: {messages:?}"
        );
        eprintln!(
            "offline/MAM copies: {:?}",
            messages
                .iter()
                .map(|m| (&m.id, &m.archive_id, m.at))
                .collect::<Vec<_>>()
        );
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        // Both arrival orders must converge to the same line, including its archive ID.
        for reverse in [false, true] {
            let mut app = App::new(vec![recipient.clone()], Modifier::Ctrl, 26, 30);
            let server = app.server_chat(&recipient);
            app.select(server);
            let mut copies = messages.clone();
            if reverse {
                copies.reverse();
            }
            for message in copies {
                app.on_incoming(&recipient, &message, now());
            }
            let index = app.chats.iter().position(|c| c.jid == *sender.jid()).unwrap();
            assert_eq!(app.chats[index].lines.len(), 1);
            assert_eq!(app.chats[index].unread, 1);
            assert!(app.chats[index].lines[0].archive_id.is_some());
            // Sending the identical URL again is a separate message, never a body dedup.
            let mut another = messages[0].clone();
            another.id = Some(receipts::new_id());
            another.archive_id = Some(receipts::new_id());
            app.on_incoming(&recipient, &another, now());
            assert_eq!(app.chats[index].lines.len(), 2);
            assert_eq!(app.chats[index].unread, 2);
        }
        let mut app = App::new(vec![recipient.clone()], Modifier::Ctrl, 26, 30);
        let server = app.server_chat(&recipient);
        app.select(server);
        for message in messages {
            keep(&shared, &recipient, &message);
            apply(
                &mut app,
                Msg::Incoming {
                    account: recipient.clone(),
                    message,
                    at: now(),
                },
                &shared,
            );
        }
        let chat = app.chats.iter().find(|c| c.jid == *sender.jid()).unwrap();
        assert_eq!(
            chat.lines.len(),
            1,
            "one incoming upload, irrespective of transport copies"
        );
        assert_eq!(chat.unread, 1, "a duplicate must not increase unread");
        assert!(!chat.lines[0].encrypted);
        assert!(chat.lines[0].media.is_some());
        let mut restarted = App::new(vec![recipient.clone()], Modifier::Ctrl, 26, 30);
        load_history(&mut restarted, &store, &[recipient]);
        let chat = restarted.chats.iter().find(|c| c.jid == *sender.jid()).unwrap();
        assert_eq!(chat.lines.len(), 1, "restart must keep the same one message");
    }

    #[tokio::test]
    async fn late_archive_pages_respect_the_other_laptops_read_frontier() {
        use super::*;
        use crate::config::Modifier;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let me: BareJid = "me@example.org".parse().unwrap();
        let room: BareJid = "chat5@muc.example.org".parse().unwrap();
        let message = |at: i64| Incoming {
            peer: room.clone(),
            sender: "bob@example.org".parse().unwrap(),
            nick: Some("bob".into()),
            device: None,
            direction: Direction::Incoming,
            encrypted: false,
            text: Some(format!("message {at}")),
            at: Some(at),
            not_for_this_device: false,
            id: Some(format!("m{at}")),
            markable: false,
            archive_id: Some(format!("a{at}")),
            untrusted: false,
            reply_to: None,
            replaces: None,
            shared_file: false,
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let server = app.server_chat(&me);
        let chat = app.chat(&me, &room, ChatKind::Room { nick: "me".into() });
        app.select(server);
        // The office laptop read through message 100. Its marker arrives before
        // some older archive pages, which are new to this laptop, but already read.
        let boundary = message(100);
        keep(&shared, &me, &boundary);
        apply(
            &mut app,
            Msg::ReadMarks {
                account: me.clone(),
                marks: vec![(room.clone(), "a100".into())],
            },
            &shared,
        );
        for at in (19..100).chain(101..106) {
            let message = message(at);
            keep(&shared, &me, &message);
            apply(
                &mut app,
                Msg::Incoming {
                    account: me.clone(),
                    message,
                    at,
                },
                &shared,
            );
        }
        assert_eq!(
            app.chats[chat].unread, 5,
            "81 already-read archive messages must not turn five new messages into 86"
        );
    }

    #[tokio::test]
    async fn startup_read_marker_for_a_room_created_after_sync() {
        use super::*;
        use crate::config::Modifier;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let me: BareJid = "me@example.org".parse().unwrap();
        let room: BareJid = "chat5@muc.example.org".parse().unwrap();
        let message = |at: i64| Incoming {
            peer: room.clone(),
            sender: "bob@example.org".parse().unwrap(),
            nick: Some("bob".into()),
            device: None,
            direction: Direction::Incoming,
            encrypted: false,
            text: Some(format!("message {at}")),
            at: Some(at),
            not_for_this_device: false,
            id: Some(format!("m{at}")),
            markable: false,
            archive_id: Some(format!("a{at}")),
            untrusted: false,
            reply_to: None,
            replaces: None,
            shared_file: false,
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        let shared = Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let server = app.server_chat(&me);
        app.select(server);
        // A room bookmarked on the other laptop does not exist locally yet.
        assert!(!app.chats.iter().any(|c| c.jid == room));
        apply(
            &mut app,
            Msg::ReadMarks {
                account: me.clone(),
                marks: vec![(room.clone(), "a500".into())],
            },
            &shared,
        );
        let chat = app.chat(&me, &room, ChatKind::Room { nick: "me".into() });
        for at in 401..506 {
            let message = message(at);
            keep(&shared, &me, &message);
            apply(
                &mut app,
                Msg::Incoming {
                    account: me.clone(),
                    message,
                    at,
                },
                &shared,
            );
            assert_eq!(
                app.chats[chat].unread,
                usize::try_from((at - 500).max(0)).unwrap(),
                "after archive message {at}, already-read history must not appear unread"
            );
        }
        assert_eq!(app.chats[chat].unread, 5, "only messages 501–505 are unread");
    }

    #[test]
    fn an_irc_chat_read_locally_stays_read_after_restart_without_live_archive_ids() {
        use crate::app::{App, ChatKind};
        use crate::config::Modifier;
        use omtalk_store::{Store, StoredMessage};
        use omtalk_xmpp::BareJid;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let me: BareJid = "me@example.org".parse().unwrap();
        let peer: BareJid = "#test%irc.example.org@irc.example.org".parse().unwrap();
        let store = Store::open(&path).unwrap();
        store.remember_room(&me, &peer, "me").unwrap();
        for i in 1..=3 {
            store
                .save_message(
                    &me,
                    &StoredMessage {
                        chat: peer.clone(),
                        sender: "bob@example.org".parse().unwrap(),
                        nick: Some("bob".into()),
                        outgoing: false,
                        encrypted: false,
                        body: "hello".into(),
                        at: i,
                        id: Some(format!("m{i}")),
                        archive_id: (i == 1).then(|| "archive-old".into()),
                        delivery: None,
                        shared_file: false,
                        reply_to: None,
                        untrusted: false,
                        edited: false,
                    },
                )
                .unwrap();
        }
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let server = app.server_chat(&me);
        super::load_history(&mut app, &store, std::slice::from_ref(&me));
        let room = app.chat(&me, &peer, ChatKind::Room { nick: "me".into() });
        app.select(server);
        app.read_marks(
            &me,
            super::counted(&store, &me, vec![(peer.clone(), "archive-old".into())]),
        );
        assert_eq!(app.chats[room].unread, 2);
        let delayed = vec![(peer.clone(), "archive-old".into())];
        app.select(room);
        assert_eq!(app.chats[room].unread, 0);
        // CURRENT: the production event loop persists this before waiting again.
        super::flush_local_reads(&mut app, &store);
        app.select(server);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = super::Shared {
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.path().join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: store.clone(),
            tx,
        };
        super::apply(
            &mut app,
            super::Msg::ReadMarks {
                account: me.clone(),
                marks: delayed,
            },
            &shared,
        );
        assert_eq!(
            app.chats[room].unread, 0,
            "a queued background count must not unread a chat read since it was counted"
        );
        drop(shared);
        drop(app);
        drop(store);
        // A fresh process, the same local DB, and an old server mark: the real IRC case.
        let store = Store::open(&path).unwrap();
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let server = app.server_chat(&me);
        super::load_history(&mut app, &store, std::slice::from_ref(&me));
        let room = app.chat(&me, &peer, ChatKind::Room { nick: "me".into() });
        app.select(server);
        super::restore_local_unread(&mut app, &store, &me);
        app.read_marks(
            &me,
            super::counted(&store, &me, vec![(peer.clone(), "archive-old".into())]),
        );
        assert_eq!(
            app.chats[room].unread, 0,
            "messages read here must stay read after restart, including messages with no room archive ID"
        );
        // A marker beyond the history here remains unknown: opening the chat must
        // not publish an older ID over the phone's newer one.
        app.read_marks(
            &me,
            super::counted(&store, &me, vec![(peer.clone(), "not-here-yet".into())]),
        );
        assert_eq!(app.chats[room].unread, 0);
        assert!(!app.chats[room].read_mark_here);
        let opened = app.select(room);
        assert!(!opened.iter().any(|a| matches!(
            a,
            crate::app::Action::Displayed {
                archive_id: Some(_),
                ..
            }
        )));
        app.select(server);
        // A valid newer external mark wins, an older delayed one never moves it back.
        store
            .save_message(
                &me,
                &StoredMessage {
                    chat: peer.clone(),
                    sender: "bob@example.org".parse().unwrap(),
                    nick: Some("bob".into()),
                    outgoing: false,
                    encrypted: false,
                    body: "new".into(),
                    at: 4,
                    id: Some("m4".into()),
                    archive_id: Some("archive-new".into()),
                    delivery: None,
                    shared_file: false,
                    reply_to: None,
                    untrusted: false,
                    edited: false,
                },
            )
            .unwrap();
        super::restore_local_unread(&mut app, &store, &me);
        assert_eq!(app.chats[room].unread, 1);
        app.read_marks(
            &me,
            super::counted(&store, &me, vec![(peer.clone(), "archive-new".into())]),
        );
        assert_eq!(app.chats[room].unread, 0);
        app.read_marks(
            &me,
            super::counted(&store, &me, vec![(peer, "archive-old".into())]),
        );
        assert_eq!(app.chats[room].unread, 0);
    }

    #[test]
    fn chat_mutes_survive_a_restart_and_unmute_removes_the_saved_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let me: omtalk_xmpp::BareJid = "me@example.org".parse().unwrap();
        let peer: omtalk_xmpp::BareJid = "team@muc.example.org".parse().unwrap();
        {
            let store = omtalk_store::Store::open(&path).unwrap();
            super::save_mute(&store, &me, &peer, true).unwrap();
        }
        let store = omtalk_store::Store::open(&path).unwrap();
        let mut app = crate::app::App::new(vec![me.clone()], crate::config::Modifier::Ctrl, 26, 30);
        super::restore_mutes(&store, &mut app);
        let index = app.chat(&me, &peer, crate::app::ChatKind::Room { nick: "me".into() });
        assert!(app.chats[index].muted);
        super::save_mute(&store, &me, &peer, false).unwrap();
        drop(store);
        let store = omtalk_store::Store::open(&path).unwrap();
        let mut app = crate::app::App::new(vec![me], crate::config::Modifier::Ctrl, 26, 30);
        super::restore_mutes(&store, &mut app);
        assert!(app.muted_chats.is_empty());
    }

    #[test]
    fn folded_groups_are_kept_and_read_back() {
        use crate::app::Group;
        let me: omtalk_xmpp::BareJid = "me@example.org".parse().unwrap();
        let folded: std::collections::BTreeSet<Group> = [
            Group::Account(me.clone()),
            Group::Folder(me.clone(), "Рабочие дела".into()),
        ]
        .into();
        assert_eq!(super::folded_from_kept(&super::folded_to_kept(&folded)), folded);
        assert!(super::folded_from_kept("nonsense\nfolder").is_empty());
    }

    #[test]
    fn unreadable_messages_are_counted_in_proper_russian() {
        assert!(super::unreadable_words(1).starts_with("ещё 1 старое сообщение зашифровано"));
        assert!(super::unreadable_words(3).starts_with("ещё 3 старых сообщения зашифрованы"));
        assert!(super::unreadable_words(11).starts_with("ещё 11 старых сообщений"));
        assert!(super::unreadable_words(21).contains("здесь его не прочитать"));
    }
}
