//! What the interface shows and how input changes it. No terminal and no network here:
//! keys and mouse turn into changes of this model and into [`Action`]s, which the
//! runtime carries out. That keeps all of it testable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use omtalk_omemo::{Direction, Incoming};
use omtalk_xmpp::services::CommandAction;
use omtalk_xmpp::{BareJid, Jid};
use ratatui::layout::Rect;

use crate::config::Modifier;
use crate::services::{Outcome, Services};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatKind {
    Direct,
    Room { nick: String },
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
    Loading,
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
            media,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub device: u32,
    pub fingerprint: Option<String>,
    pub has_session: bool,
    pub this_device: bool,
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
    /// The peer's devices; `None` until read.
    pub devices: Option<Vec<DeviceRow>>,
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
            devices: None,
        }
    }

    pub fn is_room(&self) -> bool {
        matches!(self.kind, ChatKind::Room { .. })
    }

    pub fn title(&self) -> String {
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
    pub hide_left: Option<Rect>,
    pub hide_right: Option<Rect>,
    /// Thin strips that bring a hidden pane back.
    pub show_left: Option<Rect>,
    pub show_right: Option<Rect>,
    pub chat_rows: Vec<(Rect, usize)>,
    /// Pictures and file lines, to open with a click.
    pub files: Vec<(Rect, PathBuf)>,
    /// Senders' names in a room and its participants: a click opens a chat with them.
    pub nicks: Vec<(Rect, String)>,
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
    },
    Join {
        account: BareJid,
        room: BareJid,
        nick: String,
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
    /// Show a fetched file in the outside viewer.
    Open(PathBuf),
    Quit,
}

pub const HELP: &str = "/msg JID — открыть чат · /join КОМНАТА [НИК] · /leave · /add JID [ИМЯ] · \
/accept JID · /refuse JID · /devices · /services [АДРЕС] · /help";

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
    /// The services window, over the chat while open.
    pub services: Option<Services>,
    pub quit: bool,
}

const MIN_PANE: u16 = 12;

impl App {
    pub fn new(accounts: Vec<BareJid>, modifier: Modifier, left: u16, right: u16) -> Self {
        let modifier = match modifier {
            Modifier::Ctrl => KeyModifiers::CONTROL,
            Modifier::Alt => KeyModifiers::ALT,
            Modifier::Super => KeyModifiers::SUPER,
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
            services: None,
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

    pub fn select(&mut self, index: usize) -> Vec<Action> {
        if index >= self.chats.len() {
            return Vec::new();
        }
        self.selected = Some(index);
        let chat = &mut self.chats[index];
        chat.unread = 0;
        // A direct chat's right pane lists the peer's devices; read them afresh each time
        // it is opened, since devices come and go.
        if chat.is_room() {
            return Vec::new();
        }
        vec![Action::Devices {
            account: chat.account.clone(),
            jid: chat.jid.clone(),
        }]
    }

    pub fn push(&mut self, index: usize, line: Line) {
        let selected = self.selected == Some(index);
        let chat = &mut self.chats[index];
        if !selected && !line.notice && !line.outgoing {
            chat.unread += 1;
        }
        chat.lines.push(line);
    }

    pub fn on_incoming(&mut self, account: &BareJid, message: &Incoming, at: i64) {
        let Some(text) = &message.text else { return };
        let kind = match &message.nick {
            Some(_) => ChatKind::Room { nick: String::new() },
            None => ChatKind::Direct,
        };
        let index = self.chat(account, &message.peer, kind);
        let who = match (&message.nick, message.direction) {
            (Some(nick), _) => nick.clone(),
            (None, Direction::Outgoing) => "я".to_owned(),
            (None, Direction::Incoming) => message.sender.node().map(|n| n.to_string()).unwrap_or_default(),
        };
        let line = Line::message(
            at,
            who,
            text.clone(),
            message.encrypted,
            message.direction == Direction::Outgoing,
        );
        self.push(index, line);
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
                    media: None,
                },
            ),
            _ => self.status = text,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        // Ctrl+C always leaves, whatever the modifier, as in any terminal program.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return vec![Action::Quit];
        }
        if let Some(services) = self.services.as_mut() {
            let outcome = services.on_key(key);
            return self.services_outcome(outcome);
        }
        if key.modifiers.contains(self.modifier) {
            return self.shortcut(key.code);
        }
        match key.code {
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
            _ => Vec::new(),
        }
    }

    fn input_key(&mut self, code: KeyCode) -> Vec<Action> {
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
            KeyCode::Esc => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Enter => return self.submit(),
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
        let text = std::mem::take(&mut self.input).trim().to_owned();
        self.cursor = 0;
        if text.is_empty() {
            return Vec::new();
        }
        if text.starts_with('/') {
            return self.command(&text);
        }
        let Some(chat) = self.current_mut() else {
            self.status = "нет открытого чата: /msg JID или /join КОМНАТА".into();
            return Vec::new();
        };
        chat.scroll = 0;
        vec![Action::Send {
            account: chat.account.clone(),
            to: chat.jid.clone(),
            room: chat.is_room(),
            text,
            encrypted: chat.omemo,
        }]
    }

    fn command(&mut self, line: &str) -> Vec<Action> {
        let mut words = line.split_whitespace();
        let name = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        let account = self
            .current()
            .map(|c| c.account.clone())
            .or_else(|| self.accounts.first().cloned());
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
                    let chat = &self.chats[index];
                    actions.push(Action::Send {
                        account: chat.account.clone(),
                        to: chat.jid.clone(),
                        room: false,
                        text: text.to_owned(),
                        encrypted: chat.omemo,
                    });
                }
                return actions;
            }
            ("/join", Some(room)) => {
                let nick = args
                    .get(1)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| account.node().map(|n| n.to_string()).unwrap_or_default());
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
            ("/help", _) => Err(HELP.to_owned()),
            ("/msg" | "/join" | "/add" | "/accept" | "/refuse", None) => {
                Err(format!("{name}: нужен адрес вида name@server"))
            }
            _ => Err(format!("неизвестная команда {name}; {HELP}")),
        };
        match result {
            Ok(actions) => actions,
            Err(message) => {
                self.status = message;
                Vec::new()
            }
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

    /// Opens the services window on `jid`, or on the current account's own server.
    pub fn open_services(&mut self, jid: Option<Jid>) -> Vec<Action> {
        let account = self
            .current()
            .map(|c| c.account.clone())
            .or_else(|| self.accounts.first().cloned());
        let Some(account) = account else { return Vec::new() };
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

    fn move_selection(&mut self, by: isize) -> Vec<Action> {
        if self.chats.is_empty() {
            return Vec::new();
        }
        let len = self.chats.len() as isize;
        let next = match self.selected {
            Some(i) => (i as isize + by).rem_euclid(len),
            None => 0,
        };
        self.select(next as usize)
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
                } else if let Some((_, path)) = self.areas.files.iter().find(|(r, _)| inside(*r)) {
                    return vec![Action::Open(path.clone())];
                } else if inside(self.areas.omemo_button) {
                    self.toggle_omemo();
                } else if let Some(&(_, index)) = self.areas.chat_rows.iter().find(|(r, _)| inside(*r)) {
                    self.focus = Focus::Chats;
                    return self.select(index);
                } else if inside(self.areas.input) {
                    self.focus = Focus::Input;
                } else if inside(self.areas.messages) {
                    self.focus = Focus::Messages;
                }
                Vec::new()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
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
                Vec::new()
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
        actions
    }

    #[test]
    fn typed_text_goes_to_the_open_chat_encrypted_unless_turned_off() {
        let mut app = app();
        type_text(&mut app, "/msg bob@example.org");
        assert_eq!(app.current().unwrap().jid, jid("bob@example.org"));

        let sent = type_text(&mut app, "привет, Боб");
        assert_eq!(
            sent,
            [Action::Send {
                account: jid("me@example.org"),
                to: jid("bob@example.org"),
                room: false,
                text: "привет, Боб".into(),
                encrypted: true
            }]
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
                nick: "Алиса".into()
            }]
        );
        assert!(type_text(&mut app, "/join").is_empty());
        assert!(app.status.contains("нужен адрес"));
        assert!(type_text(&mut app, "/frobnicate").is_empty());
        assert!(app.status.contains("неизвестная команда"));

        // `/msg JID text` opens the chat and sends the text there, also from a room.
        let sent = type_text(&mut app, "/msg  xff@example.org  привет, как ты ");
        assert!(
            sent.contains(&Action::Send {
                account: jid("me@example.org"),
                to: jid("xff@example.org"),
                room: false,
                text: "привет, как ты".into(),
                encrypted: true,
            }),
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
