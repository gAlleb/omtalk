//! What the interface shows and how input changes it. No terminal and no network here:
//! keys and mouse turn into changes of this model and into [`Action`]s, which the
//! runtime carries out. That keeps all of it testable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use omtalk_omemo::{Direction, Incoming};
use omtalk_xmpp::chatstates::ChatState;
use omtalk_xmpp::presence::Online;
use omtalk_xmpp::services::CommandAction;
use omtalk_xmpp::{BareJid, Jid};
use ratatui::layout::Rect;

use omtalk_store::Folder;

use crate::complete::{self, Completion};
use crate::config::Modifier;
use crate::services::{Outcome, Services};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatKind {
    Direct,
    Room {
        nick: String,
    },
    /// The account's own channel: what its server says, and what omtalk has to say about
    /// the account (connection, errors, requests). Commands only.
    Server,
}

/// How far one's own message has got.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Delivery {
    /// A file going up to the server, this far (percent).
    Uploading(u8),
    Sending,
    /// No connection now: it waits in the queue and goes when the connection is back.
    Queued,
    /// The server has it (for a room: the room sent it back).
    Sent,
    /// A client of the recipient has it (XEP-0184).
    Delivered,
    /// The recipient has seen it (XEP-0333).
    Read,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Unix time, seconds.
    pub at: i64,
    pub who: String,
    pub text: String,
    pub encrypted: bool,
    pub outgoing: bool,
    /// Said by omtalk itself (an error, a request), not by anyone in the chat.
    pub notice: bool,
    /// Written on a device refused or not decided on.
    pub untrusted: bool,
    /// Corrected since it came (XEP-0308): the text is the corrected one.
    pub edited: bool,
    /// One's own in a room: the room's copy back has come.
    pub echoed: bool,
    /// Reactions to it (XEP-0444): whose (a nick in a room, an address otherwise), and
    /// all of theirs.
    pub reactions: BTreeMap<String, Vec<String>>,
    /// The id of the message this one answers (XEP-0461).
    pub reply_to: Option<String>,
    /// The id the archive gave it (a room's, in a room): replies in rooms name that.
    pub archive_id: Option<String>,
    /// The message's id, as sent: receipts and markers refer to it.
    pub id: Option<String>,
    /// For one's own messages sent from here: how far it has got.
    pub delivery: Option<Delivery>,
    /// A shared file the message is, if it is one.
    pub media: Option<MediaSlot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSlot {
    /// Where the file is; the same for every copy of the message.
    pub url: String,
    /// The link as sent: for `aesgcm://`, it carries the key.
    pub link: String,
    pub name: String,
    pub image: bool,
    pub state: MediaState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaState {
    /// Not fetched by itself (auto_download = false): a click fetches it.
    Offered,
    Loading,
    /// Coming down, this far (percent).
    Downloading(u8),
    Ready(PathBuf),
    Failed(String),
}

impl Line {
    /// A message line, with its media slot filled in if the text is an encrypted file.
    pub fn message(at: i64, who: String, text: String, encrypted: bool, outgoing: bool) -> Self {
        Self::message_sharing(at, who, text, encrypted, outgoing, false)
    }

    /// [`Line::message`] for a message that may share a file (`shared_file`, as its
    /// sender said: XEP-0066); a link written by hand is shown as a link.
    pub fn message_sharing(
        at: i64,
        who: String,
        text: String,
        encrypted: bool,
        outgoing: bool,
        shared_file: bool,
    ) -> Self {
        let media = if shared_file {
            omtalk_media::Media::shared(&text)
        } else {
            omtalk_media::Media::from_text(&text)
        };
        let media = media.map(|media| MediaSlot {
            url: media.url().to_owned(),
            link: text.trim().to_owned(),
            name: media.name(),
            image: media.is_image(),
            state: MediaState::Loading,
        });
        Self {
            at,
            who,
            text,
            encrypted,
            outgoing,
            notice: false,
            untrusted: false,
            edited: false,
            echoed: false,
            reactions: BTreeMap::new(),
            reply_to: None,
            archive_id: None,
            media,
            id: None,
            delivery: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub device: u32,
    /// When it last wrote (Unix seconds), if it ever did here.
    pub seen: Option<i64>,
    pub fingerprint: Option<String>,
    pub has_session: bool,
    pub this_device: bool,
    pub trust: omtalk_omemo::Trust,
}

#[derive(Debug, Clone)]
pub struct Chat {
    pub account: BareJid,
    pub jid: BareJid,
    pub kind: ChatKind,
    pub lines: Vec<Line>,
    pub unread: usize,
    /// Messages typed here go out encrypted. On unless the user turns it off.
    pub omemo: bool,
    /// Rows scrolled up from the newest.
    pub scroll: usize,
    /// Older messages asked for (scrolled to the top), not here yet.
    pub older_loading: bool,
    /// Nothing older to be had: the store and the archive have given all.
    pub older_done: bool,
    /// Where the archive's next older page ends: the oldest of the last page read.
    pub older_before: Option<String>,
    /// Show the chat from its first message at or after this time (a search result).
    pub jump_to: Option<i64>,
    /// Room occupants by nick, with the real account when the room shows it.
    pub occupants: BTreeMap<String, Option<BareJid>>,
    /// Their status where it is not plain online: away, do not disturb…
    pub occupant_status: BTreeMap<String, omtalk_xmpp::presence::OwnStatus>,
    /// A room's members (owners, admins, members), present or not; read when it opens.
    pub members: Vec<BareJid>,
    /// A room's own name, if it has one.
    pub name: Option<String>,
    /// A room's subject (topic), as it last said.
    pub topic: Option<String>,
    /// The peer's devices; `None` until read.
    pub devices: Option<Vec<DeviceRow>>,
    /// The newest message here whose sender asked to learn when it is read, not yet
    /// marked read.
    pub markable: Option<String>,
    /// The archive's id of the newest message here not yet reported read to one's own
    /// other clients.
    pub read_up_to: Option<String>,
}

impl Chat {
    pub fn new(account: BareJid, jid: BareJid, kind: ChatKind) -> Self {
        Self {
            account,
            jid,
            kind,
            lines: Vec::new(),
            unread: 0,
            omemo: true,
            scroll: 0,
            older_loading: false,
            older_done: false,
            older_before: None,
            jump_to: None,
            occupants: BTreeMap::new(),
            occupant_status: BTreeMap::new(),
            members: Vec::new(),
            name: None,
            topic: None,
            devices: None,
            markable: None,
            read_up_to: None,
        }
    }

    pub fn is_room(&self) -> bool {
        matches!(self.kind, ChatKind::Room { .. })
    }

    pub fn title(&self) -> String {
        if self.kind == ChatKind::Server {
            return tr!("server", "сервер").to_owned();
        }
        if let Some(name) = &self.name {
            return name.clone();
        }
        match self.jid.node() {
            Some(node) => node.to_string(),
            None => self.jid.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Chats,
    Messages,
    Input,
    /// A room's participants on the right: picked with the arrows, opened with Enter.
    People,
}

/// Someone in a room's pane: present under a nick, or a member who is away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Person {
    Here(String),
    Away(BareJid),
}

/// How an account's connection stands. Said in words when shown, in the interface's
/// language as it is then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    Online,
    /// Lost, being made again; why.
    Reconnecting(String),
    /// The server refused the password; the account has stopped.
    Refused(String),
}

impl Connection {
    pub fn online(&self) -> bool {
        *self == Connection::Online
    }

    pub fn words(&self) -> String {
        match self {
            Connection::Connecting => tr!("connecting…", "подключение…").into(),
            Connection::Online => tr!("online", "в сети").into(),
            Connection::Reconnecting(why) => trf!(
                "reconnecting ({})",
                "переподключение ({})",
                crate::lang::reason(why)
            ),
            Connection::Refused(why) => trf!("password refused: {why}", "пароль не принят: {why}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Splitter {
    Left,
    Right,
}

/// Where things were drawn last, for the mouse.
/// A heading in the chat list: an account, or one of the user's folders inside it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// A folder inside an account.
    Folder(BareJid, String),
    Account(BareJid),
}

/// One row of the chat list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Heading(Group),
    Chat(usize),
}

/// A button in the services window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceButton {
    Back,
    Reload,
    Close,
    /// A step of the open command.
    Step(CommandAction),
    /// Leave a command's result for the listing.
    Dismiss,
}

#[derive(Debug, Clone, Default)]
pub struct Areas {
    pub screen: Rect,
    /// The open chat's oldest row is on screen: scrolling up asks for older messages.
    pub top_shown: bool,
    /// The "↑ earlier" row above the oldest message: a click asks for older ones.
    pub older: Option<Rect>,
    /// The reactions of the picker open over a message, each where it is drawn.
    pub reaction_buttons: Vec<(Rect, &'static str)>,
    /// The search results on screen, each with its index.
    pub search_rows: Vec<(Rect, usize)>,
    pub left: Option<Rect>,
    pub right: Option<Rect>,
    pub messages: Rect,
    pub input: Rect,
    pub omemo_button: Rect,
    /// The paperclip: send a file.
    pub attach_button: Rect,
    pub hide_left: Option<Rect>,
    pub hide_right: Option<Rect>,
    /// Thin strips that bring a hidden pane back.
    pub show_left: Option<Rect>,
    pub show_right: Option<Rect>,
    pub chat_rows: Vec<(Rect, usize)>,
    /// Folder and account headings: a click folds them, a chat dropped on one moves there.
    pub group_rows: Vec<(Rect, Group)>,
    /// Pictures and file lines, to open with a click.
    pub files: Vec<(Rect, PathBuf)>,
    /// Files not fetched yet, to fetch with a click: their links.
    pub fetches: Vec<(Rect, String)>,
    /// The conversation's rows on screen and their text, for selecting it.
    pub message_rows: Vec<(Rect, String)>,
    /// The same rows and the message (index among the chat's lines) each belongs to.
    pub message_lines: Vec<(Rect, usize)>,
    /// Senders' names in a room and its participants: a click opens a chat with them.
    pub nicks: Vec<(Rect, String)>,
    /// The keys window and what is clickable in it.
    pub key_window: Rect,
    pub key_rows: Vec<(Rect, usize)>,
    pub settings_window: Rect,
    pub settings_rows: Vec<(Rect, usize)>,
    pub key_buttons: Vec<(Rect, crate::keys::KeyButton)>,
    /// People in the pane beside a room who are away: a click opens a chat with them.
    pub people: Vec<(Rect, BareJid)>,
    /// The devices in the pane beside a direct chat: a click opens the keys window.
    pub side_devices: Option<Rect>,
    /// The services window and what is clickable in it.
    pub service_window: Rect,
    pub service_rows: Vec<(Rect, usize)>,
    /// A form's rows: the field, and for a many-choice list the option on that row.
    pub service_fields: Vec<(Rect, (usize, Option<usize>))>,
    pub service_buttons: Vec<(Rect, ServiceButton)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Send {
        account: BareJid,
        to: BareJid,
        room: bool,
        text: String,
        encrypted: bool,
        /// The message's id; its line, already shown as being sent, carries it too.
        id: String,
        /// A reply: what it answers, and how long the quote at the start of `text` is.
        reply: Option<(omtalk_xmpp::replies::ReplyTo, usize)>,
        /// A correction (XEP-0308) of one's own message with this id.
        replace: Option<String>,
    },
    /// The messages of `chat` before what it shows: `have` of them are shown (and kept),
    /// `before` is where the archive's page ends, if known.
    Older {
        account: BareJid,
        chat: BareJid,
        room: bool,
        have: usize,
        before: Option<String>,
    },
    /// One's own status, for every account (`/away`, `/dnd`, `/online`).
    SetStatus(omtalk_xmpp::presence::OwnStatus),
    /// `room`'s bookmark no longer says to enter it by itself, for all the account's
    /// clients.
    NoAutojoin {
        account: BareJid,
        room: BareJid,
    },
    /// OMEMO turned on or off in `chat`, to be kept.
    SetOmemo {
        account: BareJid,
        chat: BareJid,
        on: bool,
    },
    /// The messages of `chat` holding `words`.
    Search {
        account: BareJid,
        chat: BareJid,
        words: String,
    },
    /// `chat` shown from the time `at` on (a search result picked).
    Jump {
        account: BareJid,
        chat: BareJid,
        room: bool,
        at: i64,
    },
    /// One's reactions to the message `id` (XEP-0444), all of them now; as `by` they
    /// are kept.
    React {
        account: BareJid,
        chat: BareJid,
        room: bool,
        id: String,
        by: String,
        reactions: Vec<String>,
    },
    Join {
        account: BareJid,
        room: BareJid,
        /// `None`: the nick the other clients use there (the bookmark), else the
        /// account's name.
        nick: Option<String>,
    },
    Leave {
        account: BareJid,
        room: BareJid,
        nick: String,
    },
    AddContact {
        account: BareJid,
        jid: BareJid,
        name: Option<String>,
    },
    Accept {
        account: BareJid,
        jid: BareJid,
    },
    Refuse {
        account: BareJid,
        jid: BareJid,
    },
    Devices {
        account: BareJid,
        jid: BareJid,
    },
    /// Ask a server what it offers, or run one step of a command.
    Services {
        request: crate::services::Request,
        /// The command's title, for the form that comes back.
        title: String,
    },
    /// Tell chat `to` whether one is typing there (XEP-0085).
    ChatState {
        account: BareJid,
        to: BareJid,
        room: bool,
        state: ChatState,
    },
    /// Chat `to` was read here: tell the sender (`marker`, the message's id, XEP-0333;
    /// not in rooms) and one's own other clients (`archive_id`, XEP-0490).
    Displayed {
        account: BareJid,
        to: BareJid,
        room: bool,
        marker: Option<String>,
        archive_id: Option<String>,
    },
    /// Send the file at `path` to the chat: up to the server's upload service (sealed
    /// first in an encrypted chat), then its link as a message under `id`.
    Upload {
        account: BareJid,
        to: BareJid,
        room: bool,
        path: PathBuf,
        encrypted: bool,
        id: String,
    },
    /// The chat was closed: keep it so, until someone writes in it.
    CloseChat {
        account: BareJid,
        jid: BareJid,
    },
    /// The folders changed; keep them.
    SaveFolders(Vec<Folder>),
    /// Take another nick in a room.
    Nick {
        account: BareJid,
        room: BareJid,
        nick: String,
    },
    /// Read a room's name.
    RoomName {
        account: BareJid,
        room: BareJid,
    },
    /// Set a room's subject.
    SetTopic {
        account: BareJid,
        room: BareJid,
        topic: String,
    },
    /// Name a room (as its owner).
    SetRoomName {
        account: BareJid,
        room: BareJid,
        name: String,
    },
    /// Make a room: members only, real addresses shown (as OMEMO needs), kept; and enter.
    CreateRoom {
        account: BareJid,
        room: BareJid,
        name: Option<String>,
        nick: Option<String>,
    },
    /// Let `jid` into a room (as a member) and invite them.
    Invite {
        account: BareJid,
        room: BareJid,
        jid: BareJid,
    },
    /// Read a direct chat's history from the account's archive: this device has none.
    History {
        account: BareJid,
        peer: BareJid,
    },
    /// Fetch the avatar of a person or a room, unless it is known.
    Avatar {
        account: BareJid,
        jid: BareJid,
        room: bool,
    },
    /// Read who belongs to a room, present or not.
    Members {
        account: BareJid,
        room: BareJid,
    },
    /// The keys window asks something of OMEMO.
    Keys(crate::keys::Request),
    /// Write a setting into the config file (and, for those the runtime keeps, use it).
    Setting {
        section: &'static str,
        key: &'static str,
        value: crate::settings::Written,
    },
    /// Put text into the clipboards (ordinary and primary).
    Copy(String),
    /// Paste what is selected elsewhere on the desktop (primary).
    PastePrimary,
    /// Paste the ordinary clipboard (its text).
    PasteClipboard,
    /// Fetch the shared file at this link, asked for by a click.
    Fetch(String),
    /// Show a fetched file in the outside viewer.
    Open(PathBuf),
    Quit,
}

/// The keys after the leader, as the list shows them; [`App::shortcut`] does them.
/// `~/x` as the home's `x`; `None` for nothing.
pub fn expand_home(path: &str) -> Option<PathBuf> {
    let path = path.trim().trim_matches(['\'', '"']);
    if path.is_empty() {
        return None;
    }
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest)),
        None => Some(PathBuf::from(path)),
    }
}

/// A pasted text that is one existing file, as terminals paste a dropped one: a path,
/// quoted or not, or a file:// address.
fn dropped_file(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    if text.contains('\n') {
        return None;
    }
    let path = match text.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => text.trim_matches(['\'', '"']).replace("\\ ", " "),
    };
    let path = expand_home(&path)?;
    (path.is_absolute() && path.is_file()).then_some(path)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = text
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The part of `text` between display columns `from` and `to`.
fn columns(text: &str, from: usize, to: usize) -> String {
    let mut col = 0;
    let mut out = String::new();
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if col >= from && col < to {
            out.push(c);
        }
        col += w;
    }
    out
}

/// A reply being written: in which chat, to whom, the quote, and what it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Replying {
    pub account: BareJid,
    pub chat: BareJid,
    pub who: String,
    pub quote: String,
    /// `None` when the message has no id to name: the quote goes alone.
    pub to: Option<omtalk_xmpp::replies::ReplyTo>,
}

/// One's own message being corrected (XEP-0308): the input line holds its new text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editing {
    pub account: BareJid,
    pub chat: BareJid,
    /// The id of the message as first sent: corrections always name that one.
    pub id: String,
    /// Its text before, shown above the input line.
    pub was: String,
}

/// The key, what it does in English, in Russian.
pub const LEADER_KEYS: &[(&str, &str, &str)] = &[
    ("o", "OMEMO in this chat on/off", "OMEMO в этом чате вкл/выкл"),
    ("s", "the server's services", "сервисы сервера"),
    ("b", "hide/show the chat list", "список чатов скрыть/показать"),
    ("r", "hide/show the right pane", "правую панель скрыть/показать"),
    (
        "a",
        "reply (to the selected or the last)",
        "ответить (на выделенное или последнее)",
    ),
    (
        "e",
        "OMEMO keys: fingerprints, trust",
        "ключи OMEMO: отпечатки, доверие",
    ),
    ("p", "settings", "настройки"),
    ("c", "this chat's commands", "команды этого чата"),
    (
        "w",
        "close the chat (a room is left)",
        "закрыть чат (комнату — покинуть)",
    ),
    (
        "z",
        "the current account only / all",
        "только текущий аккаунт / все",
    ),
    (
        "m",
        "mouse: omtalk / the terminal (tmux selection)",
        "мышь: omtalk / терминал (выделение tmux)",
    ),
    ("↑ ↓ / k j", "previous / next chat", "предыдущий / следующий чат"),
    ("← →", "narrower / wider chat list", "уже / шире список чатов"),
    ("q", "quit", "выход"),
    ("Esc", "cancel", "отмена"),
];

/// A message `/search` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub at: i64,
    pub who: String,
    pub text: String,
}

/// The results of `/search` in a chat, over the conversation until one is picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Search {
    pub account: BareJid,
    pub chat: BareJid,
    pub words: String,
    /// `None` while the store is looked through.
    pub found: Option<Vec<Found>>,
    pub cursor: usize,
}

/// The reactions a right click on a message offers, as messengers do.
pub const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/// The reaction picker, open over message `line` of chat `chat`, where it was clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picker {
    pub chat: usize,
    pub line: usize,
    pub x: u16,
    pub y: u16,
}

/// What `/react` is given as a reaction: an emoji as it is, or a name for one
/// (":+1:", "heart") as people write them.
pub fn reaction_of(text: &str) -> String {
    let name = text.trim().trim_matches(':').to_lowercase();
    let emoji = match name.as_str() {
        "+1" | "thumbsup" | "like" | "ok" => "👍",
        "-1" | "thumbsdown" => "👎",
        "heart" | "love" | "<3" => "❤️",
        "joy" | "laugh" | "lol" | "rofl" => "😂",
        "wow" | "open_mouth" | "o" => "😮",
        "cry" | "sad" => "😢",
        "pray" | "thanks" | "thx" => "🙏",
        "fire" => "🔥",
        "tada" | "party" => "🎉",
        "eyes" => "👀",
        _ => return text.trim().to_owned(),
    };
    emoji.to_owned()
}

/// How many messages a chat holds once an archive has filled it: the rest stay in the
/// store, for scrolling up.
pub const SHOWN: usize = 300;

/// How soon a second Ctrl+C has to follow the first to quit.
const QUIT_AGAIN: std::time::Duration = std::time::Duration::from_secs(3);

pub struct App {
    pub accounts: Vec<BareJid>,
    /// Account → connection state, as shown in the status line.
    pub connection: BTreeMap<BareJid, Connection>,
    pub chats: Vec<Chat>,
    pub selected: Option<usize>,
    pub focus: Focus,
    pub input: String,
    /// In characters, not bytes.
    pub cursor: usize,
    pub left_width: u16,
    pub right_width: u16,
    pub show_left: bool,
    pub show_right: bool,
    pub modifier: KeyModifiers,
    pub icons: crate::theme::Icons,
    pub status: String,
    pub areas: Areas,
    pub dragging: Option<Splitter>,
    /// A chat being dragged to a folder.
    pub dragging_chat: Option<usize>,
    /// A Tab completion being cycled through in the input line.
    pub completion: Option<Completion>,
    /// The message being answered by what is typed now.
    pub replying: Option<Replying>,
    /// A correction being written, if any.
    pub editing: Option<Editing>,
    /// The last click in the conversation, to tell a double one.
    pub last_click: Option<(std::time::Instant, u16, u16)>,
    /// What Ctrl+W/U/K cut last, for Ctrl+Y.
    pub killed: String,
    /// Whether peers learn that their messages were read here.
    pub read_markers: bool,
    /// Whether chats learn that one is typing.
    pub send_typing: bool,
    /// The terminal window is in front (as the terminal says; without its word, it is).
    pub focused: bool,
    /// Sound, desktop notification and the terminal's bell, each for its messages.
    pub notify: crate::notify::Notify,
    /// The typing state last told, and to which chat.
    pub typing_out: Option<((BareJid, BareJid), ChatState)>,
    /// When the input last changed (Unix seconds): typing pauses after a while.
    pub typed_at: i64,
    /// Who is typing where: (account, chat) → who → since when.
    pub typing_in: BTreeMap<(BareJid, BareJid), BTreeMap<String, i64>>,
    /// The mouse is omtalk's (clicks, dragging, its own selection); off, the terminal
    /// and tmux select and paste as for any program.
    pub mouse: bool,
    /// Text being selected in the conversation: where the drag began and where it is,
    /// as screen cells.
    pub selection: Option<((u16, u16), (u16, u16))>,
    /// The selection is being dragged now.
    pub selecting: bool,
    /// The prefix key; after it, one letter from [`LEADER_KEYS`].
    pub leader: (KeyCode, KeyModifiers),
    /// The prefix was pressed: the next key is a shortcut.
    pub leader_pending: bool,
    /// The services window, over the chat while open.
    pub services: Option<Services>,
    /// The keys window, likewise.
    pub keys: Option<crate::keys::Keys>,
    /// The settings window, likewise.
    pub settings: Option<crate::settings::Settings>,
    /// Avatars are shown (and fetched).
    pub show_avatars: bool,
    /// Whose avatar is ready to draw: jid → its hash (the picture is "avatar:HASH").
    pub avatars: BTreeMap<BareJid, String>,
    /// Avatars drawn here with block characters (a terminal without pictures): the
    /// square picture by its key; with pictures, they are among the view's pictures.
    pub avatar_blocks: BTreeMap<String, image::DynamicImage>,
    /// What was sent from the input line since omtalk started (messages and commands),
    /// oldest first: ↑ and ↓ go through it, as in a shell.
    pub sent: Vec<String>,
    /// Which of `sent` the input line shows now, and the draft it replaced.
    pub recall: Option<(usize, String)>,
    /// The last command was refused or answered with how to use it: the line typed goes
    /// back into the input, to be put right instead of typed again.
    pub refused: bool,
    /// How far the chat list is scrolled, in rows; the view keeps the chat picked in sight.
    pub chats_scroll: usize,
    /// The participant picked in a room's pane, while it has the focus.
    pub people_cursor: usize,
    /// Direct chats whose history was asked of the archive already.
    pub history_asked: BTreeSet<(BareJid, BareJid)>,
    /// Whose avatars were asked for already, by account.
    pub avatars_asked: BTreeSet<(BareJid, BareJid)>,
    /// `[ui]` and `[notify]` as last set: the settings window starts from them.
    pub config_ui: crate::config::Ui,
    pub config_notify: crate::notify::NotifyConfig,
    /// Connected clients by (account, whose), each by resource.
    pub online: BTreeMap<(BareJid, BareJid), BTreeMap<String, Online>>,
    /// This program's own resource on each account.
    pub own_resource: BTreeMap<BareJid, String>,
    /// Headings whose chats are folded away in the list.
    pub collapsed: BTreeSet<Group>,
    /// The user's own folders of chats, in order.
    pub folders: Vec<Folder>,
    /// The account commands act for: the last one picked, by its chat or its heading.
    pub active: Option<BareJid>,
    /// Contacts' names in each account's roster: a chat with one opened later takes it.
    pub names: BTreeMap<(BareJid, BareJid), String>,
    /// Chats where OMEMO was turned on or off by hand: kept, a chat opened takes it.
    pub omemo_choice: BTreeMap<(BareJid, BareJid), bool>,
    /// The names contacts gave themselves (XEP-0172), after the roster's.
    pub nicknames: BTreeMap<(BareJid, BareJid), String>,
    /// One's own status, as set last (`/away`…): plain online at the start.
    pub own_status: omtalk_xmpp::presence::OwnStatus,
    /// The reaction picker, if a right click opened one.
    pub picker: Option<Picker>,
    /// `/search` results, while open.
    pub search: Option<Search>,
    /// When Ctrl+C was pressed once: a second one within [`QUIT_AGAIN`] quits.
    pub quit_asked: Option<std::time::Instant>,
    pub quit: bool,
}

pub const MIN_PANE: u16 = 12;

impl App {
    pub fn new(accounts: Vec<BareJid>, modifier: Modifier, left: u16, right: u16) -> Self {
        let modifier = match modifier {
            Modifier::Ctrl => KeyModifiers::CONTROL,
            Modifier::Alt => KeyModifiers::ALT,
            Modifier::Super => KeyModifiers::SUPER,
            Modifier::None => KeyModifiers::NONE,
        };
        Self {
            connection: accounts
                .iter()
                .map(|a| (a.clone(), Connection::Connecting))
                .collect(),
            accounts,
            chats: Vec::new(),
            selected: None,
            focus: Focus::Input,
            input: String::new(),
            cursor: 0,
            left_width: left,
            right_width: right,
            show_left: true,
            show_right: true,
            modifier,
            icons: crate::theme::nerd(),
            status: String::new(),
            areas: Areas::default(),
            dragging: None,
            dragging_chat: None,
            services: None,
            keys: None,
            settings: None,
            show_avatars: false,
            avatars: BTreeMap::new(),
            avatars_asked: BTreeSet::new(),
            history_asked: BTreeSet::new(),
            people_cursor: 0,
            chats_scroll: 0,
            refused: false,
            sent: Vec::new(),
            recall: None,
            avatar_blocks: BTreeMap::new(),
            config_ui: crate::config::Ui {
                left,
                right,
                ..Default::default()
            },
            config_notify: Default::default(),
            completion: None,
            replying: None,
            editing: None,
            last_click: None,
            killed: String::new(),
            read_markers: true,
            send_typing: true,
            focused: true,
            notify: crate::notify::Notify::default(),
            typing_out: None,
            typed_at: 0,
            typing_in: BTreeMap::new(),
            mouse: true,
            selection: None,
            selecting: false,
            leader: (KeyCode::Char('x'), KeyModifiers::CONTROL),
            leader_pending: false,
            collapsed: BTreeSet::new(),
            online: BTreeMap::new(),
            own_resource: BTreeMap::new(),
            folders: Vec::new(),
            active: None,
            quit: false,
            quit_asked: None,
            picker: None,
            search: None,
            own_status: Default::default(),
            names: BTreeMap::new(),
            nicknames: BTreeMap::new(),
            omemo_choice: BTreeMap::new(),
        }
    }

    pub fn current(&self) -> Option<&Chat> {
        self.selected.and_then(|i| self.chats.get(i))
    }

    fn current_mut(&mut self) -> Option<&mut Chat> {
        self.selected.and_then(|i| self.chats.get_mut(i))
    }

    /// Takes the names contacts have in `account`'s roster for the chats with them, now
    /// and for those opened later. A name taken away leaves the address.
    pub fn contact_names(&mut self, account: &BareJid, names: Vec<(BareJid, Option<String>)>) {
        for (jid, name) in names {
            let name = name.filter(|n| !n.trim().is_empty());
            let key = (account.clone(), jid);
            match name {
                Some(name) => drop(self.names.insert(key.clone(), name)),
                None => drop(self.names.remove(&key)),
            }
            self.rename(&key);
        }
    }

    /// The name `jid` gave themselves (XEP-0172), for when the roster has none.
    pub fn nickname(&mut self, account: &BareJid, jid: &BareJid, nick: Option<String>) {
        let key = (account.clone(), jid.clone());
        match nick {
            Some(nick) => drop(self.nicknames.insert(key.clone(), nick)),
            None => drop(self.nicknames.remove(&key)),
        }
        self.rename(&key);
    }

    /// What a contact is called: the name in the roster, else the one they gave
    /// themselves; neither, and the address shows.
    fn name_of(&self, key: &(BareJid, BareJid)) -> Option<String> {
        self.names.get(key).or(self.nicknames.get(key)).cloned()
    }

    fn rename(&mut self, key: &(BareJid, BareJid)) {
        let name = self.name_of(key);
        for chat in self
            .chats
            .iter_mut()
            .filter(|c| c.account == key.0 && c.jid == key.1 && c.kind == ChatKind::Direct)
        {
            chat.name = name.clone();
        }
    }

    /// The chat with `jid` on `account`, made if needed.
    pub fn chat(&mut self, account: &BareJid, jid: &BareJid, kind: ChatKind) -> usize {
        // The account's own server is its server channel, however it came up.
        let kind = if jid.node().is_none() && jid.domain() == account.domain() {
            ChatKind::Server
        } else {
            kind
        };
        if let Some(index) = self
            .chats
            .iter()
            .position(|c| &c.account == account && &c.jid == jid)
        {
            // A room learns one's nick in it; an empty one (a message or a presence that
            // only says "a room") must not wipe the nick known.
            if let ChatKind::Room { nick } = &kind
                && (!nick.is_empty() || !self.chats[index].is_room())
            {
                self.chats[index].kind = kind;
            }
            return index;
        }
        let mut chat = Chat::new(account.clone(), jid.clone(), kind);
        if let Some(&on) = self.omemo_choice.get(&(account.clone(), jid.clone())) {
            chat.omemo = on;
        }
        if chat.kind == ChatKind::Direct {
            chat.name = self.name_of(&(account.clone(), jid.clone()));
        }
        self.chats.push(chat);
        if self.selected.is_none() {
            self.selected = Some(self.chats.len() - 1);
        }
        self.chats.len() - 1
    }

    /// Takes a chat off the list; the selection stays near where it was.
    pub fn remove_chat(&mut self, index: usize) {
        if index >= self.chats.len() {
            return;
        }
        self.chats.remove(index);
        self.selected = match self.selected {
            _ if self.chats.is_empty() => None,
            Some(s) if s > index => Some(s - 1),
            Some(s) if s == index => Some(index.min(self.chats.len() - 1)),
            other => other,
        };
        self.selection = None;
    }

    /// Closes chat `index`: off the list, its history kept, back when someone writes. A
    /// room is left too.
    pub fn close_chat(&mut self, index: usize) -> Vec<Action> {
        let Some(chat) = self.chats.get(index) else {
            return Vec::new();
        };
        let (account, jid) = (chat.account.clone(), chat.jid.clone());
        let mut actions = vec![Action::CloseChat {
            account: account.clone(),
            jid: jid.clone(),
        }];
        match &chat.kind {
            ChatKind::Server => {
                self.status = tr!(
                    "the server channel does not close",
                    "служебный канал не закрывается"
                )
                .into();
                return Vec::new();
            }
            // The room goes once it is left (Msg::Left).
            ChatKind::Room { nick } => actions.push(Action::Leave {
                account,
                room: jid,
                nick: nick.clone(),
            }),
            ChatKind::Direct => self.remove_chat(index),
        }
        actions
    }

    /// The account's server channel, made if needed.
    pub fn server_chat(&mut self, account: &BareJid) -> usize {
        let server: BareJid = account.domain().as_str().parse().expect("a domain is a JID");
        self.chat(account, &server, ChatKind::Server)
    }

    /// A line in the account's server channel, which also shows for a while below.
    pub fn server_notice(&mut self, account: &BareJid, text: impl Into<String>, at: i64) {
        let text = text.into();
        let index = self.server_chat(account);
        self.notice(Some(index), text.clone(), at);
        self.status = text;
    }

    /// After the input changed: the open chat learns that one is typing, or that one
    /// stopped, once per change of state. Commands are not typing.
    fn after_edit(&mut self) -> Vec<Action> {
        self.typed_at = chrono::Utc::now().timestamp();
        // Some terminals type a dropped file's path in, key by key, instead of pasting
        // it: once the line is just that, it is the same offer.
        if !self.input.starts_with("/upload")
            && let Some(file) = dropped_file(&self.input)
        {
            self.input = format!("/upload {}", file.display());
            self.cursor = self.input.chars().count();
            self.status = tr!("Enter sends this file", "Enter — отправить этот файл").into();
            return Vec::new();
        }
        let Some(chat) = self.current().filter(|c| c.kind != ChatKind::Server) else {
            return Vec::new();
        };
        let key = (chat.account.clone(), chat.jid.clone());
        let room = chat.is_room();
        let typing = !self.input.trim().is_empty()
            && (!self.input.trim_start().starts_with('/') || is_action(self.input.trim_start()));
        let state = if typing {
            ChatState::Composing
        } else {
            ChatState::Active
        };
        let told = self
            .typing_out
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, s)| s.clone());
        // Nothing told yet and nothing typed: nothing to say.
        if !self.send_typing || told.as_ref() == Some(&state) || (told.is_none() && !typing) {
            return Vec::new();
        }
        self.typing_out = Some((key.clone(), state.clone()));
        vec![Action::ChatState {
            account: key.0,
            to: key.1,
            room,
            state,
        }]
    }

    /// The window came to the front or went behind. Back in front, the open chat is read.
    pub fn on_focus(&mut self, focused: bool) -> Vec<Action> {
        self.focused = focused;
        match self.selected {
            Some(index) if focused => {
                self.chats[index].unread = 0;
                self.mark_read(index)
            }
            _ => Vec::new(),
        }
    }

    /// What a message from someone, coming now, calls for. Nothing while one is looking
    /// at its chat, nor for one read from an archive (a room's history on entering, what
    /// came while away); else by kind: private, one's nick named in a room, any.
    pub fn alert_for(&self, account: &BareJid, message: &Incoming) -> crate::notify::Alert {
        let Some(text) = &message.text else {
            return Default::default();
        };
        if message.direction != Direction::Incoming || message.at.is_some() {
            return Default::default();
        }
        let chat = self
            .chats
            .iter()
            .position(|c| &c.account == account && c.jid == message.peer);
        if self.focused && chat.is_some() && chat == self.selected {
            return Default::default();
        }
        let kind = chat.map(|i| &self.chats[i].kind);
        let private = message.nick.is_none() && !matches!(kind, Some(ChatKind::Server));
        let mention = match kind {
            Some(ChatKind::Room { nick }) => crate::notify::mentions(text, nick),
            _ => false,
        };
        self.notify.alert(private, mention)
    }

    /// Who wrote and, in a room, which, as a desktop notification names them: the room
    /// by its name (its address only while it has none).
    pub fn notice_names(&self, account: &BareJid, message: &Incoming) -> (String, Option<String>) {
        let Some(nick) = &message.nick else {
            return (message.sender.to_string(), None);
        };
        let room = self
            .chats
            .iter()
            .find(|c| &c.account == account && c.jid == message.peer)
            .map_or_else(|| message.peer.to_string(), Chat::title);
        (nick.clone(), Some(room))
    }

    /// The avatars a notification of `message` shows, as hashes: the writer's (in a room,
    /// when the room tells who it is) and, from a room, the room's.
    pub fn notice_avatars(&self, account: &BareJid, message: &Incoming) -> (Option<String>, Option<String>) {
        if !self.show_avatars {
            return (None, None);
        }
        let chat = self
            .chats
            .iter()
            .find(|c| &c.account == account && c.jid == message.peer);
        match &message.nick {
            None => (self.avatars.get(&message.sender).cloned(), None),
            Some(nick) => {
                let person = chat
                    .and_then(|c| c.occupants.get(nick).cloned().flatten())
                    .and_then(|real| self.avatars.get(&real).cloned());
                (person, self.avatars.get(&message.peer).cloned())
            }
        }
    }

    /// The avatar of whoever wrote in a room, asked for once while avatars are shown:
    /// for notifications. Only where the room tells who the writer is.
    pub fn ask_writer_avatar(&mut self, account: &BareJid, message: &Incoming) -> Option<Action> {
        let nick = message.nick.as_ref()?;
        let real = self
            .chats
            .iter()
            .find(|c| &c.account == account && c.jid == message.peer)?
            .occupants
            .get(nick)
            .cloned()
            .flatten()?;
        if !self.show_avatars
            || real == *account
            || !self.avatars_asked.insert((account.clone(), real.clone()))
        {
            return None;
        }
        Some(Action::Avatar {
            account: account.clone(),
            jid: real,
            room: false,
        })
    }

    /// Called now and then: typing that stopped for 10 s is "paused", as clients do.
    pub fn typing_tick(&mut self, now: i64) -> Vec<Action> {
        // A typist who went silent long ago (a client that closed mid-word) is let go.
        for typing in self.typing_in.values_mut() {
            typing.retain(|_, at| now - *at < 30);
        }
        self.typing_in.retain(|_, t| !t.is_empty());
        match &self.typing_out {
            Some((key, ChatState::Composing)) if now - self.typed_at >= 10 => {
                let key = key.clone();
                let room = self
                    .chats
                    .iter()
                    .any(|c| c.account == key.0 && c.jid == key.1 && c.is_room());
                self.typing_out = Some((key.clone(), ChatState::Paused));
                vec![Action::ChatState {
                    account: key.0,
                    to: key.1,
                    room,
                    state: ChatState::Paused,
                }]
            }
            _ => Vec::new(),
        }
    }

    /// Someone's typing state in a chat.
    pub fn typing(&mut self, account: &BareJid, chat: &BareJid, who: String, state: ChatState, at: i64) {
        // A room tells one's own state back under one's own nick.
        let own = self
            .chats
            .iter()
            .find(|c| &c.account == account && &c.jid == chat)
            .and_then(|c| match &c.kind {
                ChatKind::Room { nick } => Some(nick.clone()),
                _ => None,
            });
        if own.as_ref() == Some(&who) {
            return;
        }
        let typing = self.typing_in.entry((account.clone(), chat.clone())).or_default();
        if state == ChatState::Composing {
            typing.insert(who, at);
        } else {
            typing.remove(&who);
        }
    }

    /// Who is typing in a chat now; one who said so long ago and went silent is not.
    pub fn typing_now(&self, account: &BareJid, chat: &BareJid, now: i64) -> Vec<String> {
        self.typing_in
            .get(&(account.clone(), chat.clone()))
            .map(|t| {
                t.iter()
                    .filter(|(_, at)| now - **at < 30)
                    .map(|(who, _)| who.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Someone in room `index` (or oneself) now goes by `new`: the same person, the
    /// occupants list and one's own nick follow, and the room is told.
    pub fn rename_occupant(&mut self, index: usize, old: &str, new: &str, at: i64) {
        let chat = &mut self.chats[index];
        let real = chat.occupants.remove(old).flatten();
        chat.occupants.insert(new.to_owned(), real);
        if let Some(status) = chat.occupant_status.remove(old) {
            chat.occupant_status.insert(new.to_owned(), status);
        }
        let own = matches!(&chat.kind, ChatKind::Room { nick } if nick == old);
        if own {
            chat.kind = ChatKind::Room { nick: new.to_owned() };
        }
        let text = if own {
            trf!("you are now {new}", "теперь вы {new}")
        } else {
            trf!("{old} is now {new}", "{old} теперь {new}")
        };
        self.notice(Some(index), text, at);
    }

    /// A room's subject came: on entering (said quietly), or changed by `by`.
    pub fn topic(&mut self, account: &BareJid, room: &BareJid, by: Option<String>, topic: String, at: i64) {
        let Some(index) = self
            .chats
            .iter()
            .position(|c| &c.account == account && &c.jid == room)
        else {
            return;
        };
        let topic = topic.trim().to_owned();
        let before = self.chats[index].topic.clone();
        self.chats[index].topic = (!topic.is_empty()).then(|| topic.clone());
        // Telling it on entering is not news; a change by someone is.
        if let Some(by) = by.filter(|_| before.is_some() || !topic.is_empty())
            && before.as_deref() != Some(topic.as_str())
        {
            let text = if topic.is_empty() {
                trf!("{by} removed the topic", "{by} убрал(а) тему")
            } else {
                trf!("{by} changed the topic: {topic}", "{by} сменил(а) тему: {topic}")
            };
            self.notice(Some(index), text, at);
        }
    }

    /// A line in the account's server channel only: news, not a problem.
    pub fn server_log(&mut self, account: &BareJid, text: impl Into<String>, at: i64) {
        let index = self.server_chat(account);
        self.notice(Some(index), text, at);
    }

    /// A client of someone (or of oneself) came, went or changed its state. One's own
    /// clients coming and going are noted in the server channel.
    pub fn presence(&mut self, account: &BareJid, online: Online, at: i64) {
        let key = (account.clone(), online.account.clone());
        let clients = self.online.entry(key).or_default();
        let known = clients.contains_key(&online.resource);
        let line = match &online.state {
            Some(_) if !known => Some(trf!(
                "{} ({}) connected",
                "{} ({}) подключился",
                online.client,
                online.resource
            )),
            None if known => Some(trf!(
                "{} ({}) disconnected",
                "{} ({}) отключился",
                online.client,
                online.resource
            )),
            _ => None,
        };
        match online.state {
            Some(_) => drop(clients.insert(online.resource.clone(), online.clone())),
            None => drop(clients.remove(&online.resource)),
        }
        let this = self.own_resource.get(account) == Some(&online.resource);
        if &online.account == account
            && !this
            && let Some(line) = line
        {
            self.server_log(account, trf!("your {line}", "ваш {line}"), at);
        }
    }

    /// A room's members who are not in it now.
    pub fn away_members(&self, index: usize) -> Vec<BareJid> {
        let chat = &self.chats[index];
        let present: Vec<&BareJid> = chat.occupants.values().flatten().collect();
        chat.members
            .iter()
            .filter(|m| **m != chat.account && !present.contains(m))
            .cloned()
            .collect()
    }

    /// Whether the account is connected now.
    pub fn connected(&self, account: &BareJid) -> bool {
        self.connection.get(account).is_some_and(Connection::online)
    }

    /// `who`'s connected clients, the most available first. Offline, nobody is known
    /// to be online: what was heard before may be stale.
    pub fn clients(&self, account: &BareJid, who: &BareJid) -> Vec<&Online> {
        if !self.connected(account) {
            return Vec::new();
        }
        let mut clients: Vec<&Online> = self
            .online
            .get(&(account.clone(), who.clone()))
            .map(|c| c.values().collect())
            .unwrap_or_default();
        clients.sort_by_key(|c| (c.state, c.client.clone()));
        clients
    }

    /// One's own message, sent from here: shown at once, as being sent. `id` is the
    /// message's, for receipts.
    pub fn sending(&mut self, index: usize, text: &str, encrypted: bool, id: &str, at: i64) {
        let mut line = Line::message(at, tr!("me", "я").into(), text.to_owned(), encrypted, true);
        line.id = Some(id.to_owned());
        line.delivery = Some(Delivery::Sending);
        self.push(index, line);
    }

    /// How far one's message `id` got. A step back is ignored (a late receipt after the
    /// read marker), a failure always shows.
    pub fn delivery(&mut self, account: &BareJid, id: &str, state: Delivery) {
        self.delivery_in(account, None, id, state);
    }

    /// [`App::delivery`] said by `from` over the network: only about a message in the
    /// chat with `from` (or with its server, for a server's refusal). Anyone who knows a
    /// message's id (in a room, everyone) could otherwise mark it read, or failed. The
    /// chat it was about, if any.
    pub fn delivery_from(
        &mut self,
        account: &BareJid,
        from: &BareJid,
        id: &str,
        state: Delivery,
    ) -> Option<BareJid> {
        self.delivery_in(account, Some(from), id, state)
    }

    fn delivery_in(
        &mut self,
        account: &BareJid,
        from: Option<&BareJid>,
        id: &str,
        state: Delivery,
    ) -> Option<BareJid> {
        let about = |chat: &Chat| match from {
            None => true,
            Some(from) if from.node().is_none() => chat.jid.domain() == from.domain() || chat.jid == *from,
            Some(from) => chat.jid == *from,
        };
        let mut found = None;
        for chat in self
            .chats
            .iter_mut()
            .filter(|c| &c.account == account && about(c))
        {
            if chat
                .lines
                .iter()
                .any(|l| l.outgoing && l.id.as_deref() == Some(id))
            {
                found = Some(chat.jid.clone());
            }
            for line in chat
                .lines
                .iter_mut()
                .rev()
                .filter(|l| l.outgoing && l.id.as_deref() == Some(id))
            {
                let later = match (&line.delivery, &state) {
                    (_, Delivery::Failed(_)) => true,
                    (Some(Delivery::Failed(_)), _) => false,
                    (Some(now), next) => next > now,
                    (None, _) => true,
                };
                if later {
                    line.delivery = Some(state.clone());
                }
            }
        }
        found
    }

    /// What is owed now that chat `index` is in sight: the sender's read marker (if
    /// those are on), and one's own other clients' note of it, which is always kept.
    fn mark_read(&mut self, index: usize) -> Vec<Action> {
        // Not in front, not read: it waits until the window is back.
        if !self.focused {
            return Vec::new();
        }
        let chat = &mut self.chats[index];
        let marker = chat
            .markable
            .take()
            .filter(|_| self.read_markers && !chat.is_room());
        let archive_id = chat.read_up_to.take();
        if marker.is_none() && archive_id.is_none() {
            return Vec::new();
        }
        vec![Action::Displayed {
            account: chat.account.clone(),
            to: chat.jid.clone(),
            room: chat.is_room(),
            marker,
            archive_id,
        }]
    }

    /// Another of one's clients read `chat`: nothing there is unread here either.
    pub fn read_elsewhere(&mut self, account: &BareJid, chat: &BareJid) {
        if let Some(c) = self
            .chats
            .iter_mut()
            .find(|c| &c.account == account && &c.jid == chat)
        {
            c.unread = 0;
            c.markable = None;
            c.read_up_to = None;
        }
    }

    pub fn select(&mut self, index: usize) -> Vec<Action> {
        if index >= self.chats.len() {
            return Vec::new();
        }
        let mut actions = Vec::new();
        // Leaving a chat mid-word: it learns the typing paused.
        if let Some(((account, to), ChatState::Composing)) = self.typing_out.clone()
            && (self.chats[index].account != account || self.chats[index].jid != to)
        {
            let room = self
                .chats
                .iter()
                .any(|c| c.account == account && c.jid == to && c.is_room());
            self.typing_out = None;
            actions.push(Action::ChatState {
                account,
                to,
                room,
                state: ChatState::Paused,
            });
        }
        self.selected = Some(index);
        self.active = Some(self.chats[index].account.clone());
        actions.extend(self.mark_read(index));
        actions.extend(self.ask_avatar(index));
        actions.extend(self.ask_history(index));
        let chat = &mut self.chats[index];
        chat.unread = 0;
        // A room's pane lists its members who are away: read afresh on opening.
        if chat.is_room() {
            actions.push(Action::Members {
                account: chat.account.clone(),
                room: chat.jid.clone(),
            });
            if chat.name.is_none() {
                actions.push(Action::RoomName {
                    account: chat.account.clone(),
                    room: chat.jid.clone(),
                });
            }
        }
        // A direct chat's right pane lists the peer's devices; read them afresh each time
        // it is opened, since devices come and go.
        if chat.kind == ChatKind::Direct {
            actions.push(Action::Devices {
                account: chat.account.clone(),
                jid: chat.jid.clone(),
            });
        }
        actions
    }

    pub fn push(&mut self, index: usize, line: Line) {
        // In the open chat but with the window behind, it is still unread.
        let seen = self.selected == Some(index) && self.focused;
        let chat = &mut self.chats[index];
        if !seen && !line.notice && !line.outgoing {
            chat.unread += 1;
        }
        chat.lines.push(line);
    }

    /// A message read from an archive: in its place by time (live ones may have come
    /// first), and the chat cut to its newest [`SHOWN`]: a long absence is kept whole in
    /// the store and comes back from there on scrolling up, not all at once.
    fn push_archived(&mut self, index: usize, line: Line) {
        let seen = self.selected == Some(index) && self.focused;
        let chat = &mut self.chats[index];
        if !seen && !line.notice && !line.outgoing {
            chat.unread += 1;
        }
        let at = chat
            .lines
            .iter()
            .rposition(|l| l.at <= line.at)
            .map_or(0, |i| i + 1);
        chat.lines.insert(at, line);
        // Not while one reads further up: the lines there would go from under one.
        if chat.scroll == 0 && chat.lines.len() > SHOWN + SHOWN / 2 {
            let cut = chat.lines.len() - SHOWN;
            chat.lines.drain(..cut);
            chat.older_done = false;
        }
    }

    /// A message arrived (or one's own came back): shown, and its read marker owed. What
    /// to do about it comes back: the marker, while the chat is in sight.
    pub fn on_incoming(&mut self, account: &BareJid, message: &Incoming, at: i64) -> Vec<Action> {
        if message.not_for_this_device {
            self.not_for_this_device(account, message, at);
            return Vec::new();
        }
        let Some(text) = &message.text else {
            return Vec::new();
        };
        let kind = match &message.nick {
            Some(_) => ChatKind::Room { nick: String::new() },
            None => ChatKind::Direct,
        };
        let index = self.chat(account, &message.peer, kind);
        let who = message
            .nick
            .clone()
            .unwrap_or_else(|| message.sender.node().map(|n| n.to_string()).unwrap_or_default());
        if let Some(typing) = self.typing_in.get_mut(&(account.clone(), message.peer.clone())) {
            typing.remove(&who);
        }
        // A room sends one's message back: the line sent from here is that message,
        // which has now arrived.
        if message.direction == Direction::Outgoing
            && let Some(id) = &message.id
            && let Some(line) = self.chats[index]
                .lines
                .iter_mut()
                .find(|l| l.outgoing && l.id.as_ref() == Some(id))
        {
            line.echoed = true;
            self.delivery(account, &id.clone(), Delivery::Sent);
            return Vec::new();
        }
        // A gateway (biboumi) sends it back under an id of its own: the same text, sent
        // from here a moment ago and not back yet, is that message.
        if message.direction == Direction::Outgoing
            && message.nick.is_some()
            && message.at.is_none()
            && let Some(line) = self.chats[index].lines.iter_mut().rev().take(30).find(|l| {
                l.outgoing && !l.echoed && &l.text == text && at - l.at < 120 && l.delivery.is_some()
            })
        {
            line.echoed = true;
            if matches!(line.delivery, Some(Delivery::Sending | Delivery::Queued)) {
                line.delivery = Some(Delivery::Sent);
            }
            return Vec::new();
        }
        let who = match (&message.nick, message.direction) {
            (Some(nick), _) => nick.clone(),
            (None, Direction::Outgoing) => tr!("me", "я").to_owned(),
            (None, Direction::Incoming) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
        };
        let mut line = Line::message_sharing(
            at,
            who,
            text.clone(),
            message.encrypted,
            message.direction == Direction::Outgoing,
            message.shared_file,
        );
        line.untrusted = message.untrusted;
        line.reply_to = message.reply_to.clone();
        line.archive_id = message.archive_id.clone();
        if message.direction == Direction::Incoming {
            line.id = message.id.clone();
        }
        if message.at.is_some() {
            self.push_archived(index, line);
        } else {
            self.push(index, line);
        }
        if message.direction == Direction::Incoming {
            let chat = &mut self.chats[index];
            if message.markable {
                chat.markable = message.id.clone();
            }
            if message.archive_id.is_some() {
                chat.read_up_to = message.archive_id.clone();
            }
            if self.selected == Some(index) {
                return self.mark_read(index);
            }
        }
        Vec::new()
    }

    /// Someone wrote, encrypted, but not for this device: said once in the chat, not for
    /// every such message.
    fn not_for_this_device(&mut self, account: &BareJid, message: &Incoming, at: i64) {
        let kind = match &message.nick {
            Some(_) => ChatKind::Room { nick: String::new() },
            None => ChatKind::Direct,
        };
        let index = self.chat(account, &message.peer, kind);
        let who = match (&message.nick, message.direction) {
            (Some(nick), _) => nick.clone(),
            (None, Direction::Outgoing) => tr!("your other device", "ваше другое устройство").to_owned(),
            (None, Direction::Incoming) => message.sender.to_string(),
        };
        let text = trf!(
            "{who}: a message not for this device: their client does not trust omtalk",
            "{who}: сообщение не для этого устройства — его клиент не доверяет omtalk"
        );
        let said = self.chats[index]
            .lines
            .iter()
            .rev()
            .take(20)
            .any(|l| l.notice && l.text == text);
        if !said {
            self.notice(Some(index), text, at);
        }
    }

    /// The fetch of a shared file ended; every line showing that link learns how.
    pub fn media_done(&mut self, url: &str, state: MediaState) {
        for line in self.chats.iter_mut().flat_map(|c| c.lines.iter_mut()) {
            if let Some(slot) = line.media.as_mut().filter(|s| s.url == url) {
                slot.state = state.clone();
            }
        }
    }
    /// A line from omtalk itself, in a chat or (without one) in the status line.
    pub fn notice(&mut self, chat: Option<usize>, text: impl Into<String>, at: i64) {
        let text = text.into();
        match chat {
            Some(index) if index < self.chats.len() => self.push(
                index,
                Line {
                    at,
                    who: String::new(),
                    text,
                    encrypted: true,
                    outgoing: false,
                    notice: true,
                    untrusted: false,
                    edited: false,
                    echoed: false,
                    reactions: BTreeMap::new(),
                    reply_to: None,
                    archive_id: None,
                    media: None,
                    id: None,
                    delivery: None,
                },
            ),
            _ => self.status = text,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let before = self.input.clone();
        let mut actions = self.handle_key(key);
        if self.input != before {
            actions.extend(self.after_edit());
        }
        actions
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        // The search results take the arrows, Enter and Esc; another key closes them
        // and goes on as it would.
        if let Some(search) = self.search.as_mut() {
            let count = search.found.as_ref().map_or(0, Vec::len);
            match key.code {
                KeyCode::Up => {
                    search.cursor = search.cursor.saturating_sub(1);
                    return Vec::new();
                }
                KeyCode::Down => {
                    search.cursor = (search.cursor + 1).min(count.saturating_sub(1));
                    return Vec::new();
                }
                KeyCode::Enter => {
                    let cursor = search.cursor;
                    return self.jump(cursor);
                }
                KeyCode::Esc => {
                    self.search = None;
                    return Vec::new();
                }
                _ => self.search = None,
            }
        }
        // A key closes the reaction picker; Esc does only that.
        if self.picker.take().is_some() && key.code == KeyCode::Esc {
            return Vec::new();
        }
        // Ctrl+C leaves, whatever the modifier, as in any terminal program; but only
        // pressed twice, a reflex in a chat costs nothing.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            let now = std::time::Instant::now();
            if self
                .quit_asked
                .is_some_and(|at| now.duration_since(at) < QUIT_AGAIN)
            {
                self.quit = true;
                return vec![Action::Quit];
            }
            self.quit_asked = Some(now);
            self.status = tr!("Ctrl+C again to quit", "ещё раз Ctrl+C — выход").into();
            return Vec::new();
        }
        if let Some(services) = self.services.as_mut() {
            let outcome = services.on_key(key);
            return self.services_outcome(outcome);
        }
        if let Some(keys) = self.keys.as_mut() {
            let outcome = keys.on_key(key);
            return self.keys_outcome(outcome);
        }
        if let Some(settings) = self.settings.as_mut() {
            let outcome = settings.on_key(key);
            return self.settings_outcome(outcome);
        }
        // The prefix, then one key: as tmux, and free of what the input line needs.
        if self.leader_pending {
            self.leader_pending = false;
            return match key.code {
                KeyCode::Esc => Vec::new(),
                code => self.shortcut(code),
            };
        }
        if (key.code, key.modifiers) == self.leader {
            self.leader_pending = true;
            return Vec::new();
        }
        if self.focus == Focus::Input
            && key.code == KeyCode::Enter
            && key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)
        {
            return self.on_paste("\n");
        }
        if self.focus == Focus::Input
            && key.code == KeyCode::Char('v')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return vec![Action::PasteClipboard];
        }
        if self.focus == Focus::Input && self.edit_key(key) {
            return Vec::new();
        }
        if !self.modifier.is_empty() && key.modifiers.contains(self.modifier) {
            return self.shortcut(key.code);
        }
        // Typing away from the input line is typing into it: any printable key takes the
        // focus to the line and goes there. The panes move with the arrows, Home and End.
        if let KeyCode::Char(c) = key.code
            && self.focus != Focus::Input
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            && !c.is_control()
        {
            self.focus = Focus::Input;
            return self.input_key(key.code);
        }
        match key.code {
            // In a line being typed, Tab completes; elsewhere it moves the focus.
            KeyCode::Tab | KeyCode::BackTab if self.focus == Focus::Input && !self.input.is_empty() => {
                self.complete(key.code == KeyCode::Tab);
                Vec::new()
            }
            // A room's participants take their turn too, when the pane shows them.
            KeyCode::Tab => {
                let people = self.people_pane();
                self.focus = match self.focus {
                    Focus::Chats => Focus::Messages,
                    Focus::Messages => Focus::Input,
                    Focus::Input if people => Focus::People,
                    Focus::Input | Focus::People => Focus::Chats,
                };
                Vec::new()
            }
            KeyCode::BackTab => {
                let people = self.people_pane();
                self.focus = match self.focus {
                    Focus::Chats if people => Focus::People,
                    Focus::Chats | Focus::People => Focus::Input,
                    Focus::Messages => Focus::Chats,
                    Focus::Input => Focus::Messages,
                };
                Vec::new()
            }
            KeyCode::PageUp => self.scroll(10),
            KeyCode::PageDown => self.scroll(-10),
            _ => match self.focus {
                Focus::Chats => self.chats_key(key.code),
                Focus::Messages => self.messages_key(key.code),
                Focus::Input => self.input_key(key.code),
                Focus::People => self.people_key(key.code),
            },
        }
    }

    fn shortcut(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Char('q') => {
                self.quit = true;
                vec![Action::Quit]
            }
            KeyCode::Char('o') => self.toggle_omemo(),
            KeyCode::Char('s') => self.open_services(None),
            KeyCode::Char('e') => self.open_keys(None),
            KeyCode::Char('p') => self.open_settings(),
            KeyCode::Char('c') => self.open_chat_commands(),
            KeyCode::Char('a') => self.reply_here(),
            KeyCode::Char('w') => match self.selected {
                Some(index) => self.close_chat(index),
                None => Vec::new(),
            },
            KeyCode::Char('z') => {
                self.focus_group();
                Vec::new()
            }
            KeyCode::Char('m') => {
                self.mouse = !self.mouse;
                self.status = if self.mouse {
                    tr!("the mouse is omtalk's", "мышь у omtalk").into()
                } else {
                    tr!(
                        "the mouse is the terminal's (prefix+m takes it back)",
                        "мышь у терминала (префикс+m — вернуть)"
                    )
                    .into()
                };
                Vec::new()
            }
            KeyCode::Char('b') => {
                self.show_left = !self.show_left;
                Vec::new()
            }
            KeyCode::Char('r') => {
                self.show_right = !self.show_right;
                Vec::new()
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Left => {
                self.left_width = self.left_width.saturating_sub(2).max(MIN_PANE);
                Vec::new()
            }
            KeyCode::Right => {
                self.left_width = (self.left_width + 2).min(self.max_pane());
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Letters are typed, never taken here (see [`App::on_key`]): the list moves with the
    /// arrows, ← folds.
    fn chats_key(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => {
                self.focus = Focus::Input;
                Vec::new()
            }
            KeyCode::Left => {
                if let Some(group) = self.selected.map(|i| self.group_of(i)) {
                    self.toggle_group(&group);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Who the current room's pane lists, in its order: those here, then members away.
    pub fn room_people(&self) -> Vec<Person> {
        let Some(index) = self.selected.filter(|&i| self.chats[i].is_room()) else {
            return Vec::new();
        };
        let here = self.chats[index].occupants.keys().cloned().map(Person::Here);
        here.chain(self.away_members(index).into_iter().map(Person::Away))
            .collect()
    }

    /// Whether the right pane shows a room's participants to pick from.
    fn people_pane(&self) -> bool {
        self.show_right && !self.room_people().is_empty()
    }

    fn people_key(&mut self, code: KeyCode) -> Vec<Action> {
        let people = self.room_people();
        if people.is_empty() {
            self.focus = Focus::Input;
            return Vec::new();
        }
        let last = people.len() - 1;
        self.people_cursor = self.people_cursor.min(last);
        match code {
            KeyCode::Up => self.people_cursor = self.people_cursor.saturating_sub(1),
            KeyCode::Down => self.people_cursor = (self.people_cursor + 1).min(last),
            KeyCode::Home => self.people_cursor = 0,
            KeyCode::End => self.people_cursor = last,
            KeyCode::Esc => self.focus = Focus::Input,
            // As a click on them: a direct chat.
            KeyCode::Enter => {
                return match people[self.people_cursor].clone() {
                    Person::Here(nick) => {
                        self.focus = Focus::Input;
                        self.open_private(&nick)
                    }
                    Person::Away(member) => {
                        let account = self.chats[self.selected.expect("a room")].account.clone();
                        let index = self.chat(&account, &member, ChatKind::Direct);
                        self.focus = Focus::Input;
                        self.select(index)
                    }
                };
            }
            _ => {}
        }
        Vec::new()
    }

    fn messages_key(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Up => self.scroll(1),
            KeyCode::Down => self.scroll(-1),
            KeyCode::Home => self.scroll(isize::MAX / 2),
            KeyCode::End => self.scroll(isize::MIN / 2),
            KeyCode::Enter => {
                self.focus = Focus::Input;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Puts one's own last message of the open chat into the input line, to correct it
    /// (`/edit`). Only a text written here or by one's other clients, with an id to name.
    pub fn edit_last(&mut self) {
        let Some(index) = self.selected else { return };
        let chat = &self.chats[index];
        let last = chat
            .lines
            .iter()
            .rev()
            .find(|l| l.outgoing && !l.notice && l.media.is_none() && l.id.is_some());
        let Some(line) = last else {
            self.status = tr!("nothing of yours to correct here", "здесь нечего исправить").into();
            return;
        };
        self.editing = Some(Editing {
            account: chat.account.clone(),
            chat: chat.jid.clone(),
            id: line.id.clone().unwrap_or_default(),
            was: line.text.clone(),
        });
        self.replying = None;
        self.input = line.text.clone();
        self.cursor = self.input.chars().count();
        self.focus = Focus::Input;
    }

    /// A correction come in (XEP-0308): the text of the message it names, by the same
    /// writer, becomes its text. Whether there was one to correct; if not, the
    /// correction shows as a message.
    pub fn correct(&mut self, account: &BareJid, message: &Incoming) -> bool {
        let (Some(id), Some(text)) = (&message.replaces, &message.text) else {
            return false;
        };
        let outgoing = message.direction == omtalk_omemo::Direction::Outgoing;
        let Some(chat) = self
            .chats
            .iter_mut()
            .find(|c| &c.account == account && c.jid == message.peer)
        else {
            return false;
        };
        let room = chat.is_room();
        let line = chat.lines.iter_mut().rev().find(|l| {
            l.id.as_deref() == Some(id.as_str())
                && l.outgoing == outgoing
                && !l.notice
                // In a room, by the same nick as the message corrected.
                && (!room || outgoing || message.nick.as_deref() == Some(l.who.as_str()))
        });
        match line {
            Some(line) => {
                line.text = text.clone();
                line.edited = true;
                true
            }
            None => false,
        }
    }

    /// Reacts with `reaction` to the message under the selection, else to the last one
    /// from someone (`/react 👍`); again, and it is taken back.
    pub fn react_here(&mut self, reaction: &str) -> Vec<Action> {
        let reaction = reaction_of(reaction);
        if reaction.is_empty() || reaction.chars().count() > 8 {
            return self.say(tr!(
                "as: /react 👍 (or :+1:, :heart:, :joy:…); a right click on a message offers some",
                "так: /react 👍 (или :+1:, :heart:, :joy:…); правый клик по сообщению предлагает готовые"
            ));
        }
        let Some(index) = self.selected else {
            return Vec::new();
        };
        let under = self.selection.and_then(|(a, _)| {
            self.areas
                .message_lines
                .iter()
                .find(|(r, _)| r.y == a.1)
                .map(|&(_, line)| line)
        });
        let line = under.or_else(|| {
            self.chats[index]
                .lines
                .iter()
                .rposition(|l| !l.notice && !l.outgoing)
        });
        match line {
            Some(line) => self.react_line(index, line, &reaction),
            None => self.say(tr!("nothing to react to", "не на что реагировать")),
        }
    }

    /// Reacts with `reaction` to message `line` of chat `index`; again, and it is taken
    /// back.
    pub fn react_line(&mut self, index: usize, line: usize, reaction: &str) -> Vec<Action> {
        let Some(chat) = self.chats.get_mut(index) else {
            return Vec::new();
        };
        let line = Some(line);
        let room = chat.is_room();
        let by = match &chat.kind {
            ChatKind::Room { nick } => nick.clone(),
            _ => chat.account.to_string(),
        };
        let Some(line) = line.and_then(|i| chat.lines.get_mut(i)) else {
            return self.say(tr!("nothing to react to", "не на что реагировать"));
        };
        // A room names the message by its own id for it, a chat by the sender's.
        let id = if room {
            line.archive_id.clone()
        } else {
            line.id.clone()
        };
        let Some(id) = id else {
            return self.say(tr!(
                "this message cannot be named to react to",
                "на это сообщение не отреагировать: у него нет номера"
            ));
        };
        let mine = line.reactions.entry(by.clone()).or_default();
        match mine.iter().position(|r| r == reaction) {
            Some(at) => drop(mine.remove(at)),
            None => mine.push(reaction.to_owned()),
        }
        let reactions = mine.clone();
        if reactions.is_empty() {
            line.reactions.remove(&by);
        }
        self.selection = None;
        vec![Action::React {
            account: chat.account.clone(),
            chat: chat.jid.clone(),
            room,
            id,
            by,
            reactions,
        }]
    }

    /// Reactions come in: `by`'s, to the message `id` of the chat with `jid`. Whether the
    /// message is here.
    pub fn reacted(&mut self, account: &BareJid, reactions: &omtalk_xmpp::reactions::Reactions) -> bool {
        let Some(chat) = self
            .chats
            .iter_mut()
            .find(|c| &c.account == account && c.jid == reactions.chat && c.is_room() == reactions.room)
        else {
            return false;
        };
        let room = chat.is_room();
        let line = chat.lines.iter_mut().rev().find(|l| {
            let id = if room { &l.archive_id } else { &l.id };
            !l.notice && id.as_deref() == Some(reactions.id.as_str())
        });
        let Some(line) = line else { return false };
        if reactions.reactions.is_empty() {
            line.reactions.remove(&reactions.by);
        } else {
            line.reactions
                .insert(reactions.by.clone(), reactions.reactions.clone());
        }
        true
    }

    /// Starts a reply: to the message under the selection (quoting what is selected),
    /// else to the last message from someone in the open chat.
    pub fn reply_here(&mut self) -> Vec<Action> {
        let Some(chat) = self.selected else {
            return Vec::new();
        };
        let selected = self.selected_text();
        let under = self.selection.and_then(|(a, _)| {
            self.areas
                .message_lines
                .iter()
                .find(|(r, _)| r.y == a.1)
                .map(|&(_, line)| line)
        });
        let line = under.or_else(|| {
            self.chats[chat]
                .lines
                .iter()
                .rposition(|l| !l.notice && !l.outgoing)
        });
        match line {
            Some(line) => self.reply_to(chat, line, (!selected.trim().is_empty()).then_some(selected)),
            None => self.say(tr!("nothing to reply to", "не на что отвечать")),
        }
    }

    /// Starts a reply to message `line` of chat `chat`, quoting `quote` or all of it.
    pub fn reply_to(&mut self, chat: usize, line: usize, quote: Option<String>) -> Vec<Action> {
        let Some(original) = self.chats.get(chat).and_then(|c| c.lines.get(line)) else {
            return Vec::new();
        };
        let chat_ref = &self.chats[chat];
        // Its own quote is not quoted again.
        let quote = quote.unwrap_or_else(|| {
            let (_, said) = crate::view::split_quote_pub(&original.text);
            said.to_owned()
        });
        let quote: String = quote.lines().take(4).collect::<Vec<_>>().join("\n");
        // A room names the message by the room's archive id and the occupant who wrote
        // it; a chat by the sender's own id.
        let id = if chat_ref.is_room() {
            original.archive_id.clone().or(original.id.clone())
        } else {
            original.id.clone()
        };
        let to = id.map(|id| omtalk_xmpp::replies::ReplyTo {
            id,
            to: match &chat_ref.kind {
                ChatKind::Room { .. } => format!("{}/{}", chat_ref.jid, original.who).parse().ok(),
                _ => None,
            },
        });
        self.replying = Some(Replying {
            account: chat_ref.account.clone(),
            chat: chat_ref.jid.clone(),
            who: original.who.clone(),
            quote,
            to,
        });
        self.selection = None;
        self.focus = Focus::Input;
        Vec::new()
    }

    fn input_key(&mut self, code: KeyCode) -> Vec<Action> {
        // Any other key takes the completion as it stands.
        self.completion = None;
        if matches!(code, KeyCode::Up | KeyCode::Down) {
            self.recall_key(code == KeyCode::Up);
            return Vec::new();
        }
        // Edited, a line from the history is the draft now.
        if !matches!(
            code,
            KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
        ) {
            self.recall = None;
        }
        let len = self.input.chars().count();
        match code {
            KeyCode::Char(c) => {
                let at = self.byte_at(self.cursor);
                self.input.insert(at, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let at = self.byte_at(self.cursor);
                self.input.remove(at);
            }
            KeyCode::Delete if self.cursor < len => {
                let at = self.byte_at(self.cursor);
                self.input.remove(at);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            // Esc drops the reply first, then the line.
            KeyCode::Esc if self.replying.is_some() => self.replying = None,
            // A correction dropped takes its text along: it was the old one.
            KeyCode::Esc if self.editing.is_some() => {
                self.editing = None;
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Esc => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Enter => return self.submit(),
            // A line break inside the message: Alt+Enter (Shift+Enter where the
            // terminal reports it), handled in on_key.
            _ => {}
        }
        Vec::new()
    }

    /// ↑ (`up`) or ↓ in the input line: within a text of several lines, to the line above
    /// or below; from its first line up (its last down), through what was sent before.
    fn recall_key(&mut self, up: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        let line_start = |at: usize| chars[..at].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
        let start = line_start(self.cursor);
        let column = self.cursor - start;
        if up && start > 0 {
            let above = line_start(start - 1);
            self.cursor = (above + column).min(start - 1);
            return;
        }
        let end = chars[self.cursor..]
            .iter()
            .position(|c| *c == '\n')
            .map(|i| self.cursor + i);
        if !up && let Some(end) = end {
            let next_end = chars[end + 1..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |i| end + 1 + i);
            self.cursor = (end + 1 + column).min(next_end);
            return;
        }
        let shown = match (self.recall.take(), up) {
            (None, true) if !self.sent.is_empty() => {
                let draft = std::mem::take(&mut self.input);
                Some((self.sent.len() - 1, draft))
            }
            (None, _) => None,
            (Some((at, draft)), true) => Some((at.saturating_sub(1), draft)),
            (Some((at, draft)), false) if at + 1 < self.sent.len() => Some((at + 1, draft)),
            // Past the newest: back to what was being written.
            (Some((_, draft)), false) => {
                self.input = draft;
                self.cursor = self.input.chars().count();
                None
            }
        };
        if let Some((at, draft)) = shown {
            self.input = self.sent[at].clone();
            self.cursor = self.input.chars().count();
            self.recall = Some((at, draft));
        }
    }

    fn byte_at(&self, chars: usize) -> usize {
        self.input
            .char_indices()
            .nth(chars)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    fn submit(&mut self) -> Vec<Action> {
        let input = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.recall = None;
        if input.trim().is_empty() {
            return Vec::new();
        }
        // Kept for ↑, once: the same line twice in a row is one entry.
        let kept = input.trim().to_owned();
        if self.sent.last() != Some(&kept) {
            self.sent.push(kept);
        }
        // "/me waves" is a message, sent as it is (XEP-0245); other "/…" are commands.
        if input.trim_start().starts_with('/') && !is_action(input.trim_start()) {
            self.refused = false;
            let actions = self.command(input.trim());
            if self.refused {
                self.cursor = input.chars().count();
                self.input = input;
            }
            return actions;
        }
        // A pasted block keeps its indentation; only empty lines around it go.
        let text = input.trim_end().trim_start_matches(['\n', '\r']).to_owned();
        let Some(index) = self.selected else {
            self.status = tr!(
                "no chat open: /msg JID or /join ROOM",
                "нет открытого чата: /msg JID или /join КОМНАТА"
            )
            .into();
            return Vec::new();
        };
        self.send_to(index, text)
    }

    /// Sends the file at `path` to chat `index`: its line shows at once, going up.
    pub fn upload(&mut self, index: usize, path: PathBuf) -> Vec<Action> {
        let chat = &self.chats[index];
        if chat.kind == ChatKind::Server {
            self.status = tr!("files cannot be sent here", "сюда файлы не отправить").into();
            return Vec::new();
        }
        let id = omtalk_xmpp::receipts::new_id();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let action = Action::Upload {
            account: chat.account.clone(),
            to: chat.jid.clone(),
            room: chat.is_room(),
            path,
            encrypted: chat.omemo,
            id: id.clone(),
        };
        let encrypted = chat.omemo;
        let mut line = Line::message(
            chrono::Utc::now().timestamp(),
            tr!("me", "я").into(),
            name,
            encrypted,
            true,
        );
        line.id = Some(id);
        line.delivery = Some(Delivery::Uploading(0));
        self.chats[index].scroll = 0;
        self.push(index, line);
        vec![action]
    }

    /// The file of message `id` is up: its line becomes the link, as any shared file.
    pub fn uploaded(&mut self, account: &BareJid, id: &str, link: &str) {
        for chat in self.chats.iter_mut().filter(|c| &c.account == account) {
            for line in chat
                .lines
                .iter_mut()
                .filter(|l| l.outgoing && l.id.as_deref() == Some(id))
            {
                // One's own upload: a file shared, whatever its link looks like.
                let mut fresh = Line::message_sharing(
                    line.at,
                    line.who.clone(),
                    link.to_owned(),
                    line.encrypted,
                    true,
                    true,
                );
                fresh.id = line.id.take();
                fresh.delivery = line.delivery.take();
                *line = fresh;
            }
        }
    }

    /// Sends `text` to chat `index`: its line shows at once, as being sent.
    pub fn send_to(&mut self, index: usize, text: String) -> Vec<Action> {
        let chat = &mut self.chats[index];
        if chat.kind == ChatKind::Server {
            self.status = tr!("only commands here", "здесь только команды").into();
            return Vec::new();
        }
        chat.scroll = 0;
        let id = omtalk_xmpp::receipts::new_id();
        // The message itself says "active" now.
        self.typing_out = None;
        if let Some(editing) = self
            .editing
            .take()
            .filter(|e| e.account == chat.account && e.chat == chat.jid)
        {
            // The line changes where it is; the correction goes as a message of its own.
            if let Some(line) = chat
                .lines
                .iter_mut()
                .rev()
                .find(|l| l.outgoing && l.id.as_deref() == Some(editing.id.as_str()))
            {
                line.text = text.clone();
                line.edited = true;
            }
            return vec![Action::Send {
                account: chat.account.clone(),
                to: chat.jid.clone(),
                room: chat.is_room(),
                text,
                encrypted: chat.omemo,
                id,
                reply: None,
                replace: Some(editing.id),
            }];
        }
        // A reply to a message of this chat: the quote goes first.
        let replying = self
            .replying
            .take()
            .filter(|r| r.account == chat.account && r.chat == chat.jid);
        let (text, reply) = match &replying {
            Some(r) => {
                // The quote says whose it is, as clients write it: "Алиса:" first.
                let (body, length) = omtalk_xmpp::replies::body(&format!("{}:\n{}", r.who, r.quote), &text);
                (body, r.to.clone().map(|to| (to, length)))
            }
            None => (text, None),
        };
        let action = Action::Send {
            account: chat.account.clone(),
            to: chat.jid.clone(),
            room: chat.is_room(),
            text: text.clone(),
            encrypted: chat.omemo,
            id: id.clone(),
            reply: reply.clone(),
            replace: None,
        };
        let encrypted = chat.omemo;
        self.sending(index, &text, encrypted, &id, chrono::Utc::now().timestamp());
        if let Some((to, _)) = reply
            && let Some(line) = self.chats[index].lines.last_mut()
        {
            line.reply_to = Some(to.id);
        }
        vec![action]
    }

    fn command(&mut self, line: &str) -> Vec<Action> {
        let mut words = line.split_whitespace();
        let name = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        let target = args.first().and_then(|a| a.parse::<Jid>().ok());
        let account = self.account_for(target.as_ref());
        let Some(account) = account else { return Vec::new() };
        let jid = |i: usize| args.get(i).and_then(|a| a.parse::<BareJid>().ok());
        let result = match (name, jid(0)) {
            ("/msg", Some(jid)) => {
                let index = self.chat(&account, &jid, ChatKind::Direct);
                let mut actions = self.select(index);
                // `/msg JID text` also sends the text, as in other clients.
                let text = line[name.len()..]
                    .trim_start()
                    .split_once(char::is_whitespace)
                    .map(|(_, text)| text.trim())
                    .unwrap_or_default();
                if !text.is_empty() {
                    actions.extend(self.send_to(index, text.to_owned()));
                }
                return actions;
            }
            ("/join", Some(room)) => {
                let nick = args.get(1).map(|n| n.to_string());
                Ok(vec![Action::Join { account, room, nick }])
            }
            // Left for good: closed here, and no longer entered by the account's clients
            // by themselves (autojoin off in its bookmark, which stays). /close only
            // hides it here.
            ("/leave", _) => match self.current() {
                Some(Chat {
                    jid,
                    kind: ChatKind::Room { nick },
                    account,
                    ..
                }) => Ok(vec![
                    Action::CloseChat {
                        account: account.clone(),
                        jid: jid.clone(),
                    },
                    Action::Leave {
                        account: account.clone(),
                        room: jid.clone(),
                        nick: nick.clone(),
                    },
                    Action::NoAutojoin {
                        account: account.clone(),
                        room: jid.clone(),
                    },
                ]),
                _ => Err(tr!("this is not a room", "это не комната").to_owned()),
            },
            ("/add", Some(jid)) => {
                let name = (args.len() > 1).then(|| args[1..].join(" "));
                Ok(vec![Action::AddContact { account, jid, name }])
            }
            ("/accept", Some(jid)) => Ok(vec![Action::Accept { account, jid }]),
            ("/refuse", Some(jid)) => Ok(vec![Action::Refuse { account, jid }]),
            ("/devices", _) => match self.current() {
                Some(chat) if !chat.is_room() => Ok(vec![Action::Devices {
                    account: chat.account.clone(),
                    jid: chat.jid.clone(),
                }]),
                _ => Err(tr!("open a direct chat", "откройте личный чат").to_owned()),
            },
            ("/services", _) => match args.first().map(|a| a.parse::<Jid>()) {
                Some(Err(_)) => Err(tr!(
                    "/services: a service address is needed, as example.org",
                    "/services: нужен адрес сервиса, например example.org"
                )
                .to_owned()),
                address => return self.open_services(address.and_then(Result::ok)),
            },
            ("/folder", _) => match self.selected {
                Some(index) => {
                    let name = line[name.len()..].trim();
                    return self.move_to_folder(index, (!name.is_empty()).then_some(name));
                }
                None => Err(tr!(
                    "open the chat to put in a folder",
                    "откройте чат, который положить в папку"
                )
                .to_owned()),
            },
            // Names may hold spaces: "/rename-folder старая папка → новая"; two single
            // words need no arrow.
            ("/rename-folder", _) => {
                let rest = line[name.len()..].trim();
                let parts = rest
                    .split_once('→')
                    .map(|(a, b)| (a.trim(), b.trim()))
                    .or_else(|| {
                        let mut words = rest.split_whitespace();
                        match (words.next(), words.next(), words.next()) {
                            (Some(a), Some(b), None) => Some((a, b)),
                            _ => None,
                        }
                    });
                match parts {
                    Some((old, new)) if !old.is_empty() && !new.is_empty() => self.rename_folder(old, new),
                    _ => Err(tr!("/rename-folder OLD NEW", "/rename-folder СТАРАЯ НОВАЯ").to_owned()),
                }
            }
            // The list shows while a command is being typed: start one.
            ("/upload", _) => {
                let path = line[name.len()..].trim();
                match (self.selected, expand_home(path)) {
                    (None, _) => Err(tr!("no chat open", "нет открытого чата").to_owned()),
                    (_, None) => Err(tr!("/upload PATH", "/upload ПУТЬ").to_owned()),
                    (Some(_), Some(file)) if !file.is_file() => {
                        Err(trf!("no such file: {}", "нет такого файла: {}", file.display()))
                    }
                    (Some(index), Some(file)) => return self.upload(index, file),
                }
            }
            ("/keys", _) => {
                let peer = match args.first().map(|a| a.parse::<BareJid>()) {
                    Some(Ok(peer)) => Some(peer),
                    Some(Err(_)) => return self.say(tr!("/keys [ADDRESS]", "/keys [АДРЕС]")),
                    None => None,
                };
                return self.open_keys(peer);
            }
            ("/reply", _) => return self.reply_here(),
            ("/search", _) => {
                let words = line["/search".len()..].trim();
                let Some(chat) = self.current().filter(|c| c.kind != ChatKind::Server) else {
                    return self.say(tr!("open a chat to search in", "откройте чат, в котором искать"));
                };
                if words.is_empty() {
                    return self.say(tr!("as: /search words", "так: /search слова"));
                }
                let (account, chat) = (chat.account.clone(), chat.jid.clone());
                self.search = Some(Search {
                    account: account.clone(),
                    chat: chat.clone(),
                    words: words.to_owned(),
                    found: None,
                    cursor: 0,
                });
                return vec![Action::Search {
                    account,
                    chat,
                    words: words.to_owned(),
                }];
            }
            ("/away" | "/dnd" | "/online", _) => {
                use xmpp_parsers::presence::Show;
                let show = match name {
                    "/away" => Some(Show::Away),
                    "/dnd" => Some(Show::Dnd),
                    _ => None,
                };
                let text = line[name.len()..].trim();
                let status = omtalk_xmpp::presence::OwnStatus {
                    show,
                    text: (!text.is_empty()).then(|| text.to_owned()),
                };
                self.own_status = status.clone();
                self.status = trf!("status: {}", "статус: {}", status_words(&status));
                return vec![Action::SetStatus(status)];
            }
            ("/react", _) => {
                let reaction = line["/react".len()..].trim();
                return self.react_here(reaction);
            }
            ("/edit", _) => {
                self.edit_last();
                return Vec::new();
            }
            ("/me", _) => {
                return self.say(tr!(
                    "/me TEXT says what you do, as in IRC",
                    "/me ТЕКСТ — о себе в третьем лице, как в IRC"
                ));
            }
            ("/settings", _) => return self.open_settings(),
            ("/commands", _) => return self.open_chat_commands(),
            ("/nick", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let nick = line[name.len()..].trim().to_owned();
                    if nick.is_empty() {
                        return self.say(tr!(
                            "/nick NICK changes your nick in this room",
                            "/nick НИК — сменить свой ник в этой комнате"
                        ));
                    }
                    Ok(vec![Action::Nick {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        nick,
                    }])
                }
                None => Err(tr!("this is not a room", "это не комната").to_owned()),
            },
            ("/topic", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let topic = line[name.len()..].trim().to_owned();
                    if topic.is_empty() {
                        let now = chat
                            .topic
                            .clone()
                            .unwrap_or_else(|| tr!("no topic", "темы нет").into());
                        return self.say(&trf!(
                            "topic: {now} · /topic TEXT changes it",
                            "тема: {now} · /topic ТЕКСТ — сменить"
                        ));
                    }
                    Ok(vec![Action::SetTopic {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        topic,
                    }])
                }
                None => Err(tr!("this is not a room", "это не комната").to_owned()),
            },
            ("/name", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let new = line[name.len()..].trim().to_owned();
                    if new.is_empty() {
                        return self.say(tr!(
                            "/name TITLE changes the room's title (owner only)",
                            "/name НАЗВАНИЕ — сменить название комнаты (только владелец)"
                        ));
                    }
                    Ok(vec![Action::SetRoomName {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        name: new,
                    }])
                }
                None => Err(tr!("this is not a room", "это не комната").to_owned()),
            },
            ("/create", _) => {
                let mut rest = line[name.len()..].trim().splitn(2, char::is_whitespace);
                let address = rest.next().unwrap_or_default();
                let title = rest
                    .next()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned);
                if address.is_empty() {
                    return self.say(tr!(
                        "/create NAME [TITLE] makes a closed room; NAME or name@service",
                        "/create ИМЯ [НАЗВАНИЕ] — новая закрытая комната; ИМЯ или имя@сервис"
                    ));
                }
                // A bare name goes to the account's room service, which the runtime finds.
                let room = if address.contains('@') {
                    address.parse::<BareJid>().ok()
                } else {
                    format!("{address}@rooms.invalid").parse::<BareJid>().ok()
                };
                match room {
                    Some(room) => Ok(vec![Action::CreateRoom {
                        account,
                        room,
                        name: title,
                        nick: None,
                    }]),
                    None => Err(trf!(
                        "{address} cannot be a room's address",
                        "{address}: не годится для адреса комнаты"
                    )),
                }
            }
            ("/invite", Some(jid)) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => Ok(vec![Action::Invite {
                    account: chat.account.clone(),
                    room: chat.jid.clone(),
                    jid,
                }]),
                None => Err(tr!(
                    "open the room to invite to",
                    "откройте комнату, в которую позвать"
                )
                .to_owned()),
            },
            ("/close", _) => match self.selected {
                Some(index) => return self.close_chat(index),
                None => Err(tr!("no chat open", "нет открытого чата").to_owned()),
            },
            ("/quit", _) => {
                self.quit = true;
                return vec![Action::Quit];
            }
            ("/help", _) => {
                self.input = "/".into();
                self.cursor = 1;
                self.focus = Focus::Input;
                Ok(Vec::new())
            }
            ("/msg" | "/join" | "/add" | "/accept" | "/refuse" | "/invite", None) => Err(trf!(
                "{name}: an address as name@server is needed",
                "{name}: нужен адрес вида name@server"
            )),
            _ => Err(trf!("no command {name}", "нет команды {name}")),
        };
        match result {
            Ok(actions) => actions,
            Err(message) => {
                self.status = message;
                self.refused = true;
                Vec::new()
            }
        }
    }

    /// The account to reach `jid` from: the one on its server (or on the server whose
    /// service it is, as `muc.example.org`), else the one last picked.
    pub fn account_for(&self, jid: Option<&Jid>) -> Option<BareJid> {
        let domain = jid.map(|j| j.domain().as_str());
        let local = domain.and_then(|d| {
            self.accounts
                .iter()
                .find(|a| a.domain().as_str() == d)
                .or_else(|| {
                    self.accounts
                        .iter()
                        .find(|a| d.ends_with(&format!(".{}", a.domain())))
                })
        });
        local.or(self.active.as_ref()).or(self.accounts.first()).cloned()
    }

    /// Pasted text goes in at the cursor as it is, line breaks included; in the services
    /// window's form, into the field being edited, on one line.
    pub fn on_paste(&mut self, text: &str) -> Vec<Action> {
        let before = self.input.clone();
        let mut actions = self.paste(text);
        if self.input != before {
            actions.extend(self.after_edit());
        }
        actions
    }

    fn paste(&mut self, text: &str) -> Vec<Action> {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(services) = self.services.as_mut() {
            for c in text.chars() {
                let c = if c == '\n' { ' ' } else { c };
                services.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            return Vec::new();
        }
        self.focus = Focus::Input;
        self.completion = None;
        // A file dropped on the terminal arrives as its path: offer to send it.
        if self.input.is_empty()
            && let Some(file) = dropped_file(&text)
        {
            self.input = format!("/upload {}", file.display());
            self.cursor = self.input.chars().count();
            self.status = tr!("Enter sends this file", "Enter — отправить этот файл").into();
            return Vec::new();
        }
        let at = self.byte_at(self.cursor);
        self.input.insert_str(at, &text);
        self.cursor += text.chars().count();
        Vec::new()
    }

    /// The text under the selection, row by row as shown.
    pub fn selected_text(&self) -> String {
        let Some((a, b)) = self.selection else {
            return String::new();
        };
        let ((sx, sy), (ex, ey)) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
        let mut rows: Vec<&(Rect, String)> = self
            .areas
            .message_rows
            .iter()
            .filter(|(r, _)| r.y >= sy && r.y <= ey)
            .collect();
        rows.sort_by_key(|(r, _)| r.y);
        let mut out = Vec::new();
        for (rect, text) in rows {
            let from = if rect.y == sy {
                sx.saturating_sub(rect.x) as usize
            } else {
                0
            };
            let to = if rect.y == ey {
                (ex.saturating_sub(rect.x) as usize) + 1
            } else {
                usize::MAX
            };
            let piece = columns(text, from, to);
            // The conversation's own indent is not part of what was said.
            out.push(piece.strip_prefix("  ").unwrap_or(&piece).trim_end().to_owned());
        }
        out.join("\n").trim_matches('\n').to_owned()
    }

    /// Line editing as in a shell (readline): Ctrl+W/U/K cut a word, to the start, to the
    /// end; Ctrl+Y puts back what was cut; Ctrl+A/E go to the start and end; Ctrl+←/→
    /// and Alt+B/F go by words; Ctrl+H and Ctrl+D delete a character. They win over the
    /// held-modifier shortcuts while typing (the leader key reaches all of those). With
    /// Alt as the modifier, Alt+B/F stay its shortcuts. `false` for any other key.
    fn edit_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT) && self.modifier != KeyModifiers::ALT;
        let chars: Vec<char> = self.input.chars().collect();
        let word_back = |from: usize| {
            let mut at = from;
            while at > 0 && chars[at - 1].is_whitespace() {
                at -= 1;
            }
            while at > 0 && !chars[at - 1].is_whitespace() {
                at -= 1;
            }
            at
        };
        let word_forward = |from: usize| {
            let mut at = from;
            while at < chars.len() && chars[at].is_whitespace() {
                at += 1;
            }
            while at < chars.len() && !chars[at].is_whitespace() {
                at += 1;
            }
            at
        };
        let cursor = self.cursor;
        let mut cut = |from: usize, to: usize| {
            self.killed = chars[from..to].iter().collect();
            self.input = chars[..from].iter().chain(&chars[to..]).collect();
            self.cursor = from;
        };
        match (key.code, ctrl, alt) {
            (KeyCode::Char('w'), true, _) | (KeyCode::Backspace, true, _) | (KeyCode::Backspace, _, true) => {
                cut(word_back(cursor), cursor)
            }
            (KeyCode::Char('u'), true, _) => cut(0, cursor),
            (KeyCode::Char('k'), true, _) => cut(cursor, chars.len()),
            (KeyCode::Char('h'), true, _) if cursor > 0 => cut(cursor - 1, cursor),
            (KeyCode::Char('d'), true, _) if cursor < chars.len() => cut(cursor, cursor + 1),
            (KeyCode::Char('y'), true, _) => {
                let killed: Vec<char> = self.killed.chars().collect();
                self.input = chars[..cursor]
                    .iter()
                    .chain(&killed)
                    .chain(&chars[cursor..])
                    .collect();
                self.cursor = cursor + killed.len();
            }
            (KeyCode::Char('a'), true, _) => self.cursor = 0,
            (KeyCode::Char('e'), true, _) => self.cursor = chars.len(),
            (KeyCode::Left, true, _) | (KeyCode::Char('b'), _, true) => self.cursor = word_back(cursor),
            (KeyCode::Right, true, _) | (KeyCode::Char('f'), _, true) => self.cursor = word_forward(cursor),
            _ => return false,
        }
        self.completion = None;
        true
    }

    /// A word in the status line, as a command's answer.
    fn say(&mut self, text: &str) -> Vec<Action> {
        self.status = text.to_owned();
        self.refused = true;
        Vec::new()
    }

    /// What Tab can complete here.
    fn completion_context(&self) -> complete::Context {
        let mut context = complete::Context::default();
        if let Some(chat) = self.current().filter(|c| c.is_room()) {
            let own = match &chat.kind {
                ChatKind::Room { nick } => nick.as_str(),
                ChatKind::Direct | ChatKind::Server => "",
            };
            context.nicks = chat.occupants.keys().filter(|n| *n != own).cloned().collect();
        }
        for chat in self.chats.iter().filter(|c| c.kind != ChatKind::Server) {
            let list = if chat.is_room() {
                &mut context.rooms
            } else {
                &mut context.people
            };
            list.push(chat.jid.to_string());
            for real in chat.occupants.values().flatten() {
                if !self.accounts.contains(real) {
                    context.people.push(real.to_string());
                }
            }
        }
        let account = self.account_for(None);
        let mut servers: Vec<String> = self.accounts.iter().map(|a| a.domain().to_string()).collect();
        for room in self.chats.iter().filter(|c| c.is_room()) {
            let domain = room.jid.domain().to_string();
            if !servers.contains(&domain) {
                servers.push(domain);
            }
        }
        context.servers = servers;
        context.folders = self
            .folders
            .iter()
            .filter(|f| Some(&f.account) == account.as_ref())
            .map(|f| f.name.clone())
            .collect();
        context
    }

    /// Completes the word before the cursor; again, the next candidate (Shift+Tab, the
    /// one before).
    fn complete(&mut self, forward: bool) {
        let completion = match self.completion.take() {
            Some(mut c) => {
                let len = c.candidates.len();
                c.index = if forward {
                    (c.index + 1) % len
                } else {
                    (c.index + len - 1) % len
                };
                c
            }
            None => {
                let Some((start, candidates)) =
                    complete::candidates(&self.input, self.cursor, &self.completion_context())
                else {
                    return;
                };
                let index = if forward { 0 } else { candidates.len() - 1 };
                Completion {
                    start,
                    inserted: self.cursor - start,
                    candidates,
                    index,
                }
            }
        };
        let chars: Vec<char> = self.input.chars().collect();
        let candidate = &completion.candidates[completion.index];
        let mut line: String = chars[..completion.start].iter().collect();
        line.push_str(candidate);
        line.extend(&chars[completion.start + completion.inserted..]);
        self.input = line;
        let inserted = candidate.chars().count();
        self.cursor = completion.start + inserted;
        // One candidate: done. Several: Tab goes on through them.
        if completion.candidates.len() > 1 {
            self.completion = Some(Completion {
                inserted,
                ..completion
            });
        }
    }

    /// Opens a direct chat with a room participant, by nick: the room must show who is
    /// behind it.
    pub fn open_private(&mut self, nick: &str) -> Vec<Action> {
        let Some(chat) = self.current().filter(|c| c.is_room()) else {
            return Vec::new();
        };
        let account = chat.account.clone();
        match chat.occupants.get(nick) {
            Some(Some(real)) if *real == account => {
                self.status = tr!("this is you", "это вы").into();
                Vec::new()
            }
            Some(Some(real)) => {
                let real = real.clone();
                let index = self.chat(&account, &real, ChatKind::Direct);
                self.focus = Focus::Input;
                self.select(index)
            }
            Some(None) => {
                self.status = trf!(
                    "{nick}: the room does not show who this is",
                    "{nick}: комната не показывает адрес участника"
                );
                Vec::new()
            }
            None => {
                self.status = trf!(
                    "{nick} is not in the room now, the address is unknown",
                    "{nick} сейчас не в комнате, адрес неизвестен"
                );
                Vec::new()
            }
        }
    }

    /// Opens the keys window: `peer`'s devices (else the open chat's peer) and one's own.
    pub fn open_keys(&mut self, peer: Option<BareJid>) -> Vec<Action> {
        let peer = peer.or_else(|| {
            self.current()
                .filter(|c| c.kind == ChatKind::Direct)
                .map(|c| c.jid.clone())
        });
        let Some(account) = self.account_for(peer.clone().map(Jid::from).as_ref()) else {
            return Vec::new();
        };
        let (keys, request) = crate::keys::Keys::open(account, peer);
        self.keys = Some(keys);
        vec![Action::Keys(request)]
    }

    fn keys_outcome(&mut self, outcome: crate::keys::Outcome) -> Vec<Action> {
        match outcome {
            crate::keys::Outcome::Stay => Vec::new(),
            crate::keys::Outcome::Close => {
                self.keys = None;
                Vec::new()
            }
            crate::keys::Outcome::Ask(request) => vec![Action::Keys(request)],
        }
    }

    fn keys_mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let row = self
            .areas
            .key_rows
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, i)| i);
        let button = self
            .areas
            .key_buttons
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, b)| b);
        let outside = !inside(self.areas.key_window);
        let Some(keys) = self.keys.as_mut() else {
            return Vec::new();
        };
        let outcome = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match (button, row) {
                (Some(button), _) => keys.press(button),
                (None, Some(row)) => keys.click_row(row),
                _ if outside => crate::keys::Outcome::Close,
                _ => crate::keys::Outcome::Stay,
            },
            MouseEventKind::ScrollUp => keys.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            MouseEventKind::ScrollDown => keys.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            _ => crate::keys::Outcome::Stay,
        };
        self.keys_outcome(outcome)
    }

    /// A direct chat with nothing in it (this device is new, or never had it): its
    /// history is asked of the archive, once.
    fn ask_history(&mut self, index: usize) -> Option<Action> {
        let chat = self.chats.get(index)?;
        if chat.kind != ChatKind::Direct || chat.lines.iter().any(|l| !l.notice) {
            return None;
        }
        let key = (chat.account.clone(), chat.jid.clone());
        self.history_asked.insert(key.clone()).then_some(Action::History {
            account: key.0,
            peer: key.1,
        })
    }

    /// The avatar of chat `index`, asked for once while avatars are shown.
    pub fn ask_avatar(&mut self, index: usize) -> Option<Action> {
        let chat = self.chats.get(index)?;
        let room = match chat.kind {
            ChatKind::Direct => false,
            ChatKind::Room { .. } => true,
            ChatKind::Server => return None,
        };
        let key = (chat.account.clone(), chat.jid.clone());
        if !self.show_avatars || !self.avatars_asked.insert(key) {
            return None;
        }
        Some(Action::Avatar {
            account: chat.account.clone(),
            jid: chat.jid.clone(),
            room,
        })
    }

    /// The picture to draw for `jid`, if one is shown.
    pub fn avatar_of(&self, jid: &BareJid) -> Option<String> {
        self.show_avatars
            .then(|| self.avatars.get(jid))
            .flatten()
            .map(|hash| format!("avatar:{hash}"))
    }

    /// Opens the settings window on the settings as they are now, the ones changed on
    /// the way (the mouse, the widths) too.
    pub fn open_settings(&mut self) -> Vec<Action> {
        let mut ui = self.config_ui.clone();
        ui.mouse = self.mouse;
        ui.left = self.left_width;
        ui.right = self.right_width;
        ui.read_markers = self.read_markers;
        ui.typing = self.send_typing;
        ui.avatars = self.show_avatars;
        self.settings = Some(crate::settings::Settings::open(crate::settings::Current {
            ui: &ui,
            notify: &self.config_notify,
        }));
        Vec::new()
    }

    fn settings_outcome(&mut self, outcome: crate::settings::Outcome) -> Vec<Action> {
        match outcome {
            crate::settings::Outcome::Stay => Vec::new(),
            crate::settings::Outcome::Close => {
                self.settings = None;
                Vec::new()
            }
            crate::settings::Outcome::Changed(row) => self.apply_setting(row),
        }
    }

    /// Puts a changed setting to use at once, and has it written down.
    fn apply_setting(&mut self, row: usize) -> Vec<Action> {
        use crate::settings::Value;
        let Some(settings) = self.settings.as_mut() else {
            return Vec::new();
        };
        let setting = settings.rows[row].clone();
        let used: Result<(), String> = match (setting.key, &setting.value) {
            ("modifier", value) => {
                let (modifier, ui) = match value.text().as_str() {
                    "alt" => (KeyModifiers::ALT, Modifier::Alt),
                    "super" => (KeyModifiers::SUPER, Modifier::Super),
                    "none" => (KeyModifiers::NONE, Modifier::None),
                    _ => (KeyModifiers::CONTROL, Modifier::Ctrl),
                };
                self.modifier = modifier;
                self.config_ui.modifier = ui;
                Ok(())
            }
            ("leader", Value::Text(text)) => crate::config::parse_key(text).map(|key| {
                self.leader = key;
                self.config_ui.leader = text.clone();
            }),
            ("icons", value) => {
                let text = value.text() == "text";
                self.icons = if text {
                    crate::theme::text_icons()
                } else {
                    crate::theme::nerd()
                };
                self.config_ui.icons = if text {
                    crate::config::IconSet::Text
                } else {
                    crate::config::IconSet::Nerd
                };
                Ok(())
            }
            ("left", Value::Number { value, .. }) => {
                self.left_width = *value;
                self.config_ui.left = *value;
                Ok(())
            }
            ("right", Value::Number { value, .. }) => {
                self.right_width = *value;
                self.config_ui.right = *value;
                Ok(())
            }
            ("mouse", Value::Bool(on)) => {
                self.mouse = *on;
                self.config_ui.mouse = *on;
                Ok(())
            }
            ("read_markers", Value::Bool(on)) => {
                self.read_markers = *on;
                self.config_ui.read_markers = *on;
                Ok(())
            }
            ("typing", Value::Bool(on)) => {
                self.send_typing = *on;
                self.config_ui.typing = *on;
                Ok(())
            }
            ("auto_download", Value::Bool(on)) => {
                self.config_ui.auto_download = *on;
                Ok(())
            }
            ("language", value) => {
                let language = if value.text() == "ru" {
                    crate::lang::Language::Russian
                } else {
                    crate::lang::Language::English
                };
                crate::lang::set(language);
                self.config_ui.language = language;
                // The icons carry words too.
                self.icons = match self.config_ui.icons {
                    crate::config::IconSet::Text => crate::theme::text_icons(),
                    crate::config::IconSet::Nerd => crate::theme::nerd(),
                };
                Ok(())
            }
            ("avatars", Value::Bool(on)) => {
                self.show_avatars = *on;
                self.config_ui.avatars = *on;
                Ok(())
            }
            ("viewer", Value::Text(text)) if text.is_empty() => {
                Err(tr!("a program is needed", "нужна программа").into())
            }
            ("viewer", Value::Text(text)) => {
                self.config_ui.viewer = text.clone();
                Ok(())
            }
            (_, _) if setting.section == "notify" => {
                let notify = settings.notify();
                notify.ready().map(|ready| {
                    self.notify = ready;
                    self.config_notify = notify;
                    self.config_ui.bell = true;
                })
            }
            _ => Ok(()),
        };
        // Turned on: the open chat's avatar is asked for at once.
        let avatar = match (setting.key, used.is_ok()) {
            ("avatars", true) => self.selected.and_then(|i| self.ask_avatar(i)),
            _ => None,
        };
        // The window itself in the language just chosen, where it was.
        if setting.key == "language" && used.is_ok() {
            let cursor = self.settings.as_ref().map_or(0, |s| s.cursor);
            self.open_settings();
            if let Some(settings) = self.settings.as_mut() {
                settings.cursor = cursor;
            }
        }
        let settings = self.settings.as_mut().expect("open");
        match used {
            Ok(()) => {
                settings.problem = None;
                let mut actions = vec![Action::Setting {
                    section: setting.section,
                    key: setting.key,
                    value: setting.value.written(),
                }];
                actions.extend(avatar);
                actions
            }
            Err(problem) => {
                settings.problem = Some(problem);
                Vec::new()
            }
        }
    }

    fn settings_mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let row = self
            .areas
            .settings_rows
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, i)| i);
        let outside = !inside(self.areas.settings_window);
        let Some(settings) = self.settings.as_mut() else {
            return Vec::new();
        };
        let outcome = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match row {
                Some(row) => settings.click_row(row),
                None if outside => crate::settings::Outcome::Close,
                None => crate::settings::Outcome::Stay,
            },
            MouseEventKind::ScrollUp => settings.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            MouseEventKind::ScrollDown => settings.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            _ => crate::settings::Outcome::Stay,
        };
        self.settings_outcome(outcome)
    }

    /// Opens the services window on `jid`, or on the current account's own server.
    pub fn open_services(&mut self, jid: Option<Jid>) -> Vec<Action> {
        let Some(account) = self.account_for(jid.as_ref()) else {
            return Vec::new();
        };
        let jid = jid.unwrap_or_else(|| Jid::from(account.domain().to_owned()));
        let (services, request) = Services::open(account, jid);
        self.services = Some(services);
        vec![Action::Services {
            request,
            title: String::new(),
        }]
    }

    /// The commands of the open chat (Gajim's "Execute command"): a room's, a contact's
    /// client's while one is online (else the contact's address), the own server's from
    /// the server channel. A biboumi channel (`#chan%irc.server@biboumi`) leads back to
    /// its IRC server's commands.
    pub fn open_chat_commands(&mut self) -> Vec<Action> {
        let Some(chat) = self.current() else {
            return self.open_services(None);
        };
        let account = chat.account.clone();
        let jid: Jid = match chat.kind {
            ChatKind::Server => Jid::from(account.domain().to_owned()),
            ChatKind::Room { .. } => chat.jid.clone().into(),
            ChatKind::Direct => match self.clients(&account, &chat.jid).first() {
                Some(client) => format!("{}/{}", chat.jid, client.resource)
                    .parse()
                    .unwrap_or_else(|_| chat.jid.clone().into()),
                None => chat.jid.clone().into(),
            },
        };
        let parent = chat
            .is_room()
            .then(|| {
                chat.jid
                    .node()?
                    .as_str()
                    .split_once('%')
                    .map(|(_, server)| server.to_owned())
            })
            .flatten()
            .and_then(|server| format!("{server}@{}", chat.jid.domain()).parse::<Jid>().ok());
        let (mut services, request) = Services::open(account, jid);
        if let Some(parent) = parent {
            services = services.with_parent(parent);
        }
        self.services = Some(services);
        vec![Action::Services {
            request,
            title: String::new(),
        }]
    }

    /// Closes the window; a command still waiting for its form is cancelled on the
    /// server, so it does not hang there half done.
    pub fn close_services(&mut self) -> Vec<Action> {
        let Some(mut services) = self.services.take() else {
            return Vec::new();
        };
        match services.form.as_ref().filter(|f| !f.finished()) {
            Some(_) => match services.act(CommandAction::Cancel) {
                Outcome::Request(request, title) => vec![Action::Services { request, title }],
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    fn services_outcome(&mut self, outcome: Outcome) -> Vec<Action> {
        match outcome {
            Outcome::Stay => Vec::new(),
            Outcome::Close => self.close_services(),
            Outcome::Request(request, title) => vec![Action::Services { request, title }],
        }
    }

    fn services_mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let areas = &self.areas;
        let row = areas
            .service_rows
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, i)| i);
        let field = areas
            .service_fields
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, i)| i);
        let button = areas
            .service_buttons
            .iter()
            .find(|(r, _)| inside(*r))
            .map(|&(_, b)| b);
        let outside = !inside(areas.service_window);
        let Some(services) = self.services.as_mut() else {
            return Vec::new();
        };
        let outcome = match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match (button, row, field) {
                (Some(ServiceButton::Close), ..) => return self.close_services(),
                (Some(ServiceButton::Back), ..) => services.go_back(),
                (Some(ServiceButton::Reload), ..) => services.reload(),
                (Some(ServiceButton::Step(action)), ..) => services.act(action),
                (Some(ServiceButton::Dismiss), ..) => {
                    services.form = None;
                    Outcome::Stay
                }
                (None, Some(row), _) => services.click_row(row),
                (None, _, Some((field, option))) => services.click_field(field, option),
                // A click beside the window leaves it, as Esc does.
                _ if outside => return self.close_services(),
                _ => Outcome::Stay,
            },
            MouseEventKind::ScrollUp => services.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            MouseEventKind::ScrollDown => services.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            _ => Outcome::Stay,
        };
        self.services_outcome(outcome)
    }

    /// OMEMO on or off in the open chat; the choice is kept for it.
    pub fn toggle_omemo(&mut self) -> Vec<Action> {
        let Some(chat) = self.current_mut() else {
            return Vec::new();
        };
        chat.omemo = !chat.omemo;
        let (account, jid, on) = (chat.account.clone(), chat.jid.clone(), chat.omemo);
        self.omemo_choice.insert((account.clone(), jid.clone()), on);
        vec![Action::SetOmemo {
            account,
            chat: jid,
            on,
        }]
    }

    /// The folder holding a chat, if any.
    fn folder_of(&self, index: usize) -> Option<usize> {
        let chat = &self.chats[index];
        self.folders
            .iter()
            .position(|f| f.account == chat.account && f.chats.contains(&chat.jid))
    }

    /// The heading a chat is listed under.
    pub fn group_of(&self, index: usize) -> Group {
        match self.folder_of(index) {
            Some(folder) => Group::Folder(
                self.folders[folder].account.clone(),
                self.folders[folder].name.clone(),
            ),
            None => Group::Account(self.chats[index].account.clone()),
        }
    }

    /// The chat list as a tree, each row with its depth: accounts, in each its folders
    /// with their chats, then its other chats. Account headings only when there is more
    /// than one account or any folder.
    pub fn tree(&self) -> Vec<(Entry, usize)> {
        let mut rows = Vec::new();
        let headings = self.accounts.len() > 1 || !self.folders.is_empty();
        let base = usize::from(headings);
        for account in &self.accounts {
            let group = Group::Account(account.clone());
            if headings {
                rows.push((Entry::Heading(group.clone()), 0));
                if self.collapsed.contains(&group) {
                    continue;
                }
            }
            let mut placed = Vec::new();
            for folder in self.folders.iter().filter(|f| &f.account == account) {
                let group = Group::Folder(account.clone(), folder.name.clone());
                let folded = self.collapsed.contains(&group);
                rows.push((Entry::Heading(group), base));
                for jid in &folder.chats {
                    if let Some(index) = self
                        .chats
                        .iter()
                        .position(|c| &c.account == account && &c.jid == jid)
                    {
                        placed.push(index);
                        if !folded {
                            rows.push((Entry::Chat(index), base + 1));
                        }
                    }
                }
            }
            // The server channel first, then the other chats in no folder.
            let loose = self
                .chats
                .iter()
                .enumerate()
                .filter(|(i, c)| &c.account == account && !placed.contains(i))
                .filter(|(_, c)| c.kind == ChatKind::Server)
                .chain(self.chats.iter().enumerate().filter(|(i, c)| {
                    &c.account == account && !placed.contains(i) && c.kind != ChatKind::Server
                }));
            for (index, _) in loose {
                rows.push((Entry::Chat(index), base));
            }
        }
        rows
    }

    /// The chat list as shown, without the depths.
    pub fn list(&self) -> Vec<Entry> {
        self.tree().into_iter().map(|(entry, _)| entry).collect()
    }

    /// Chats in the order the list shows them, folded ones left out.
    pub fn visible_chats(&self) -> Vec<usize> {
        self.list()
            .into_iter()
            .filter_map(|e| match e {
                Entry::Chat(i) => Some(i),
                Entry::Heading(_) => None,
            })
            .collect()
    }

    /// Folds a heading's chats away, or opens them again.
    pub fn toggle_group(&mut self, group: &Group) {
        if !self.collapsed.remove(group) {
            self.collapsed.insert(group.clone());
        }
    }

    /// Folds every account but the current chat's: to look at one server only. Again,
    /// and all are open.
    pub fn focus_group(&mut self) {
        let Some(current) = self.current().map(|c| Group::Account(c.account.clone())) else {
            return;
        };
        let others: BTreeSet<Group> = self
            .accounts
            .iter()
            .map(|a| Group::Account(a.clone()))
            .filter(|g| *g != current)
            .collect();
        let folded: BTreeSet<Group> = self
            .collapsed
            .iter()
            .filter(|g| matches!(g, Group::Account(_)))
            .cloned()
            .collect();
        self.collapsed.retain(|g| !matches!(g, Group::Account(_)));
        if folded != others {
            self.collapsed.extend(others);
        }
    }

    /// Puts a chat into a folder of its account, made if new, or (with `None`) takes it
    /// out of its own. A folder left empty goes.
    pub fn move_to_folder(&mut self, index: usize, folder: Option<&str>) -> Vec<Action> {
        let Some(chat) = self.chats.get(index) else {
            return Vec::new();
        };
        if chat.kind == ChatKind::Server {
            self.status = tr!(
                "the server channel does not go in a folder",
                "служебный канал не кладётся в папку"
            )
            .into();
            return Vec::new();
        }
        let (account, jid) = (chat.account.clone(), chat.jid.clone());
        for f in self.folders.iter_mut().filter(|f| f.account == account) {
            f.chats.retain(|c| *c != jid);
        }
        if let Some(name) = folder {
            match self
                .folders
                .iter_mut()
                .find(|f| f.account == account && f.name == name)
            {
                Some(f) => f.chats.push(jid),
                None => self.folders.push(Folder {
                    account: account.clone(),
                    name: name.to_owned(),
                    chats: vec![jid],
                }),
            }
            // Where it went stays open, so the chat does not vanish from sight.
            self.collapsed
                .remove(&Group::Folder(account.clone(), name.to_owned()));
            self.collapsed.remove(&Group::Account(account));
        }
        self.folders.retain(|f| !f.chats.is_empty());
        vec![Action::SaveFolders(self.folders.clone())]
    }

    /// Renames a folder of the account commands act for.
    fn rename_folder(&mut self, old: &str, new: &str) -> Result<Vec<Action>, String> {
        let Some(account) = self.account_for(None) else {
            return Err(tr!("no account", "нет аккаунта").to_owned());
        };
        if self.folders.iter().any(|f| f.account == account && f.name == new) {
            return Err(trf!(
                "there is a folder «{new}» already",
                "папка «{new}» уже есть"
            ));
        }
        let Some(folder) = self
            .folders
            .iter_mut()
            .find(|f| f.account == account && f.name == old)
        else {
            return Err(trf!(
                "{account} has no folder «{old}»",
                "в {account} нет папки «{old}»"
            ));
        };
        folder.name = new.to_owned();
        if self
            .collapsed
            .remove(&Group::Folder(account.clone(), old.to_owned()))
        {
            self.collapsed.insert(Group::Folder(account, new.to_owned()));
        }
        Ok(vec![Action::SaveFolders(self.folders.clone())])
    }

    fn move_selection(&mut self, by: isize) -> Vec<Action> {
        let order = self.visible_chats();
        if order.is_empty() {
            return Vec::new();
        }
        let len = order.len() as isize;
        let next = match self.selected.and_then(|s| order.iter().position(|&i| i == s)) {
            Some(at) => (at as isize + by).rem_euclid(len),
            None if by < 0 => len - 1,
            None => 0,
        };
        self.select(order[next as usize])
    }

    fn scroll(&mut self, by: isize) -> Vec<Action> {
        let top_shown = self.areas.top_shown;
        let Some(chat) = self.current_mut() else {
            return Vec::new();
        };
        chat.scroll = chat.scroll.saturating_add_signed(by);
        // Up past the oldest shown: the ones before it.
        if by > 0 && top_shown {
            return self.ask_older();
        }
        Vec::new()
    }

    /// Shows the chat from result `index` of the search on; the search closes.
    pub fn jump(&mut self, index: usize) -> Vec<Action> {
        let Some(search) = self.search.take() else {
            return Vec::new();
        };
        let Some(found) = search.found.as_ref().and_then(|f| f.get(index)) else {
            return Vec::new();
        };
        let room = self
            .chats
            .iter()
            .find(|c| c.account == search.account && c.jid == search.chat)
            .is_some_and(|c| c.is_room());
        vec![Action::Jump {
            account: search.account,
            chat: search.chat,
            room,
            at: found.at,
        }]
    }

    /// The search's results came.
    pub fn found(&mut self, account: &BareJid, chat: &BareJid, words: &str, found: Vec<Found>) {
        if let Some(search) = self
            .search
            .as_mut()
            .filter(|s| &s.account == account && &s.chat == chat && s.words == words)
        {
            search.found = Some(found);
            search.cursor = 0;
        }
    }

    /// The chat shown from `at` on (see [`Action::Jump`]): what comes before is had by
    /// scrolling up, as ever.
    pub fn jumped(&mut self, account: &BareJid, jid: &BareJid, lines: Vec<Line>, at: i64) {
        let Some(index) = self
            .chats
            .iter()
            .position(|c| &c.account == account && &c.jid == jid)
        else {
            return;
        };
        let chat = &mut self.chats[index];
        chat.lines = lines;
        chat.scroll = 0;
        chat.older_loading = false;
        chat.older_done = false;
        chat.older_before = None;
        chat.jump_to = Some(at);
        self.focus = Focus::Messages;
    }

    /// Asks for the open chat's messages before those it shows, once at a time.
    pub fn ask_older(&mut self) -> Vec<Action> {
        let Some(chat) = self.current_mut() else {
            return Vec::new();
        };
        if !chat.older_loading && !chat.older_done && chat.kind != ChatKind::Server {
            chat.older_loading = true;
            let oldest_archived = chat.lines.iter().find_map(|l| l.archive_id.clone());
            return vec![Action::Older {
                account: chat.account.clone(),
                chat: chat.jid.clone(),
                room: chat.is_room(),
                have: chat.lines.iter().filter(|l| !l.notice).count(),
                before: chat.older_before.clone().or(oldest_archived),
            }];
        }
        Vec::new()
    }

    /// Older messages of a chat, come from the store or the archive: before the ones
    /// shown, each once. `before` is where the archive's next page ends.
    pub fn older(
        &mut self,
        account: &BareJid,
        jid: &BareJid,
        lines: Vec<Line>,
        before: Option<String>,
        done: bool,
    ) {
        let Some(chat) = self
            .chats
            .iter_mut()
            .find(|c| &c.account == account && &c.jid == jid)
        else {
            return;
        };
        chat.older_loading = false;
        chat.older_done = done;
        if before.is_some() {
            chat.older_before = before;
        }
        let known: BTreeSet<String> = chat.lines.iter().filter_map(|l| l.id.clone()).collect();
        let mut older: Vec<Line> = lines
            .into_iter()
            .filter(|l| l.id.as_ref().is_none_or(|id| !known.contains(id)))
            .collect();
        older.append(&mut chat.lines);
        chat.lines = older;
    }

    fn max_pane(&self) -> u16 {
        (self.areas.screen.width / 2).max(MIN_PANE)
    }

    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Vec<Action> {
        let (x, y) = (mouse.column, mouse.row);
        let inside = |r: Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;
        let inside_opt = |r: Option<Rect>| r.is_some_and(inside);
        if self.services.is_some() {
            return self.services_mouse(mouse);
        }
        if self.keys.is_some() {
            return self.keys_mouse(mouse);
        }
        if self.settings.is_some() {
            return self.settings_mouse(mouse);
        }
        // The search results: a click picks one, the wheel goes through them, a click
        // elsewhere closes them.
        if self.search.is_some() {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(&(_, index)) = self.areas.search_rows.iter().find(|(r, _)| inside(*r)) {
                        return self.jump(index);
                    }
                    self.search = None;
                }
                MouseEventKind::ScrollUp => {
                    return self.on_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
                }
                MouseEventKind::ScrollDown => {
                    return self.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
                }
                _ => return Vec::new(),
            }
        }
        // The reaction picker takes the next click: on a reaction, it goes; anywhere
        // else, the picker closes.
        if let Some(picker) = self.picker {
            return match mouse.kind {
                MouseEventKind::Down(_) => {
                    self.picker = None;
                    let chosen = self.areas.reaction_buttons.iter().find(|(r, _)| inside(*r));
                    match chosen.map(|&(_, reaction)| reaction) {
                        Some(reaction) => self.react_line(picker.chat, picker.line, reaction),
                        None => Vec::new(),
                    }
                }
                _ => Vec::new(),
            };
        }
        // Selecting text in the conversation: a drag from a place that is not a link.
        let on_link = self.areas.nicks.iter().any(|(r, _)| inside(*r))
            || self.areas.files.iter().any(|(r, _)| inside(*r))
            || self.areas.fetches.iter().any(|(r, _)| inside(*r))
            || inside_opt(self.areas.older);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if inside(self.areas.messages) && !on_link => {
                // A double click on a message answers it.
                let now = std::time::Instant::now();
                let double = self.last_click.is_some_and(|(at, cx, cy)| {
                    now.duration_since(at).as_millis() < 400 && cy == y && cx.abs_diff(x) <= 2
                });
                self.last_click = Some((now, x, y));
                if double
                    && let (Some(chat), Some(&(_, line))) = (
                        self.selected,
                        self.areas.message_lines.iter().find(|(r, _)| inside(*r)),
                    )
                {
                    self.selecting = false;
                    return self.reply_to(chat, line, None);
                }
                self.focus = Focus::Messages;
                self.selection = Some(((x, y), (x, y)));
                self.selecting = true;
                return Vec::new();
            }
            MouseEventKind::Drag(MouseButton::Left) if self.selecting => {
                if let Some((anchor, _)) = self.selection {
                    let messages = self.areas.messages;
                    let x = x.clamp(messages.x, messages.right().saturating_sub(1));
                    let y = y.clamp(messages.y, messages.bottom().saturating_sub(1));
                    self.selection = Some((anchor, (x, y)));
                }
                return Vec::new();
            }
            MouseEventKind::Up(MouseButton::Left) if self.selecting => {
                self.selecting = false;
                let text = self.selected_text();
                let moved = self.selection.is_some_and(|(a, b)| a != b);
                if !moved || text.trim().is_empty() {
                    self.selection = None;
                    return Vec::new();
                }
                return vec![Action::Copy(text)];
            }
            // The middle button on a chat closes it, as on a browser's tab.
            MouseEventKind::Down(MouseButton::Middle)
                if let Some(&(_, index)) = self.areas.chat_rows.iter().find(|(r, _)| inside(*r)) =>
            {
                return self.close_chat(index);
            }
            // Elsewhere it pastes what is selected anywhere, as everywhere.
            MouseEventKind::Down(MouseButton::Middle) => {
                self.focus = Focus::Input;
                return vec![Action::PastePrimary];
            }
            // A right click on a message offers reactions to it.
            MouseEventKind::Down(MouseButton::Right)
                if inside(self.areas.messages)
                    && let (Some(chat), Some(&(_, line))) = (
                        self.selected,
                        self.areas.message_lines.iter().find(|(r, _)| inside(*r)),
                    ) =>
            {
                self.selection = None;
                self.picker = Some(Picker { chat, line, x, y });
                return Vec::new();
            }
            MouseEventKind::Down(_) => self.selection = None,
            _ => {}
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A pane's inner border is its handle.
                if let Some(left) = self.areas.left
                    && x == left.x + left.width - 1
                    && inside(left)
                {
                    self.dragging = Some(Splitter::Left);
                    return Vec::new();
                }
                if let Some(right) = self.areas.right
                    && x == right.x
                    && inside(right)
                {
                    self.dragging = Some(Splitter::Right);
                    return Vec::new();
                }
                if inside_opt(self.areas.hide_left) {
                    self.show_left = false;
                } else if inside_opt(self.areas.hide_right) {
                    self.show_right = false;
                } else if inside_opt(self.areas.show_left) {
                    self.show_left = true;
                } else if inside_opt(self.areas.show_right) {
                    self.show_right = true;
                } else if let Some((_, nick)) = self.areas.nicks.iter().find(|(r, _)| inside(*r)) {
                    let nick = nick.clone();
                    return self.open_private(&nick);
                } else if let Some((_, person)) = self.areas.people.iter().find(|(r, _)| inside(*r)) {
                    let person = person.clone();
                    let Some(account) = self.current().map(|c| c.account.clone()) else {
                        return Vec::new();
                    };
                    let index = self.chat(&account, &person, ChatKind::Direct);
                    self.focus = Focus::Input;
                    return self.select(index);
                } else if inside_opt(self.areas.side_devices) {
                    return self.open_keys(None);
                } else if inside_opt(self.areas.older) {
                    return self.ask_older();
                } else if let Some((_, link)) = self.areas.fetches.iter().find(|(r, _)| inside(*r)) {
                    return vec![Action::Fetch(link.clone())];
                } else if let Some((_, path)) = self.areas.files.iter().find(|(r, _)| inside(*r)) {
                    return vec![Action::Open(path.clone())];
                } else if inside(self.areas.omemo_button) {
                    return self.toggle_omemo();
                } else if inside(self.areas.attach_button) {
                    // The path is typed (Tab helps), or a file dropped on the terminal.
                    self.input = "/upload ".into();
                    self.cursor = self.input.chars().count();
                    self.focus = Focus::Input;
                    self.status = tr!(
                        "a path to a file, or drop a file here",
                        "путь к файлу или перетащите файл сюда"
                    )
                    .into();
                } else if let Some((_, group)) = self.areas.group_rows.iter().find(|(r, _)| inside(*r)) {
                    let group = group.clone();
                    self.toggle_group(&group);
                    if let Group::Account(account) = group {
                        self.active = Some(account);
                    }
                } else if let Some(&(_, index)) = self.areas.chat_rows.iter().find(|(r, _)| inside(*r)) {
                    self.focus = Focus::Chats;
                    // It may be dragged on to a folder from here.
                    self.dragging_chat = Some(index);
                    return self.select(index);
                } else if inside(self.areas.input) {
                    self.focus = Focus::Input;
                } else if inside(self.areas.messages) {
                    self.focus = Focus::Messages;
                }
                Vec::new()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_chat.is_some() && self.dragging.is_none() {
                    self.status = tr!("let go on a folder", "отпустите на папке").into();
                }
                let max = self.max_pane();
                match self.dragging {
                    Some(Splitter::Left) => self.left_width = (x + 1).clamp(MIN_PANE, max),
                    Some(Splitter::Right) => {
                        self.right_width = self.areas.screen.width.saturating_sub(x).clamp(MIN_PANE, max)
                    }
                    None => {}
                }
                Vec::new()
            }
            MouseEventKind::Up(_) => {
                self.dragging = None;
                // A chat let go over a heading moves there: into a folder, or out of one
                // onto its account.
                let dropped = self.dragging_chat.take().and_then(|index| {
                    let (_, group) = self.areas.group_rows.iter().find(|(r, _)| inside(*r))?;
                    Some((index, group.clone()))
                });
                match dropped {
                    Some((index, group)) if group != self.group_of(index) => match group {
                        Group::Folder(account, name) if account == self.chats[index].account => {
                            self.move_to_folder(index, Some(&name))
                        }
                        Group::Account(account) if account == self.chats[index].account => {
                            self.move_to_folder(index, None)
                        }
                        _ => {
                            self.status = tr!(
                                "a chat of another account: each account has folders of its own",
                                "чат другого аккаунта: папки у каждого аккаунта свои"
                            )
                            .into();
                            Vec::new()
                        }
                    },
                    _ => Vec::new(),
                }
            }
            MouseEventKind::ScrollUp if inside(self.areas.messages) => self.scroll(3),
            MouseEventKind::ScrollDown if inside(self.areas.messages) => self.scroll(-3),
            MouseEventKind::ScrollUp if inside_opt(self.areas.left) => self.move_selection(-1),
            MouseEventKind::ScrollDown if inside_opt(self.areas.left) => self.move_selection(1),
            // A room's participants, as with the arrow keys: the pane follows the one picked.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if inside_opt(self.areas.right) && !self.room_people().is_empty() =>
            {
                self.focus = Focus::People;
                let code = if mouse.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                (0..3).flat_map(|_| self.people_key(code)).collect()
            }
            _ => Vec::new(),
        }
    }
}

/// Someone's status beside their name, if not plain online: "away: lunch".
pub fn others_status(status: &omtalk_xmpp::presence::OwnStatus) -> Option<String> {
    (status != &omtalk_xmpp::presence::OwnStatus::default()).then(|| status_words(status))
}

/// One's own status in words: "away: lunch".
pub fn status_words(status: &omtalk_xmpp::presence::OwnStatus) -> String {
    use xmpp_parsers::presence::Show;
    let show = match status.show {
        None => tr!("online", "в сети"),
        Some(Show::Away) => tr!("away", "отошёл"),
        Some(Show::Dnd) => tr!("do not disturb", "не беспокоить"),
        Some(Show::Xa) => tr!("away for long", "давно нет"),
        Some(Show::Chat) => tr!("free to chat", "готов поболтать"),
    };
    match &status.text {
        Some(text) => format!("{show}: {text}"),
        None => show.to_owned(),
    }
}

/// Whether `text` is an action, "/me waves" (XEP-0245), shown as "* nick waves".
pub fn is_action(text: &str) -> bool {
    text.strip_prefix("/me")
        .is_some_and(|rest| rest.starts_with([' ', '\n']) && !rest.trim().is_empty())
}

/// What an action says, after "/me "; `None` for a plain message.
pub fn action_of(text: &str) -> Option<&str> {
    is_action(text).then(|| text["/me".len()..].trim_start_matches([' ', '\n']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    fn app() -> App {
        let mut app = App::new(vec![jid("me@example.org")], Modifier::Ctrl, 26, 30);
        app.areas.screen = Rect::new(0, 0, 120, 40);
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: crossterm::event::KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn type_text(app: &mut App, text: &str) -> Vec<Action> {
        // A fresh line, as one types after putting a refused one aside.
        app.input.clear();
        app.cursor = 0;
        let mut actions = Vec::new();
        for c in text.chars() {
            actions.extend(app.on_key(key(KeyCode::Char(c))));
        }
        actions.extend(app.on_key(key(KeyCode::Enter)));
        // Typing states are the typing test's business.
        actions.retain(|a| !matches!(a, Action::ChatState { .. }));
        actions
    }

    #[test]
    fn typed_text_goes_to_the_open_chat_encrypted_unless_turned_off() {
        let mut app = app();
        type_text(&mut app, "/msg bob@example.org");
        assert_eq!(app.current().unwrap().jid, jid("bob@example.org"));

        let sent = type_text(&mut app, "привет, Боб");
        let [Action::Send { id, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        assert_eq!(
            sent,
            [Action::Send {
                account: jid("me@example.org"),
                to: jid("bob@example.org"),
                room: false,
                text: "привет, Боб".into(),
                encrypted: true,
                id: id.clone(),
                reply: None,
                replace: None,
            }]
        );
        // Shown at once, as being sent, under the same id.
        let line = app.current().unwrap().lines.last().unwrap();
        assert_eq!(
            (line.text.as_str(), line.id.as_ref(), &line.delivery),
            ("привет, Боб", Some(id), &Some(Delivery::Sending))
        );
        assert!(app.input.is_empty());

        app.on_key(ctrl('o'));
        let sent = type_text(&mut app, "открыто");
        assert!(
            matches!(&sent[..], [Action::Send { encrypted: false, .. }]),
            "the toggle reaches the next message"
        );
    }

    #[test]
    fn editing_works_on_characters_not_bytes() {
        let mut app = app();
        for c in "привт".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Char('е')));
        app.on_key(key(KeyCode::Home));
        app.on_key(key(KeyCode::Delete));
        assert_eq!(app.input, "ривет");
    }

    #[test]
    fn the_modifier_is_the_configured_one() {
        let mut app = App::new(vec![jid("me@example.org")], Modifier::Alt, 26, 30);
        app.on_key(ctrl('b'));
        assert!(
            app.show_left,
            "Ctrl+B is plain text input when the modifier is Alt"
        );
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT));
        assert!(!app.show_left);
    }

    #[test]
    fn commands_make_actions_and_mistakes_explain_themselves() {
        let mut app = app();
        let join = type_text(&mut app, "/join room@muc.example.org Алиса");
        assert_eq!(
            join,
            [Action::Join {
                account: jid("me@example.org"),
                room: jid("room@muc.example.org"),
                nick: Some("Алиса".into())
            }]
        );
        assert!(type_text(&mut app, "/join").is_empty());
        assert!(app.status.contains("нужен адрес"));
        assert!(type_text(&mut app, "/frobnicate").is_empty());
        assert!(app.status.contains("нет команды"));

        // `/msg JID text` opens the chat and sends the text there, also from a room.
        let sent = type_text(&mut app, "/msg  xff@example.org  привет, как ты ");
        assert!(
            sent.iter().any(
                |a| matches!(a, Action::Send { to, text, room: false, encrypted: true, .. }
                if *to == jid("xff@example.org") && text == "привет, как ты")
            ),
            "{sent:?}"
        );
        assert_eq!(app.current().unwrap().jid, jid("xff@example.org"));
        assert!(
            !type_text(&mut app, "/msg xff@example.org")
                .iter()
                .any(|a| matches!(a, Action::Send { .. }))
        );
    }

    #[test]
    fn edit_corrects_ones_last_message_in_place() {
        let mut app = app();
        type_text(&mut app, "/msg bob@example.org");
        let sent = type_text(&mut app, "встреча в пятт");
        let [Action::Send { id: first, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        let first = first.clone();

        type_text(&mut app, "/edit");
        assert_eq!(app.input, "встреча в пятт", "the old text, to change");
        assert!(app.editing.is_some());
        // The line as it should have been.
        app.input = "встреча в пять".into();
        app.cursor = app.input.chars().count();
        let sent = app.on_key(key(KeyCode::Enter));
        let sent: Vec<_> = sent
            .into_iter()
            .filter(|a| !matches!(a, Action::ChatState { .. }))
            .collect();
        let [Action::Send { text, replace, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        assert_eq!(
            (text.as_str(), replace.as_ref()),
            ("встреча в пять", Some(&first))
        );
        let lines = &app.current().unwrap().lines;
        let line = lines.last().unwrap();
        assert_eq!((line.text.as_str(), line.edited), ("встреча в пять", true));
        assert_eq!(
            lines.iter().filter(|l| l.outgoing).count(),
            1,
            "no second message"
        );
        assert!(app.editing.is_none());

        // Esc drops a correction, and its text with it.
        type_text(&mut app, "/edit");
        app.on_key(key(KeyCode::Esc));
        assert!(app.editing.is_none() && app.input.is_empty());
    }

    #[test]
    fn away_and_dnd_set_ones_status_with_words_and_online_clears_it() {
        use omtalk_xmpp::presence::OwnStatus;
        use xmpp_parsers::presence::Show;
        let mut app = app();
        let sent = type_text(&mut app, "/away обед до двух");
        let away = OwnStatus {
            show: Some(Show::Away),
            text: Some("обед до двух".into()),
        };
        assert_eq!(sent, [Action::SetStatus(away.clone())]);
        assert_eq!(app.own_status, away);
        assert_eq!(status_words(&app.own_status), "отошёл: обед до двух");
        assert!(matches!(
            &type_text(&mut app, "/dnd")[..],
            [Action::SetStatus(OwnStatus {
                show: Some(Show::Dnd),
                text: None
            })]
        ));
        assert_eq!(
            type_text(&mut app, "/online"),
            [Action::SetStatus(OwnStatus::default())]
        );
        assert_eq!(app.own_status, OwnStatus::default());
    }

    #[test]
    fn a_room_left_is_closed_here_too() {
        let mut app = app();
        let me = jid("me@example.org");
        let room = app.chat(
            &me,
            &jid("saved@muc.example.org"),
            ChatKind::Room { nick: "s".into() },
        );
        app.select(room);
        let actions = type_text(&mut app, "/leave");
        assert!(
            matches!(
                &actions[..],
                [
                    Action::CloseChat { .. },
                    Action::Leave { .. },
                    Action::NoAutojoin { .. }
                ]
            ),
            "{actions:?}"
        );
        let actions = app.close_chat(room);
        assert!(matches!(
            &actions[..],
            [Action::CloseChat { .. }, Action::Leave { .. }]
        ));
    }

    #[test]
    fn omemo_turned_off_in_a_chat_is_kept_for_it() {
        let mut app = app();
        let me = jid("me@example.org");
        let irc = jid("#chan%irc.example.org@irc.example.org");
        let room = app.chat(&me, &irc, ChatKind::Room { nick: "s".into() });
        app.select(room);
        assert!(app.chats[room].omemo, "on unless told");
        let kept = app.toggle_omemo();
        assert_eq!(
            kept,
            [Action::SetOmemo {
                account: me.clone(),
                chat: irc.clone(),
                on: false
            }]
        );
        // As after a restart: the choice read back, the chat made anew.
        let mut again = super::tests::app();
        again.omemo_choice.insert((me.clone(), irc.clone()), false);
        let room = again.chat(&me, &irc, ChatKind::Room { nick: "s".into() });
        assert!(!again.chats[room].omemo);
    }

    #[test]
    fn ones_message_back_from_a_gateway_under_another_id_is_not_shown_twice() {
        let mut app = app();
        let me = jid("me@example.org");
        let room = app.chat(
            &me,
            &jid("#chan%irc.example.org@irc.example.org"),
            ChatKind::Room { nick: "s[x]".into() },
        );
        app.select(room);
        let sent = type_text(&mut app, "привет, IRC");
        assert_eq!(sent.len(), 1);
        let back = Incoming {
            peer: jid("#chan%irc.example.org@irc.example.org"),
            sender: me.clone(),
            nick: Some("s[x]".into()),
            device: None,
            direction: Direction::Outgoing,
            encrypted: false,
            text: Some("привет, IRC".into()),
            at: None,
            not_for_this_device: false,
            id: Some("biboumi-made-1".into()),
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
            replaces: None,
            shared_file: false,
        };
        let now = chrono::Utc::now().timestamp();
        app.on_incoming(&me, &back, now);
        let own: Vec<_> = app.chats[room].lines.iter().filter(|l| !l.notice).collect();
        assert_eq!(own.len(), 1, "once");
        assert!(own[0].echoed);
        // The same words again, sent again: a second line, and its own copy back.
        type_text(&mut app, "привет, IRC");
        app.on_incoming(
            &me,
            &Incoming {
                id: Some("biboumi-made-2".into()),
                ..back
            },
            now,
        );
        assert_eq!(app.chats[room].lines.iter().filter(|l| !l.notice).count(), 2);
    }

    #[test]
    fn an_archive_fills_a_chat_in_order_and_only_its_newest_stay() {
        let mut app = app();
        let me = jid("me@example.org");
        let room = app.chat(
            &me,
            &jid("team@muc.example.org"),
            ChatKind::Room { nick: "me".into() },
        );
        let message = |text: String, at: Option<i64>| Incoming {
            peer: jid("team@muc.example.org"),
            sender: jid("bob@example.org"),
            nick: Some("bob".into()),
            device: None,
            direction: Direction::Incoming,
            encrypted: false,
            text: Some(text),
            at,
            not_for_this_device: false,
            id: None,
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
            replaces: None,
            shared_file: false,
        };
        // Live first, then the archive's older one: in its place by time.
        app.on_incoming(&me, &message("сейчас".into(), None), 1_000_000);
        app.on_incoming(&me, &message("утром".into(), Some(10)), 10);
        assert_eq!(app.chats[room].lines[0].text, "утром");

        app.chats[room].older_done = true;
        for i in 0..(SHOWN * 2) as i64 {
            app.on_incoming(&me, &message(format!("из архива {i}"), Some(100 + i)), 100 + i);
        }
        let lines = &app.chats[room].lines;
        assert!(lines.len() <= SHOWN + SHOWN / 2, "{}", lines.len());
        assert_eq!(lines.last().unwrap().text, "сейчас", "the newest stay");
        assert!(
            !app.chats[room].older_done,
            "the rest is to be had by scrolling up"
        );
    }

    #[test]
    fn reactions_can_be_named_as_people_write_them() {
        assert_eq!(reaction_of(":+1:"), "👍");
        assert_eq!(reaction_of("heart"), "❤️");
        assert_eq!(reaction_of(":JOY:"), "😂");
        assert_eq!(reaction_of("🦀"), "🦀", "an emoji as it is");
        assert_eq!(reaction_of(" ok "), "👍");
    }

    #[test]
    fn reactions_go_on_the_message_they_name_and_back_off_again() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.select(bob);
        let mut line = Line::message(0, "bob".into(), "обед?".into(), true, false);
        line.id = Some("b1".into());
        app.chats[bob].lines.push(line);

        let sent = type_text(&mut app, "/react 👍");
        assert_eq!(
            sent,
            [Action::React {
                account: me.clone(),
                chat: jid("bob@example.org"),
                room: false,
                id: "b1".into(),
                by: "me@example.org".into(),
                reactions: vec!["👍".into()],
            }]
        );
        let reactions = omtalk_xmpp::reactions::Reactions {
            chat: jid("bob@example.org"),
            room: false,
            id: "b1".into(),
            by: "bob@example.org".into(),
            reactions: vec!["👍".into(), "❤".into()],
        };
        assert!(app.reacted(&me, &reactions));
        assert_eq!(app.chats[bob].lines.last().unwrap().reactions.len(), 2);

        // Again: one's own taken back, all of them gone now.
        let sent = type_text(&mut app, "/react 👍");
        assert!(matches!(&sent[..], [Action::React { reactions, .. }] if reactions.is_empty()));
        let line = app.chats[bob].lines.last().unwrap();
        assert_eq!(line.reactions.len(), 1, "bob's stay");
        // Not to another chat's message, nor a room's.
        let elsewhere = omtalk_xmpp::reactions::Reactions {
            room: true,
            ..reactions
        };
        assert!(!app.reacted(&me, &elsewhere));
    }

    #[test]
    fn a_correction_changes_only_what_its_writer_wrote() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        let message = |text: &str, id: &str, replaces: Option<&str>, direction| Incoming {
            peer: jid("bob@example.org"),
            sender: jid("bob@example.org"),
            nick: None,
            device: Some(1),
            direction,
            encrypted: true,
            text: Some(text.into()),
            at: None,
            not_for_this_device: false,
            id: Some(id.into()),
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
            replaces: replaces.map(str::to_owned),
            shared_file: false,
        };
        app.on_incoming(&me, &message("опечтака", "b1", None, Direction::Incoming), 0);
        assert!(app.correct(&me, &message("опечатка", "b2", Some("b1"), Direction::Incoming)));
        let line = app.chats[bob].lines.last().unwrap();
        assert_eq!((line.text.as_str(), line.edited), ("опечатка", true));
        // Not one's own message from bob's side, nor a message that is not there.
        assert!(!app.correct(&me, &message("x", "b3", Some("b1"), Direction::Outgoing)));
        assert!(!app.correct(&me, &message("x", "b4", Some("nope"), Direction::Incoming)));
        assert_eq!(app.chats[bob].lines.last().unwrap().text, "опечатка");
    }

    #[test]
    fn unread_counts_only_other_chats_and_clears_on_opening() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        let carol = app.chat(&me, &jid("carol@example.org"), ChatKind::Direct);
        app.select(bob);
        let incoming = |from: &str| Incoming {
            peer: jid(from),
            sender: jid(from),
            nick: None,
            device: Some(1),
            direction: Direction::Incoming,
            encrypted: true,
            text: Some("hi".into()),
            at: None,
            not_for_this_device: false,
            id: None,
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
            replaces: None,
            shared_file: false,
        };
        app.on_incoming(&me, &incoming("bob@example.org"), 0);
        app.on_incoming(&me, &incoming("carol@example.org"), 0);
        app.on_incoming(&me, &incoming("carol@example.org"), 0);
        assert_eq!((app.chats[bob].unread, app.chats[carol].unread), (0, 2));
        app.select(carol);
        assert_eq!(app.chats[carol].unread, 0);
    }

    #[test]
    fn dragging_a_border_resizes_its_pane_within_limits() {
        let mut app = app();
        app.areas.left = Some(Rect::new(0, 0, 26, 39));
        let at = |kind, column| MouseEvent {
            kind,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(at(MouseEventKind::Down(MouseButton::Left), 25));
        assert_eq!(app.dragging, Some(Splitter::Left));
        app.on_mouse(at(MouseEventKind::Drag(MouseButton::Left), 40));
        assert_eq!(app.left_width, 41);
        app.on_mouse(at(MouseEventKind::Drag(MouseButton::Left), 2));
        assert_eq!(app.left_width, MIN_PANE, "never narrower than readable");
        app.on_mouse(at(MouseEventKind::Drag(MouseButton::Left), 110));
        assert_eq!(app.left_width, 60, "never more than half the screen");
        app.on_mouse(at(MouseEventKind::Up(MouseButton::Left), 110));
        assert_eq!(app.dragging, None);
    }

    #[test]
    fn clicking_the_omemo_button_toggles_the_chat() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.select(bob);
        app.areas.omemo_button = Rect::new(2, 36, 12, 1);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 36,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(click);
        assert!(!app.chats[bob].omemo);
        app.on_mouse(click);
        assert!(app.chats[bob].omemo);
    }

    #[test]
    fn a_link_alone_is_a_file_and_clicking_its_picture_opens_it() {
        let line = Line::message(0, "bob".into(), "aesgcm://h/p.png#3b78aa076c56fa93bab4bb7e75794bf3369293c49bee6dcd924132244217e9c10dfcb540a3a36418de5a95e2".into(), true, false);
        let slot = line.media.as_ref().unwrap();
        assert_eq!(
            (slot.name.as_str(), slot.image, &slot.state),
            ("p.png", true, &MediaState::Loading)
        );
        assert!(
            Line::message(0, "bob".into(), "просто текст".into(), true, false)
                .media
                .is_none()
        );

        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.push(bob, line);
        app.media_done("https://h/p.png", MediaState::Ready("/cache/p.png".into()));
        assert_eq!(
            app.chats[bob].lines[0].media.as_ref().unwrap().state,
            MediaState::Ready("/cache/p.png".into())
        );

        app.areas.files = vec![(Rect::new(30, 10, 40, 8), "/cache/p.png".into())];
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 35,
            row: 12,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(app.on_mouse(click), [Action::Open("/cache/p.png".into())]);
    }

    #[test]
    fn a_contact_is_called_by_its_name_in_the_roster() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        let room = app.chat(
            &me,
            &jid("team@muc.example.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.contact_names(
            &me,
            vec![
                (jid("bob@example.org"), Some("Боб".into())),
                (jid("team@muc.example.org"), Some("not a room's name".into())),
                (jid("carol@example.org"), Some("Кэрол".into())),
            ],
        );
        assert_eq!(app.chats[bob].title(), "Боб");
        assert_eq!(app.chats[room].title(), "team", "a room keeps its own");
        let carol = app.chat(&me, &jid("carol@example.org"), ChatKind::Direct);
        assert_eq!(app.chats[carol].title(), "Кэрол", "a chat opened later takes it");
        // Renamed to nothing: the address again; another account's roster changes nothing.
        app.contact_names(&me, vec![(jid("bob@example.org"), None)]);
        assert_eq!(app.chats[bob].title(), "bob");
        app.contact_names(
            &jid("work@example.org"),
            vec![(jid("carol@example.org"), Some("x".into()))],
        );
        assert_eq!(app.chats[carol].title(), "Кэрол");
    }

    #[test]
    fn a_contact_without_a_name_in_the_roster_goes_by_the_one_they_gave_themselves() {
        let mut app = app();
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.nickname(&me, &jid("bob@example.org"), Some("Бобби".into()));
        assert_eq!(app.chats[bob].title(), "Бобби");
        app.contact_names(&me, vec![(jid("bob@example.org"), Some("Боб".into()))]);
        assert_eq!(app.chats[bob].title(), "Боб", "the roster's comes first");
        app.contact_names(&me, vec![(jid("bob@example.org"), None)]);
        assert_eq!(app.chats[bob].title(), "Бобби", "and theirs again without it");
        app.nickname(&me, &jid("bob@example.org"), None);
        assert_eq!(app.chats[bob].title(), "bob");
    }

    #[test]
    fn ctrl_c_quits_only_when_pressed_twice() {
        let mut app = app();
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.on_key(ctrl_c).is_empty());
        assert!(!app.quit, "once: it asks");
        assert!(app.status.contains("Ctrl+C"), "{}", app.status);
        assert_eq!(app.on_key(ctrl_c), [Action::Quit]);
        assert!(app.quit);

        // Long after the first, it asks again.
        let mut app = super::tests::app();
        app.quit_asked = Some(std::time::Instant::now() - QUIT_AGAIN * 2);
        assert!(app.on_key(ctrl_c).is_empty());
        assert!(!app.quit);
    }

    #[test]
    fn typing_away_from_the_input_line_types_into_it() {
        let mut app = app();
        app.focus = Focus::Chats;
        type_keys(&mut app, "привет");
        assert_eq!(app.focus, Focus::Input, "the focus jumps to the line");
        assert_eq!(app.input, "привет", "with the letters typed");

        app.input.clear();
        app.cursor = 0;
        app.focus = Focus::Chats;
        type_keys(&mut app, "j");
        assert_eq!(
            (app.focus, app.input.as_str()),
            (Focus::Input, "j"),
            "any letter, the panes keep none"
        );
        app.input.clear();
        app.cursor = 0;
        app.focus = Focus::Chats;
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.focus, Focus::Chats, "the arrows stay the pane's");
        app.focus = Focus::Messages;
        type_keys(&mut app, "5");
        assert_eq!((app.focus, app.input.as_str()), (Focus::Input, "5"));
    }

    fn type_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }
}
