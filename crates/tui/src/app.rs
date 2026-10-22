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
    /// A message line, with its media slot filled in if the text is a shared file.
    pub fn message(at: i64, who: String, text: String, encrypted: bool, outgoing: bool) -> Self {
        let media = omtalk_media::Media::from_text(&text).map(|media| MediaSlot {
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
    /// Room occupants by nick, with the real account when the room shows it.
    pub occupants: BTreeMap<String, Option<BareJid>>,
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
            occupants: BTreeMap::new(),
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
            return "сервер".to_owned();
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
    pub key_buttons: Vec<(Rect, crate::keys::KeyButton)>,
    /// People in the pane beside a room who are away: a click opens a chat with them.
    pub people: Vec<(Rect, BareJid)>,
    /// The devices in the pane beside a direct chat: a click opens the keys window.
    pub side_devices: Option<Rect>,
    /// The services window and what is clickable in it.
    pub service_window: Rect,
    pub service_rows: Vec<(Rect, usize)>,
    pub service_fields: Vec<(Rect, usize)>,
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
    /// Read who belongs to a room, present or not.
    Members {
        account: BareJid,
        room: BareJid,
    },
    /// The keys window asks something of OMEMO.
    Keys(crate::keys::Request),
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

pub const LEADER_KEYS: &[(&str, &str)] = &[
    ("o", "OMEMO в этом чате вкл/выкл"),
    ("s", "сервисы сервера"),
    ("b", "список чатов скрыть/показать"),
    ("r", "правую панель скрыть/показать"),
    ("a", "ответить (на выделенное или последнее)"),
    ("e", "ключи OMEMO: отпечатки, доверие"),
    ("w", "закрыть чат (комнату — покинуть)"),
    ("z", "только текущий аккаунт / все"),
    ("m", "мышь: omtalk / терминал (выделение tmux)"),
    ("↑ ↓ / k j", "предыдущий / следующий чат"),
    ("← →", "уже / шире список чатов"),
    ("q", "выход"),
    ("Esc", "отмена"),
];

pub struct App {
    pub accounts: Vec<BareJid>,
    /// Account → connection state, as shown in the status line.
    pub connection: BTreeMap<BareJid, String>,
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
    pub quit: bool,
}

const MIN_PANE: u16 = 12;

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
                .map(|a| (a.clone(), "подключение…".to_owned()))
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
            icons: crate::theme::NERD,
            status: String::new(),
            areas: Areas::default(),
            dragging: None,
            dragging_chat: None,
            services: None,
            keys: None,
            completion: None,
            replying: None,
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
        }
    }

    pub fn current(&self) -> Option<&Chat> {
        self.selected.and_then(|i| self.chats.get(i))
    }

    fn current_mut(&mut self) -> Option<&mut Chat> {
        self.selected.and_then(|i| self.chats.get_mut(i))
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
            if let ChatKind::Room { .. } = kind {
                self.chats[index].kind = kind;
            }
            return index;
        }
        self.chats.push(Chat::new(account.clone(), jid.clone(), kind));
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
                self.status = "служебный канал не закрывается".into();
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
            self.status = "Enter — отправить этот файл".into();
            return Vec::new();
        }
        let Some(chat) = self.current().filter(|c| c.kind != ChatKind::Server) else {
            return Vec::new();
        };
        let key = (chat.account.clone(), chat.jid.clone());
        let room = chat.is_room();
        let typing = !self.input.trim().is_empty() && !self.input.trim_start().starts_with('/');
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
        let own = matches!(&chat.kind, ChatKind::Room { nick } if nick == old);
        if own {
            chat.kind = ChatKind::Room { nick: new.to_owned() };
        }
        let text = if own {
            format!("теперь вы {new}")
        } else {
            format!("{old} теперь {new}")
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
                format!("{by} убрал(а) тему")
            } else {
                format!("{by} сменил(а) тему: {topic}")
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
            Some(_) if !known => Some(format!("{} ({}) подключился", online.client, online.resource)),
            None if known => Some(format!("{} ({}) отключился", online.client, online.resource)),
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
            self.server_log(account, format!("ваш {line}"), at);
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
        self.connection.get(account).is_some_and(|s| s == "в сети")
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
        let mut line = Line::message(at, "я".into(), text.to_owned(), encrypted, true);
        line.id = Some(id.to_owned());
        line.delivery = Some(Delivery::Sending);
        self.push(index, line);
    }

    /// How far one's message `id` got. A step back is ignored (a late receipt after the
    /// read marker), a failure always shows.
    pub fn delivery(&mut self, account: &BareJid, id: &str, state: Delivery) {
        for chat in self.chats.iter_mut().filter(|c| &c.account == account) {
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
            && self.chats[index]
                .lines
                .iter()
                .any(|l| l.outgoing && l.id.as_ref() == Some(id))
        {
            self.delivery(account, &id.clone(), Delivery::Sent);
            return Vec::new();
        }
        let who = match (&message.nick, message.direction) {
            (Some(nick), _) => nick.clone(),
            (None, Direction::Outgoing) => "я".to_owned(),
            (None, Direction::Incoming) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
        };
        let mut line = Line::message(
            at,
            who,
            text.clone(),
            message.encrypted,
            message.direction == Direction::Outgoing,
        );
        line.untrusted = message.untrusted;
        line.reply_to = message.reply_to.clone();
        line.archive_id = message.archive_id.clone();
        if message.direction == Direction::Incoming {
            line.id = message.id.clone();
        }
        self.push(index, line);
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
            (None, Direction::Outgoing) => "ваше другое устройство".to_owned(),
            (None, Direction::Incoming) => message.sender.to_string(),
        };
        let text = format!("{who}: сообщение не для этого устройства — его клиент не доверяет omtalk");
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
        // Ctrl+C always leaves, whatever the modifier, as in any terminal program.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![Action::Quit];
        }
        if let Some(services) = self.services.as_mut() {
            let outcome = services.on_key(key);
            return self.services_outcome(outcome);
        }
        if let Some(keys) = self.keys.as_mut() {
            let outcome = keys.on_key(key);
            return self.keys_outcome(outcome);
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
        match key.code {
            // In a line being typed, Tab completes; elsewhere it moves the focus.
            KeyCode::Tab | KeyCode::BackTab if self.focus == Focus::Input && !self.input.is_empty() => {
                self.complete(key.code == KeyCode::Tab);
                Vec::new()
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Chats => Focus::Messages,
                    Focus::Messages => Focus::Input,
                    Focus::Input => Focus::Chats,
                };
                Vec::new()
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Chats => Focus::Input,
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
            },
        }
    }

    fn shortcut(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Char('q') => {
                self.quit = true;
                vec![Action::Quit]
            }
            KeyCode::Char('o') => {
                self.toggle_omemo();
                Vec::new()
            }
            KeyCode::Char('s') => self.open_services(None),
            KeyCode::Char('e') => self.open_keys(None),
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
                    "мышь у omtalk".into()
                } else {
                    "мышь у терминала (префикс+m — вернуть)".into()
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

    fn chats_key(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Enter => {
                self.focus = Focus::Input;
                Vec::new()
            }
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(group) = self.selected.map(|i| self.group_of(i)) {
                    self.toggle_group(&group);
                }
                Vec::new()
            }
            KeyCode::Char('z') => {
                self.focus_group();
                Vec::new()
            }
            // Into a folder: the command, ready to be given a name.
            KeyCode::Char('m') => {
                self.input = "/folder ".into();
                self.cursor = self.input.chars().count();
                self.focus = Focus::Input;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn messages_key(&mut self, code: KeyCode) -> Vec<Action> {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(-1),
            KeyCode::Char('g') | KeyCode::Home => self.scroll(isize::MAX / 2),
            KeyCode::Char('G') | KeyCode::End => self.scroll(isize::MIN / 2),
            KeyCode::Enter | KeyCode::Char('i') => {
                self.focus = Focus::Input;
                Vec::new()
            }
            KeyCode::Char('r') => self.reply_here(),
            _ => Vec::new(),
        }
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
            None => self.say("не на что отвечать"),
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
        if input.trim().is_empty() {
            return Vec::new();
        }
        if input.trim_start().starts_with('/') {
            return self.command(input.trim());
        }
        // A pasted block keeps its indentation; only empty lines around it go.
        let text = input.trim_end().trim_start_matches(['\n', '\r']).to_owned();
        let Some(index) = self.selected else {
            self.status = "нет открытого чата: /msg JID или /join КОМНАТА".into();
            return Vec::new();
        };
        self.send_to(index, text)
    }

    /// Sends the file at `path` to chat `index`: its line shows at once, going up.
    pub fn upload(&mut self, index: usize, path: PathBuf) -> Vec<Action> {
        let chat = &self.chats[index];
        if chat.kind == ChatKind::Server {
            self.status = "сюда файлы не отправить".into();
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
        let mut line = Line::message(chrono::Utc::now().timestamp(), "я".into(), name, encrypted, true);
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
                let mut fresh =
                    Line::message(line.at, line.who.clone(), link.to_owned(), line.encrypted, true);
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
            self.status = "здесь только команды".into();
            return Vec::new();
        }
        chat.scroll = 0;
        let id = omtalk_xmpp::receipts::new_id();
        // The message itself says "active" now.
        self.typing_out = None;
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
            ("/leave", _) => match self.current() {
                Some(Chat {
                    jid,
                    kind: ChatKind::Room { nick },
                    account,
                    ..
                }) => Ok(vec![Action::Leave {
                    account: account.clone(),
                    room: jid.clone(),
                    nick: nick.clone(),
                }]),
                _ => Err("это не комната".to_owned()),
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
                _ => Err("откройте личный чат".to_owned()),
            },
            ("/services", _) => match args.first().map(|a| a.parse::<Jid>()) {
                Some(Err(_)) => Err("/services: нужен адрес сервиса, например example.org".to_owned()),
                address => return self.open_services(address.and_then(Result::ok)),
            },
            ("/folder", _) => match self.selected {
                Some(index) => {
                    let name = line[name.len()..].trim();
                    return self.move_to_folder(index, (!name.is_empty()).then_some(name));
                }
                None => Err("откройте чат, который положить в папку".to_owned()),
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
                    _ => Err("/rename-folder СТАРАЯ НОВАЯ".to_owned()),
                }
            }
            // The list shows while a command is being typed: start one.
            ("/upload", _) => {
                let path = line[name.len()..].trim();
                match (self.selected, expand_home(path)) {
                    (None, _) => Err("нет открытого чата".to_owned()),
                    (_, None) => Err("/upload ПУТЬ".to_owned()),
                    (Some(_), Some(file)) if !file.is_file() => {
                        Err(format!("нет такого файла: {}", file.display()))
                    }
                    (Some(index), Some(file)) => return self.upload(index, file),
                }
            }
            ("/keys", _) => {
                let peer = match args.first().map(|a| a.parse::<BareJid>()) {
                    Some(Ok(peer)) => Some(peer),
                    Some(Err(_)) => return self.say("/keys [АДРЕС]"),
                    None => None,
                };
                return self.open_keys(peer);
            }
            ("/reply", _) => return self.reply_here(),
            ("/nick", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let nick = line[name.len()..].trim().to_owned();
                    if nick.is_empty() {
                        return self.say("/nick НИК — сменить свой ник в этой комнате");
                    }
                    Ok(vec![Action::Nick {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        nick,
                    }])
                }
                None => Err("это не комната".to_owned()),
            },
            ("/topic", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let topic = line[name.len()..].trim().to_owned();
                    if topic.is_empty() {
                        let now = chat.topic.clone().unwrap_or_else(|| "темы нет".into());
                        return self.say(&format!("тема: {now} · /topic ТЕКСТ — сменить"));
                    }
                    Ok(vec![Action::SetTopic {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        topic,
                    }])
                }
                None => Err("это не комната".to_owned()),
            },
            ("/name", _) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => {
                    let new = line[name.len()..].trim().to_owned();
                    if new.is_empty() {
                        return self.say("/name НАЗВАНИЕ — сменить название комнаты (только владелец)");
                    }
                    Ok(vec![Action::SetRoomName {
                        account: chat.account.clone(),
                        room: chat.jid.clone(),
                        name: new,
                    }])
                }
                None => Err("это не комната".to_owned()),
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
                    return self.say("/create ИМЯ [НАЗВАНИЕ] — новая закрытая комната; ИМЯ или имя@сервис");
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
                    None => Err(format!("{address}: не годится для адреса комнаты")),
                }
            }
            ("/invite", Some(jid)) => match self.current().filter(|c| c.is_room()) {
                Some(chat) => Ok(vec![Action::Invite {
                    account: chat.account.clone(),
                    room: chat.jid.clone(),
                    jid,
                }]),
                None => Err("откройте комнату, в которую позвать".to_owned()),
            },
            ("/close", _) => match self.selected {
                Some(index) => return self.close_chat(index),
                None => Err("нет открытого чата".to_owned()),
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
            ("/msg" | "/join" | "/add" | "/accept" | "/refuse" | "/invite", None) => {
                Err(format!("{name}: нужен адрес вида name@server"))
            }
            _ => Err(format!("нет команды {name}")),
        };
        match result {
            Ok(actions) => actions,
            Err(message) => {
                self.status = message;
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
            self.status = "Enter — отправить этот файл".into();
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
                self.status = "это вы".into();
                Vec::new()
            }
            Some(Some(real)) => {
                let real = real.clone();
                let index = self.chat(&account, &real, ChatKind::Direct);
                self.focus = Focus::Input;
                self.select(index)
            }
            Some(None) => {
                self.status = format!("{nick}: комната не показывает адрес участника");
                Vec::new()
            }
            None => {
                self.status = format!("{nick} сейчас не в комнате, адрес неизвестен");
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
                (None, _, Some(field)) => services.click_field(field),
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

    pub fn toggle_omemo(&mut self) {
        if let Some(chat) = self.current_mut() {
            chat.omemo = !chat.omemo;
        }
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
            self.status = "служебный канал не кладётся в папку".into();
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
            return Err("нет аккаунта".to_owned());
        };
        if self.folders.iter().any(|f| f.account == account && f.name == new) {
            return Err(format!("папка «{new}» уже есть"));
        }
        let Some(folder) = self
            .folders
            .iter_mut()
            .find(|f| f.account == account && f.name == old)
        else {
            return Err(format!("в {account} нет папки «{old}»"));
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
        if let Some(chat) = self.current_mut() {
            chat.scroll = chat.scroll.saturating_add_signed(by);
        }
        Vec::new()
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
        // Selecting text in the conversation: a drag from a place that is not a link.
        let on_link = self.areas.nicks.iter().any(|(r, _)| inside(*r))
            || self.areas.files.iter().any(|(r, _)| inside(*r))
            || self.areas.fetches.iter().any(|(r, _)| inside(*r));
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
                } else if let Some((_, link)) = self.areas.fetches.iter().find(|(r, _)| inside(*r)) {
                    return vec![Action::Fetch(link.clone())];
                } else if let Some((_, path)) = self.areas.files.iter().find(|(r, _)| inside(*r)) {
                    return vec![Action::Open(path.clone())];
                } else if inside(self.areas.omemo_button) {
                    self.toggle_omemo();
                } else if inside(self.areas.attach_button) {
                    // The path is typed (Tab helps), or a file dropped on the terminal.
                    self.input = "/upload ".into();
                    self.cursor = self.input.chars().count();
                    self.focus = Focus::Input;
                    self.status = "путь к файлу или перетащите файл сюда".into();
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
                    self.status = "отпустите на папке".into();
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
                            self.status = "чат другого аккаунта: папки у каждого аккаунта свои".into();
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
            _ => Vec::new(),
        }
    }
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
}
