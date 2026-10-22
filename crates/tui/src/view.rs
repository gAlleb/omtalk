//! Draws the model, and remembers where everything went, for the mouse.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{Local, TimeZone};
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui_image::StatefulImage;
use ratatui_image::protocol::StatefulProtocol;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use omtalk_xmpp::BareJid;
use omtalk_xmpp::services::{CommandAction, CommandStatus, NoteKind};
use xmpp_parsers::data_forms::FieldType;

use crate::app::{
    App, Areas, Chat, ChatKind, Delivery, Entry, Focus, Group, LEADER_KEYS, Line, MediaState, ServiceButton,
};
use crate::complete;
use crate::services::FieldState;
use crate::theme;

/// Narrower than this, the chat list folds away; the participants pane even earlier.
const FOLD_LEFT_BELOW: u16 = 50;
const FOLD_RIGHT_BELOW: u16 = 90;

/// Fetched pictures, by link, ready for the terminal's graphics.
pub type Pictures = HashMap<String, StatefulProtocol>;

/// Rows a picture takes in the conversation, and its widest.
const PICTURE_ROWS: usize = 10;
const PICTURE_WIDTH: u16 = 48;

pub fn draw(frame: &mut Frame, app: &mut App, pictures: &mut Pictures) {
    let area = frame.area();
    app.areas = Areas {
        screen: area,
        ..Areas::default()
    };
    frame.render_widget(Block::default().style(Style::default().fg(theme::TEXT)), area);

    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let show_left = app.show_left && area.width >= FOLD_LEFT_BELOW;
    let show_right = app.show_right && area.width >= FOLD_RIGHT_BELOW;
    let left_width = if show_left {
        app.left_width.min(area.width / 2)
    } else {
        1
    };
    let right_width = if show_right {
        app.right_width.min(area.width / 2)
    } else {
        1
    };
    let [left, center, right] = Layout::horizontal([
        Constraint::Length(left_width),
        Constraint::Min(20),
        Constraint::Length(right_width),
    ])
    .areas(main);

    if show_left {
        chats(frame, app, left);
    } else {
        strip(frame, left, "›");
        app.areas.show_left = Some(left);
    }
    center_pane(frame, app, center, pictures);
    if show_right {
        side(frame, app, right);
    } else {
        strip(frame, right, "‹");
        app.areas.show_right = Some(right);
    }
    status_line(frame, app, status);
    services_window(frame, app, area);
}

fn strip(frame: &mut Frame, area: Rect, arrow: &str) {
    let mid = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    frame.render_widget(Paragraph::new(arrow).style(Style::default().fg(theme::DIM)), mid);
}

fn pane(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ))
}

fn chats(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane("omtalk", app.focus == Focus::Chats).title(
        TextLine::from(Span::styled(" ‹ ", Style::default().fg(theme::DIM))).alignment(Alignment::Right),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.areas.left = Some(area);
    app.areas.hide_left = Some(Rect::new(area.x + area.width.saturating_sub(4), area.y, 3, 1));

    let mut y = inner.y;
    let rows = app.tree();
    // Whether the branch at each depth goes on below the current row.
    let mut open: Vec<bool> = Vec::new();
    for (at, (entry, depth)) in rows.iter().cloned().enumerate() {
        if y >= inner.bottom() {
            break;
        }
        // Each row hangs off the heading above it: ├ while siblings follow, └ for the
        // last, │ where a branch above goes on past it.
        let last = rows[at + 1..]
            .iter()
            .find(|(_, d)| *d <= depth)
            .is_none_or(|(_, d)| *d < depth);
        let mut branch = String::new();
        if depth > 0 {
            for level in 1..depth {
                branch.push_str(if open.get(level).copied().unwrap_or(false) {
                    "│ "
                } else {
                    "  "
                });
            }
            branch.push_str(if last { "└ " } else { "├ " });
        }
        open.resize(depth + 1, false);
        open[depth] = !last;
        let row = Rect::new(inner.x, y, inner.width, 1);
        match entry {
            Entry::Heading(group) => {
                let folded = app.collapsed.contains(&group);
                let paragraph = match &group {
                    Group::Account(account) => account_row(app, account, folded, inner.width),
                    Group::Folder(account, name) => {
                        folder_row(app, account, name, folded, inner.width, &branch)
                    }
                };
                frame.render_widget(paragraph, row);
                app.areas.group_rows.push((row, group.clone()));
            }
            Entry::Chat(index) => {
                frame.render_widget(
                    chat_row(
                        &app.chats[index],
                        app.selected == Some(index),
                        inner.width,
                        app.icons,
                        &branch,
                        !app.clients(&app.chats[index].account, &app.chats[index].jid)
                            .is_empty(),
                    ),
                    row,
                );
                app.areas.chat_rows.push((row, index));
            }
        }
        y += 1;
    }
    if app.chats.is_empty() && inner.height > 2 {
        let hint = Paragraph::new(vec![
            TextLine::from(Span::styled("нет чатов", Style::default().fg(theme::DIM))),
            TextLine::from(Span::styled("/msg или /join", Style::default().fg(theme::DIM))),
        ]);
        // Below the headings, if any.
        if y + 2 <= inner.bottom() {
            frame.render_widget(hint, Rect::new(inner.x + 1, y, inner.width.saturating_sub(1), 2));
        }
    }
}

/// An account's heading in the chat list: fold mark, state, address, and while folded
/// the unread count of its chats, so nothing hides there unnoticed.
fn account_row(app: &App, account: &BareJid, folded: bool, width: u16) -> Paragraph<'static> {
    let online = app.connection.get(account).is_some_and(|s| s == "в сети");
    let mark = if folded { "▸ " } else { "▾ " };
    let unread: usize = app
        .chats
        .iter()
        .filter(|c| &c.account == account)
        .map(|c| c.unread)
        .sum();
    let badge = if folded && unread > 0 {
        format!(" {unread} ")
    } else {
        String::new()
    };
    let room = (width as usize).saturating_sub(mark.width() + 2 + badge.width());
    let name = truncate(&account.to_string(), room);
    let gap = (width as usize).saturating_sub(mark.width() + 2 + name.width() + badge.width());
    Paragraph::new(TextLine::from(vec![
        Span::styled(mark, Style::default().fg(theme::DIM)),
        Span::styled(
            "● ",
            Style::default().fg(if online { theme::OK } else { theme::DIM }),
        ),
        Span::styled(name, {
            // The account commands go to stands out.
            let style = Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD);
            if app.active.as_ref() == Some(account) {
                style.add_modifier(Modifier::UNDERLINED)
            } else {
                style
            }
        }),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        ),
    ]))
}

/// A folder's heading: fold mark, its symbol and name, and while folded its unread count.
fn folder_row(
    app: &App,
    account: &BareJid,
    name: &str,
    folded: bool,
    width: u16,
    branch: &str,
) -> Paragraph<'static> {
    let width = width.saturating_sub(branch.width() as u16);
    let mark = if folded { "▸ " } else { "▾ " };
    let icon = format!("{} ", app.icons.folder);
    let unread: usize = app
        .folders
        .iter()
        .find(|f| &f.account == account && f.name == name)
        .map(|f| {
            app.chats
                .iter()
                .filter(|c| c.account == f.account && f.chats.contains(&c.jid))
                .map(|c| c.unread)
                .sum()
        })
        .unwrap_or(0);
    let badge = if folded && unread > 0 {
        format!(" {unread} ")
    } else {
        String::new()
    };
    let room = (width as usize).saturating_sub(mark.width() + icon.width() + badge.width());
    let name = truncate(name, room);
    let gap = (width as usize).saturating_sub(mark.width() + icon.width() + name.width() + badge.width());
    Paragraph::new(TextLine::from(vec![
        Span::styled(branch.to_owned(), Style::default().fg(theme::BORDER)),
        Span::styled(mark, Style::default().fg(theme::DIM)),
        Span::styled(icon, Style::default().fg(theme::ACCENT)),
        Span::styled(
            name,
            Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD),
        ),
    ]))
}

fn chat_row(
    chat: &Chat,
    selected: bool,
    width: u16,
    icons: theme::Icons,
    branch: &str,
    online: bool,
) -> Paragraph<'static> {
    let width = width.saturating_sub(branch.width() as u16);
    let icon = match chat.kind {
        ChatKind::Room { .. } => "# ".to_owned(),
        ChatKind::Direct => "@ ".to_owned(),
        ChatKind::Server => format!("{} ", icons.service),
    };
    let badge = if chat.unread > 0 {
        format!(" {} ", chat.unread)
    } else {
        String::new()
    };
    let lock = if chat.omemo { "" } else { icons.chat_unlocked };
    let room_for_title = (width as usize).saturating_sub(icon.width() + badge.width() + lock.width() + 1);
    let title = truncate(&chat.title(), room_for_title);
    let gap = (width as usize).saturating_sub(icon.width() + title.width() + lock.width() + badge.width());
    let base = if selected {
        theme::selected()
    } else if chat.unread > 0 {
        Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT)
    };
    Paragraph::new(TextLine::from(vec![
        Span::styled(branch.to_owned(), Style::default().fg(theme::BORDER)),
        Span::styled(
            icon,
            if selected {
                base
            } else {
                // A person's mark is green while any of their clients is online.
                base.fg(match chat.kind {
                    ChatKind::Room { .. } => theme::ACCENT,
                    ChatKind::Direct if online => theme::OK,
                    _ => theme::DIM,
                })
            },
        ),
        Span::styled(title, base),
        // On the selected row the colours are swapped: a coloured lock would turn into a
        // coloured block there.
        Span::styled(lock, if selected { base } else { base.fg(theme::WARN) }),
        Span::styled(" ".repeat(gap), base),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
    ]))
}

fn center_pane(frame: &mut Frame, app: &mut App, area: Rect, pictures: &mut Pictures) {
    let input_height = input_height(app, area.width).min(area.height.saturating_sub(3).max(3));
    let [messages, input] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(input_height)]).areas(area);
    app.areas.messages = messages;
    app.areas.input = input;

    let (title, subtitle) = match app.current() {
        Some(chat) if chat.kind == ChatKind::Direct => {
            let clients: Vec<String> = app
                .clients(&chat.account, &chat.jid)
                .iter()
                .map(|c| c.client.clone())
                .collect();
            let seen = if clients.is_empty() {
                "не в сети".to_owned()
            } else {
                clients.join(", ")
            };
            (chat.title(), format!("{} · {seen}", chat.jid))
        }
        Some(chat) => (chat.title(), chat.jid.to_string()),
        None => ("omtalk".to_owned(), String::new()),
    };
    let scrolled = app.current().is_some_and(|c| c.scroll > 0);
    let mut block = pane(&title, app.focus == Focus::Messages).title(
        TextLine::from(Span::styled(
            format!(" {subtitle} "),
            Style::default().fg(theme::DIM),
        ))
        .alignment(Alignment::Right),
    );
    if scrolled {
        block = block.title_bottom(
            TextLine::from(Span::styled(
                " ↓ ещё ниже — End ",
                Style::default().fg(theme::ACCENT),
            ))
            .alignment(Alignment::Center),
        );
    }
    let inner = block.inner(messages);
    frame.render_widget(block, messages);

    match app.selected {
        Some(index) => {
            let width = inner.width as usize;
            let ready = |url: &str| pictures.contains_key(url);
            let rows = message_rows(&app.chats[index].lines, width, app.icons, &ready);
            let height = inner.height as usize;
            let max_scroll = rows.len().saturating_sub(height);
            let chat = &mut app.chats[index];
            chat.scroll = chat.scroll.min(max_scroll);
            let end = rows.len() - chat.scroll;
            let start = end.saturating_sub(height);
            // Newest at the bottom, like every chat; a short history sits low too.
            let shown = &rows[start..end];
            let top = inner.y + (height - shown.len()) as u16;
            let mut texts = Vec::with_capacity(shown.len());
            for (offset, row) in shown.iter().enumerate() {
                let y = top + offset as u16;
                let mut text = row.text.clone();
                let plain: String = row.text.spans.iter().map(|s| s.content.as_ref()).collect();
                app.areas
                    .message_rows
                    .push((Rect::new(inner.x, y, inner.width, 1), plain));
                if let Some(picture) = &row.picture {
                    // A picture is drawn whole from its first row, or not at all: when
                    // scrolled half out of sight, its first visible row names it.
                    let first_visible = picture.row == 0 || offset == 0;
                    let whole = picture.row == 0 && offset + PICTURE_ROWS <= shown.len();
                    match pictures.get_mut(&picture.url) {
                        Some(protocol) if whole => {
                            let width = PICTURE_WIDTH.min(inner.width.saturating_sub(2));
                            let place = Rect::new(inner.x + 2, y, width, PICTURE_ROWS as u16);
                            frame.render_stateful_widget(StatefulImage::default(), place, protocol);
                            if let Some(path) = &row.open {
                                app.areas.files.push((place, path.clone()));
                            }
                        }
                        _ if first_visible => {
                            text = TextLine::from(Span::styled(
                                format!("  {} — прокрутите, чтобы увидеть", picture.name),
                                Style::default().fg(theme::DIM),
                            ));
                        }
                        _ => {}
                    }
                } else if let Some((nick, width)) = &row.who {
                    app.areas
                        .nicks
                        .push((Rect::new(inner.x, y, *width, 1), nick.clone()));
                } else if let Some(path) = &row.open {
                    app.areas
                        .files
                        .push((Rect::new(inner.x, y, inner.width, 1), path.clone()));
                }
                texts.push(text);
            }
            frame.render_widget(
                Paragraph::new(texts),
                Rect {
                    y: top,
                    height: inner.bottom() - top,
                    ..inner
                },
            );
        }
        None => empty_state(frame, inner),
    }
    highlight_selection(frame, app);
    input_box(frame, app, input);
    suggestions(frame, app, input, messages);
    leader_keys(frame, app, input);
}

/// Connected clients, one a row: the program, its state, its own words.
fn client_rows(rows: &mut Vec<TextLine<'static>>, clients: &[&omtalk_xmpp::presence::Online], width: u16) {
    use omtalk_xmpp::presence::State;
    for client in clients {
        let state = client.state.unwrap_or(State::Online);
        let colour = match state {
            State::Chat | State::Online => theme::OK,
            State::Away | State::Xa => theme::ACCENT,
            State::Dnd => theme::WARN,
        };
        let mut words = state.words().to_owned();
        if let Some(status) = &client.status {
            words = format!("{words} · {status}");
        }
        let line = format!("{}  {words}", client.client);
        rows.push(TextLine::from(vec![
            Span::styled("● ", Style::default().fg(colour)),
            Span::raw(truncate(&line, (width as usize).saturating_sub(2))),
        ]));
    }
}

/// The selected text in the conversation, in the terminal's own colours swapped.
fn highlight_selection(frame: &mut Frame, app: &App) {
    let Some((a, b)) = app.selection else { return };
    let ((sx, sy), (ex, ey)) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let buffer = frame.buffer_mut();
    for (rect, _) in &app.areas.message_rows {
        if rect.y < sy || rect.y > ey {
            continue;
        }
        let from = if rect.y == sy { sx } else { rect.x };
        let to = if rect.y == ey {
            ex
        } else {
            rect.right().saturating_sub(1)
        };
        for x in from.max(rect.x)..=to.min(rect.right().saturating_sub(1)) {
            let cell = &mut buffer[(x, rect.y)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

/// The leader key's name, as "Ctrl+X".
pub fn leader_label(app: &App) -> String {
    let (code, modifiers) = app.leader;
    let mut label = String::new();
    for (flag, name) in [
        (KeyModifiers::CONTROL, "Ctrl+"),
        (KeyModifiers::ALT, "Alt+"),
        (KeyModifiers::SUPER, "Super+"),
    ] {
        if modifiers.contains(flag) {
            label.push_str(name);
        }
    }
    match code {
        crossterm::event::KeyCode::Char(' ') => label.push_str("Space"),
        crossterm::event::KeyCode::Char(c) => label.extend(c.to_uppercase()),
        crossterm::event::KeyCode::F(n) => label.push_str(&format!("F{n}")),
        other => label.push_str(&format!("{other:?}")),
    }
    label
}

/// After the leader key: what the next key does, as which-key in an editor.
fn leader_keys(frame: &mut Frame, app: &App, input: Rect) {
    if !app.leader_pending {
        return;
    }
    let left = LEADER_KEYS.iter().map(|(k, _)| k.width()).max().unwrap_or(0);
    let width = (LEADER_KEYS
        .iter()
        .map(|(_, a)| left + 3 + a.width())
        .max()
        .unwrap_or(0) as u16
        + 2)
    .min(input.width);
    let height = (LEADER_KEYS.len() as u16 + 2).min(input.y.saturating_sub(1));
    if height < 3 {
        return;
    }
    let area = Rect::new(
        input.x + input.width.saturating_sub(width),
        input.y.saturating_sub(height),
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .title(Span::styled(
            format!(" {} … ", leader_label(app)),
            Style::default().fg(theme::ACCENT),
        ));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let lines: Vec<TextLine> = LEADER_KEYS
        .iter()
        .map(|(key, about)| {
            TextLine::from(vec![
                Span::styled(
                    format!("{key:<left$}"),
                    Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   {about}"), Style::default().fg(theme::DIM)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Over the bottom of the chat, just above the input: the commands that fit what is
/// typed, or the candidates Tab goes through.
fn suggestions(frame: &mut Frame, app: &App, input: Rect, above: Rect) {
    let mut rows: Vec<(String, &str, bool)> = match &app.completion {
        Some(c) if c.candidates.len() > 1 => c
            .candidates
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let text = text.trim_end().trim_end_matches(':').to_owned();
                let about = complete::about(&text).map(|c| c.about).unwrap_or("");
                (text, about, i == c.index)
            })
            .collect(),
        _ => complete::commands_for(&app.input)
            .into_iter()
            .map(|c| {
                (
                    format!("{} {}", c.name, c.args).trim_end().to_owned(),
                    c.about,
                    false,
                )
            })
            .collect(),
    };
    if rows.is_empty() || app.focus != Focus::Input {
        return;
    }
    let height = (rows.len() as u16 + 2)
        .min(above.height.saturating_sub(1))
        .min(14);
    if height < 3 {
        return;
    }
    rows.truncate(height as usize - 2);
    let left = rows.iter().map(|(t, ..)| t.width()).max().unwrap_or(0);
    let wanted = rows
        .iter()
        .map(|(_, about, _)| left + 3 + about.width())
        .max()
        .unwrap_or(0) as u16
        + 2;
    let width = wanted.clamp(24, input.width);
    let area = Rect::new(input.x, input.y.saturating_sub(height), width, height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(true))
        .title(Span::styled(" Tab — дописать ", Style::default().fg(theme::DIM)));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let room = (inner.width as usize).saturating_sub(left + 3);
    let lines: Vec<TextLine> = rows
        .into_iter()
        .map(|(text, about, chosen)| {
            let style = if chosen {
                theme::selected()
            } else {
                Style::default().fg(theme::ACCENT)
            };
            TextLine::from(vec![
                Span::styled(format!("{text:<left$}"), style),
                Span::styled(
                    format!("   {}", truncate(about, room)),
                    Style::default().fg(theme::DIM),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn empty_state(frame: &mut Frame, area: Rect) {
    let lines = vec![
        TextLine::from(Span::styled(
            "omtalk",
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        )),
        TextLine::from(""),
        TextLine::from(Span::styled(
            "/msg имя@сервер — написать",
            Style::default().fg(theme::DIM),
        )),
        TextLine::from(Span::styled(
            "/join комната@conference.сервер — войти в группу",
            Style::default().fg(theme::DIM),
        )),
        TextLine::from(Span::styled(
            "/help — все команды",
            Style::default().fg(theme::DIM),
        )),
    ];
    let height = lines.len() as u16;
    let top = area.y + area.height.saturating_sub(height) / 2;
    frame.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        Rect {
            y: top,
            height: height.min(area.height),
            ..area
        },
    );
}

/// The chat as rows of exactly `width` cells at most: day separators, a header where a
/// new sender (or a pause) starts, wrapped text below it.
/// One row of the conversation as drawn.
#[derive(Debug, Clone)]
pub struct Row {
    pub text: TextLine<'static>,
    /// A fetched file a click on this row opens.
    pub open: Option<PathBuf>,
    pub picture: Option<PictureRow>,
    /// A sender's name heading this row, and its width: a click on it opens a chat.
    pub who: Option<(String, u16)>,
}

/// One of the rows a picture covers.
#[derive(Debug, Clone)]
pub struct PictureRow {
    pub url: String,
    pub name: String,
    /// Which of its rows, from the top.
    pub row: usize,
}

impl From<TextLine<'static>> for Row {
    fn from(text: TextLine<'static>) -> Self {
        Self {
            text,
            open: None,
            picture: None,
            who: None,
        }
    }
}

pub fn message_rows(
    lines: &[Line],
    width: usize,
    icons: theme::Icons,
    ready: &dyn Fn(&str) -> bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut last_day = None;
    let mut last_who: Option<(&str, bool, i64)> = None;
    let body_width = width.saturating_sub(2).max(1);
    for line in lines {
        let time = Local.timestamp_opt(line.at, 0).single();
        let day = time.map(|t| t.date_naive());
        if day != last_day {
            if let Some(t) = time {
                let label = format!(" {} ", t.format("%-d.%m.%Y"));
                let side = width.saturating_sub(label.width()) / 2;
                rows.push(
                    TextLine::from(Span::styled(
                        format!(
                            "{}{label}{}",
                            "─".repeat(side),
                            "─".repeat(width.saturating_sub(side + label.width()))
                        ),
                        Style::default().fg(theme::BORDER),
                    ))
                    .into(),
                );
            }
            last_day = day;
            last_who = None;
        }
        let clock = time.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
        if line.notice {
            for text in wrap(&line.text, width.saturating_sub(4).max(1)) {
                rows.push(
                    TextLine::from(Span::styled(
                        format!("  · {text}"),
                        Style::default().fg(theme::DIM).add_modifier(Modifier::ITALIC),
                    ))
                    .into(),
                );
            }
            last_who = None;
            continue;
        }
        let same_group = last_who.is_some_and(|(who, outgoing, at)| {
            who == line.who && outgoing == line.outgoing && line.at - at < 300
        });
        if !same_group {
            let who_style = if line.outgoing {
                Style::default().fg(theme::OUTGOING).add_modifier(Modifier::BOLD)
            } else {
                theme::nick(&line.who)
            };
            let mut header = Row::from(TextLine::from(vec![
                Span::styled(
                    if line.who.is_empty() {
                        "?".to_owned()
                    } else {
                        line.who.clone()
                    },
                    who_style,
                ),
                Span::styled(format!("  {clock}"), Style::default().fg(theme::DIM)),
            ]));
            if !line.outgoing && !line.who.is_empty() {
                header.who = Some((line.who.clone(), line.who.width() as u16));
            }
            rows.push(header);
        }
        last_who = Some((&line.who, line.outgoing, line.at));
        let marker = icons.plain_mark;
        if let Some(slot) = &line.media {
            let path = match &slot.state {
                MediaState::Ready(path) => Some(path.clone()),
                _ => None,
            };
            if slot.image && path.is_some() && ready(&slot.url) {
                for row in 0..PICTURE_ROWS {
                    rows.push(Row {
                        text: TextLine::from(""),
                        open: path.clone(),
                        picture: Some(PictureRow {
                            url: slot.url.clone(),
                            name: slot.name.clone(),
                            row,
                        }),
                        who: None,
                    });
                }
                push_delivery(&mut rows, line, body_width, true);
                continue;
            }
            let (icon, state, colour) = match &slot.state {
                MediaState::Loading => (
                    if slot.image { icons.picture } else { icons.file },
                    "загружается…".to_owned(),
                    theme::DIM,
                ),
                MediaState::Ready(_) if slot.image => {
                    (icons.picture, "открыть — клик".to_owned(), theme::ACCENT)
                }
                MediaState::Ready(_) => (icons.file, "открыть — клик".to_owned(), theme::ACCENT),
                MediaState::Failed(e) => (icons.file, format!("не загрузилось: {e}"), theme::WARN),
            };
            let mut spans = vec![Span::raw("  ")];
            if !line.encrypted {
                spans.push(Span::styled(marker, Style::default().fg(theme::WARN)));
            }
            spans.push(Span::styled(
                format!("{icon} {} ", slot.name),
                Style::default().fg(theme::TEXT).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(state, Style::default().fg(colour)));
            rows.push(Row {
                text: TextLine::from(spans),
                open: path,
                picture: None,
                who: None,
            });
            push_delivery(&mut rows, line, body_width, false);
            continue;
        }
        let mut first = true;
        let text_width = if line.encrypted {
            body_width
        } else {
            body_width.saturating_sub(marker.width()).max(1)
        };
        for text in wrap(&line.text, text_width) {
            let mut spans = vec![Span::raw("  ")];
            if !line.encrypted && first {
                spans.push(Span::styled(marker, Style::default().fg(theme::WARN)));
            } else if !line.encrypted {
                spans.push(Span::raw(" ".repeat(marker.width())));
            }
            spans.push(Span::styled(text, Style::default().fg(theme::TEXT)));
            rows.push(TextLine::from(spans).into());
            first = false;
        }
        push_delivery(&mut rows, line, body_width, false);
    }
    rows
}

/// How far one's message got, after its last word, or below where it does not fit (and
/// always below a picture, whose rows hold no text).
fn push_delivery(rows: &mut Vec<Row>, line: &Line, body_width: usize, below: bool) {
    let Some(delivery) = &line.delivery else { return };
    let (mark, style) = delivery_mark(delivery);
    let last = rows.last_mut().expect("a message has a row");
    if !below && last.text.width() + mark.width() < body_width + 2 {
        last.text.spans.push(Span::styled(format!(" {mark}"), style));
        return;
    }
    // A long one (a failure's reason) wraps like the message.
    for part in wrap(&mark, body_width.max(1)) {
        rows.push(TextLine::from(vec![Span::raw("  "), Span::styled(part, style)]).into());
    }
}

/// The sign for how far a message got: ⋯ being sent, ✓ on the server, ✓✓ delivered,
/// ✓✓ in the accent colour read, ✗ with the reason when it failed.
fn delivery_mark(delivery: &Delivery) -> (String, Style) {
    let dim = Style::default().fg(theme::DIM);
    match delivery {
        Delivery::Uploading(percent) => (format!("⇡ {percent}%"), dim),
        Delivery::Sending => ("⋯".into(), dim),
        Delivery::Sent => ("✓".into(), dim),
        Delivery::Delivered => ("✓✓".into(), dim),
        Delivery::Read => (
            "✓✓".into(),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Delivery::Failed(reason) => (
            format!("✗ не отправлено: {reason}"),
            Style::default().fg(theme::WARN),
        ),
    }
}

/// Greedy wrapping on display width: at spaces where possible, inside a word where not.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split(' ') {
            let word_width = word.width();
            let space = usize::from(!line.is_empty());
            if line_width + space + word_width <= width {
                if space == 1 {
                    line.push(' ');
                }
                line.push_str(word);
                line_width += space + word_width;
                continue;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                line_width = 0;
            }
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if line_width + w > width && !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                    line_width = 0;
                }
                line.push(c);
                line_width += w;
            }
        }
        out.push(line);
    }
    out
}

fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn input_box(frame: &mut Frame, app: &mut App, area: Rect) {
    let omemo = app.current().is_none_or(|c| c.omemo);
    let (label, colour) = if omemo {
        (app.icons.locked, theme::OK)
    } else {
        (app.icons.unlocked, theme::WARN)
    };
    let button = format!(" {label} ");
    let attach = format!(" {} ", app.icons.attach);
    // The hint as long as the border has room for beside the lock.
    let hint = [
        " Enter ⏎ · Alt+Enter — новая строка ",
        " ⏎ · Alt+⏎ строка ",
        " ⏎ ",
    ]
    .into_iter()
    .find(|h| button.width() + attach.width() + h.width() + 4 <= area.width as usize)
    .unwrap_or("");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(app.focus == Focus::Input))
        .title(Span::styled(
            button.clone(),
            Style::default().fg(colour).add_modifier(Modifier::BOLD),
        ))
        .title(Span::styled(attach.clone(), Style::default().fg(theme::ACCENT)))
        .title(
            TextLine::from(Span::styled(hint, Style::default().fg(theme::DIM))).alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    app.areas.omemo_button = Rect::new(area.x + 1, area.y, button.width() as u16, 1);
    app.areas.attach_button = Rect::new(
        area.x + 1 + button.width() as u16,
        area.y,
        attach.width() as u16,
        1,
    );

    if app.input.is_empty() {
        let hint = if app.current().is_some() {
            "Сообщение…   / — команды, Tab — дописать"
        } else {
            "/msg имя@сервер или /join комната"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(theme::DIM))),
            inner,
        );
    } else {
        let (rows, (row, col)) = input_rows(&app.input, app.cursor, inner.width as usize);
        // The rows around the cursor, when there are more than fit.
        let skip = (row + 1).saturating_sub(inner.height as usize);
        let shown: Vec<TextLine> = rows
            .into_iter()
            .skip(skip)
            .take(inner.height as usize)
            .map(TextLine::from)
            .collect();
        frame.render_widget(Paragraph::new(shown), inner);
        if app.focus == Focus::Input {
            frame.set_cursor_position(Position::new(inner.x + col as u16, inner.y + (row - skip) as u16));
        }
        return;
    }
    if app.focus == Focus::Input {
        frame.set_cursor_position(Position::new(inner.x, inner.y));
    }
}

/// How many rows the input box wants: its text wrapped, up to a limit.
pub fn input_height(app: &App, width: u16) -> u16 {
    let (rows, _) = input_rows(&app.input, app.cursor, width.saturating_sub(2) as usize);
    rows.len().clamp(1, INPUT_ROWS) as u16 + 2
}

/// At most this many rows of text in the input box; more scroll.
const INPUT_ROWS: usize = 8;

/// The input as rows of at most `width` cells (its own line breaks, and wrapping), and
/// where the cursor (in characters) is among them. The cursor gets a cell of its own at
/// the end of a full row.
pub fn input_rows(input: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(2) - 1;
    let mut rows = vec![String::new()];
    let mut place = (0, 0);
    let mut col = 0;
    for (index, c) in input.chars().enumerate() {
        if index == cursor {
            place = (rows.len() - 1, col);
        }
        if c == '\n' {
            rows.push(String::new());
            col = 0;
            continue;
        }
        let w = c.width().unwrap_or(0);
        if col + w > width {
            rows.push(String::new());
            col = 0;
        }
        rows.last_mut().unwrap().push(c);
        col += w;
    }
    if cursor >= input.chars().count() {
        place = (rows.len() - 1, col);
    }
    (rows, place)
}

fn side(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.current() {
        Some(chat) if chat.is_room() => format!("Участники {}", chat.occupants.len()),
        Some(chat) if chat.kind == ChatKind::Server => "Аккаунт".to_owned(),
        Some(_) => "Собеседник".to_owned(),
        None => "—".to_owned(),
    };
    let block = pane(&title, false).title(
        TextLine::from(Span::styled(" › ", Style::default().fg(theme::DIM))).alignment(Alignment::Right),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.areas.right = Some(area);
    app.areas.hide_right = Some(Rect::new(area.x + area.width.saturating_sub(4), area.y, 3, 1));

    let Some(chat) = app.current() else { return };
    let mut rows: Vec<TextLine> = Vec::new();
    let mut nicks = Vec::new();
    match &chat.kind {
        ChatKind::Room { .. } => {
            for (nick, real) in &chat.occupants {
                let y = inner.y + rows.len() as u16;
                if y < inner.bottom() {
                    nicks.push((Rect::new(inner.x, y, inner.width, 1), nick.clone()));
                }
                let mut spans = vec![
                    Span::styled("● ", Style::default().fg(theme::OK)),
                    Span::styled(nick.clone(), theme::nick(nick)),
                ];
                if let Some(real) = real
                    && real.node().is_some_and(|n| n.as_str() != nick)
                {
                    spans.push(Span::styled(format!(" {real}"), Style::default().fg(theme::DIM)));
                }
                rows.push(TextLine::from(spans));
            }
        }
        ChatKind::Server => {
            let state = app.connection.get(&chat.account).cloned().unwrap_or_default();
            let colour = if state == "в сети" {
                theme::OK
            } else {
                theme::WARN
            };
            rows.push(TextLine::from(vec![
                Span::styled("● ", Style::default().fg(colour)),
                Span::styled(
                    chat.account.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            rows.push(TextLine::from(Span::styled(
                format!("  {state}"),
                Style::default().fg(theme::DIM),
            )));
            rows.push(TextLine::default());
            let chats = app.chats.iter().filter(|c| c.account == chat.account);
            let rooms = chats.clone().filter(|c| c.is_room()).count();
            let direct = chats.filter(|c| c.kind == ChatKind::Direct).count();
            let label = |name: &str, value: String| {
                TextLine::from(vec![
                    Span::styled(format!("{name:<8}"), Style::default().fg(theme::DIM)),
                    Span::raw(value),
                ])
            };
            rows.push(label("сервер", chat.account.domain().to_string()));
            rows.push(label("чаты", direct.to_string()));
            rows.push(label("комнаты", rooms.to_string()));
            rows.push(TextLine::default());
            for command in ["/services", "/add адрес", "/join комната"] {
                rows.push(TextLine::from(Span::styled(
                    command,
                    Style::default().fg(theme::ACCENT),
                )));
            }
            rows.push(TextLine::default());
            rows.push(TextLine::from(Span::styled(
                "свои клиенты",
                Style::default().fg(theme::DIM),
            )));
            let own = app.own_resource.get(&chat.account);
            let mut clients: Vec<omtalk_xmpp::presence::Online> = app
                .clients(&chat.account, &chat.account)
                .into_iter()
                .cloned()
                .collect();
            for client in &mut clients {
                if Some(&client.resource) == own {
                    client.client = format!("{} (это)", client.client);
                }
            }
            client_rows(&mut rows, &clients.iter().collect::<Vec<_>>(), inner.width);
        }
        ChatKind::Direct => {
            rows.push(TextLine::from(Span::styled(
                "в сети",
                Style::default().fg(theme::DIM),
            )));
            let clients = app.clients(&chat.account, &chat.jid);
            if clients.is_empty() {
                rows.push(TextLine::from(Span::styled(
                    "  никого",
                    Style::default().fg(theme::DIM),
                )));
            }
            client_rows(&mut rows, &clients, inner.width);
            rows.push(TextLine::default());
            rows.push(TextLine::from(Span::styled(
                "устройства OMEMO",
                Style::default().fg(theme::DIM),
            )));
            let devices = match &chat.devices {
                None => {
                    rows.push(TextLine::from(Span::styled("…", Style::default().fg(theme::DIM))));
                    &[][..]
                }
                Some(devices) if devices.is_empty() => {
                    rows.push(TextLine::from(Span::styled(
                        "нет OMEMO-устройств:",
                        Style::default().fg(theme::WARN),
                    )));
                    rows.push(TextLine::from(Span::styled(
                        "пишите без шифрования (^O)",
                        Style::default().fg(theme::DIM),
                    )));
                    &[][..]
                }
                Some(devices) => devices.as_slice(),
            };
            for device in devices {
                let (dot, colour) = match (&device.fingerprint, device.has_session) {
                    (None, _) => ("○", theme::DIM),
                    (Some(_), true) => ("●", theme::OK),
                    (Some(_), false) => ("●", theme::ACCENT),
                };
                let mut header = vec![
                    Span::styled(format!("{dot} "), Style::default().fg(colour)),
                    Span::styled(device.device.to_string(), Style::default().fg(theme::TEXT)),
                ];
                if device.this_device {
                    header.push(Span::styled(
                        " это устройство",
                        Style::default().fg(theme::ACCENT),
                    ));
                }
                rows.push(TextLine::from(header));
                let detail = match &device.fingerprint {
                    Some(fp) => fp.clone(),
                    None => "нет ключей — неактивно?".to_owned(),
                };
                for part in wrap(&detail, inner.width.saturating_sub(2) as usize) {
                    rows.push(TextLine::from(Span::styled(
                        format!("  {part}"),
                        Style::default().fg(theme::DIM),
                    )));
                }
            }
        }
    }
    frame.render_widget(Paragraph::new(rows), inner);
    app.areas.nicks.extend(nicks);
}

fn status_line(frame: &mut Frame, app: &App, area: Rect) {
    let key = |k: &str| {
        let text = if app.modifier == KeyModifiers::ALT {
            format!("M-{k}")
        } else if app.modifier == KeyModifiers::SUPER {
            format!("◆{k}")
        } else if app.modifier.is_empty() {
            format!("{} {}", leader_label(app), k.to_lowercase())
        } else {
            format!("^{k}")
        };
        Span::styled(text, Style::default().fg(theme::ACCENT))
    };
    let dim = |t: &str| Span::styled(t.to_owned(), Style::default().fg(theme::DIM));
    let hints = TextLine::from(vec![
        Span::styled(leader_label(app), Style::default().fg(theme::ACCENT)),
        dim(" клавиши  "),
        key("O"),
        dim(" OMEMO  "),
        Span::styled("Tab", Style::default().fg(theme::ACCENT)),
        dim(" дописать/фокус  "),
        Span::styled("/", Style::default().fg(theme::ACCENT)),
        dim(" команды  "),
        key("Q"),
        dim(" выход "),
    ]);
    // A message for the user takes the line: first, in full, in place of the hints and
    // with the connections down to their dots. It fades after a while (the runtime
    // clears it).
    if !app.status.is_empty() {
        let mut spans: Vec<Span> = app
            .connection
            .values()
            .map(|state| {
                Span::styled(
                    "●",
                    Style::default().fg(if state == "в сети" {
                        theme::OK
                    } else {
                        theme::WARN
                    }),
                )
            })
            .collect();
        spans.push(Span::raw(" "));
        let room = (area.width as usize).saturating_sub(spans.iter().map(|s| s.width()).sum());
        spans.push(Span::styled(
            truncate(&app.status, room),
            Style::default().fg(theme::WARN),
        ));
        frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
        return;
    }
    let mut left = Vec::new();
    for (account, state) in &app.connection {
        let colour = if state == "в сети" {
            theme::OK
        } else {
            theme::WARN
        };
        left.push(Span::styled("● ", Style::default().fg(colour)));
        left.push(Span::styled(
            format!("{account} {state}  "),
            Style::default().fg(theme::DIM),
        ));
    }
    let hints_width = hints.width() as u16;
    // Where the accounts' names do not fit beside the hints, their dots do.
    if TextLine::from(left.clone()).width() + hints_width as usize > area.width as usize {
        left = app
            .connection
            .values()
            .map(|state| {
                Span::styled(
                    "● ",
                    Style::default().fg(if state == "в сети" {
                        theme::OK
                    } else {
                        theme::WARN
                    }),
                )
            })
            .collect();
    }
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(hints_width)]).areas(area);
    frame.render_widget(Paragraph::new(TextLine::from(left)), left_area);
    frame.render_widget(Paragraph::new(hints), right_area);
}

/// The services window, over the middle of the screen.
fn services_window(frame: &mut Frame, app: &mut App, screen: Rect) {
    let Some(services) = app.services.as_ref() else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(84);
    let height = screen.height.saturating_sub(3).min(30);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height.saturating_sub(1) - height) / 2,
        width,
        height,
    };
    app.areas.service_window = area;
    frame.render_widget(Clear, area);
    let place = match &services.node {
        Some(node) => format!("{} · {node}", services.jid),
        None => services.jid.to_string(),
    };
    let title = match &services.form {
        Some(form) => format!("{} · {}", form.title, form.jid),
        None => format!("Сервисы · {place}"),
    };
    let block = pane(&title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let [body, problem, buttons, hint] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let body = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(2),
        ..body
    };

    let (status, problem_style) = match (&services.problem, services.loading) {
        (Some(text), _) => (text.clone(), Style::default().fg(theme::WARN)),
        (None, true) => ("загрузка…".to_owned(), Style::default().fg(theme::DIM)),
        (None, false) => (String::new(), Style::default()),
    };
    frame.render_widget(Paragraph::new(format!(" {status}")).style(problem_style), problem);

    let mut row_areas = Vec::new();
    let mut field_areas = Vec::new();
    let labels: Vec<(String, ServiceButton, bool)>;
    let hint_text: String;
    match &services.form {
        None => {
            let mut lines: Vec<(TextLine, Option<usize>)> = Vec::new();
            if let Some(info) = services.listing.as_ref().and_then(|l| l.info.as_ref()) {
                for identity in &info.identities {
                    let name = identity.name.clone().unwrap_or_default();
                    lines.push((
                        TextLine::from(vec![
                            Span::styled(name, Style::default().add_modifier(Modifier::BOLD)),
                            Span::styled(
                                format!("  {}/{}", identity.category, identity.kind),
                                Style::default().fg(theme::DIM),
                            ),
                        ]),
                        None,
                    ));
                }
            }
            let listing = services.listing.clone().unwrap_or_default();
            let mut row = 0;
            for (heading, entries, icon) in [
                ("Сервисы", &listing.items, app.icons.service),
                ("Команды", &listing.commands, app.icons.command),
            ] {
                if entries.is_empty() {
                    continue;
                }
                lines.push((TextLine::default(), None));
                lines.push((
                    TextLine::styled(
                        heading,
                        Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    None,
                ));
                for entry in entries {
                    let name = entry
                        .name
                        .clone()
                        .unwrap_or_else(|| entry.node.clone().unwrap_or_else(|| entry.jid.to_string()));
                    let address = match (&entry.node, heading) {
                        (_, "Команды") => String::new(),
                        (Some(node), _) => format!("  {} · {node}", entry.jid),
                        (None, _) if name != entry.jid.as_str() => format!("  {}", entry.jid),
                        (None, _) => String::new(),
                    };
                    let mut line = TextLine::from(vec![
                        Span::raw(format!(" {icon} ")),
                        Span::raw(name),
                        Span::styled(address, Style::default().fg(theme::DIM)),
                    ]);
                    if row == services.cursor {
                        line = line.style(theme::selected());
                    }
                    lines.push((line, Some(row)));
                    row += 1;
                }
            }
            if services.listing.is_some() && row == 0 && services.problem.is_none() {
                lines.push((
                    TextLine::styled("здесь ничего нет", Style::default().fg(theme::DIM)),
                    None,
                ));
            }
            let cursor_line = lines
                .iter()
                .position(|(_, r)| *r == Some(services.cursor))
                .unwrap_or(0);
            let skip = cursor_line.saturating_sub(body.height as usize - 1);
            for (i, (line, row)) in lines
                .into_iter()
                .skip(skip)
                .take(body.height as usize)
                .enumerate()
            {
                let place = Rect {
                    y: body.y + i as u16,
                    height: 1,
                    ..body
                };
                if let Some(row) = row {
                    row_areas.push((place, row));
                }
                frame.render_widget(Paragraph::new(line), place);
            }
            labels = vec![
                ("Назад".into(), ServiceButton::Back, false),
                ("Обновить".into(), ServiceButton::Reload, false),
                ("Закрыть".into(), ServiceButton::Close, false),
            ];
            hint_text = "Enter открыть · ← назад · r обновить · Esc закрыть".into();
        }
        Some(form) => {
            let mut lines: Vec<(TextLine, Option<usize>)> = Vec::new();
            let instructions = form.step.form.as_ref().and_then(|f| f.instructions.clone());
            for text in instructions.into_iter() {
                for part in wrap(&text, body.width as usize) {
                    lines.push((TextLine::styled(part, Style::default().fg(theme::DIM)), None));
                }
            }
            for note in form.notes() {
                let style = match note.kind {
                    NoteKind::Info => Style::default(),
                    NoteKind::Warn | NoteKind::Error => Style::default().fg(theme::WARN),
                };
                for part in wrap(&note.text, body.width as usize) {
                    lines.push((TextLine::styled(part, style), None));
                }
            }
            if !lines.is_empty() {
                lines.push((TextLine::default(), None));
            }
            let label_width = form
                .fields
                .iter()
                .filter(|f| f.shown() && f.kind != FieldType::Fixed)
                .map(|f| f.label.width() + 2)
                .max()
                .unwrap_or(0)
                .min(body.width as usize / 3);
            let running = !form.finished();
            for (index, field) in form.fields.iter().enumerate() {
                if !field.shown() {
                    continue;
                }
                let focused = running && index == form.focus;
                if field.kind == FieldType::Fixed {
                    for part in wrap(&field.value, body.width as usize) {
                        lines.push((TextLine::styled(part, Style::default().fg(theme::DIM)), None));
                    }
                    continue;
                }
                let mark = if field.required { "*" } else { "" };
                let label = format!("{}{mark}", field.label);
                let value = field_value(field, focused, running);
                let label_style = if focused {
                    Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::DIM)
                };
                let value_style = if focused {
                    Style::default().add_modifier(Modifier::UNDERLINED)
                } else {
                    Style::default()
                };
                // A label too long for the column (servers write whole sentences) goes
                // on its own line, above its value.
                let label = if label.width() > label_width {
                    for part in wrap(&label, body.width as usize) {
                        lines.push((TextLine::styled(part, label_style), Some(index)));
                    }
                    " ".repeat(label_width + 1)
                } else {
                    format!("{label:>label_width$} ")
                };
                lines.push((
                    TextLine::from(vec![
                        Span::styled(label, label_style),
                        Span::styled(value, value_style),
                    ]),
                    Some(index),
                ));
            }
            let focus_line = lines
                .iter()
                .rposition(|(_, r)| *r == Some(form.focus))
                .unwrap_or(0);
            let skip = focus_line.saturating_sub(body.height as usize - 1);
            for (i, (line, field)) in lines
                .into_iter()
                .skip(skip)
                .take(body.height as usize)
                .enumerate()
            {
                let place = Rect {
                    y: body.y + i as u16,
                    height: 1,
                    ..body
                };
                if let Some(field) = field {
                    field_areas.push((place, field));
                }
                frame.render_widget(Paragraph::new(line), place);
            }
            if running {
                let default = form.step.default_action;
                labels = form
                    .step
                    .actions
                    .iter()
                    .map(|&a| {
                        (
                            step_label(a).to_owned(),
                            ServiceButton::Step(a),
                            Some(a) == default,
                        )
                    })
                    .chain([("Отмена".to_owned(), ServiceButton::Close, false)])
                    .collect();
                let desc = form.fields.get(form.focus).and_then(|f| f.desc.clone());
                hint_text = desc
                    .unwrap_or_else(|| "Tab поле · Пробел/←→ выбрать · Enter дальше · Esc отменить".into());
            } else {
                let done = match form.step.status {
                    CommandStatus::Canceled => "отменено",
                    _ => "выполнено",
                };
                labels = vec![
                    (format!("К списку ({done})"), ServiceButton::Dismiss, true),
                    ("Закрыть".into(), ServiceButton::Close, false),
                ];
                hint_text = "Enter к списку · Esc закрыть".into();
            }
        }
    }

    // Buttons, right-aligned, the default one stressed.
    let total: u16 = labels.iter().map(|(l, ..)| l.width() as u16 + 5).sum();
    let mut x = buttons.x + buttons.width.saturating_sub(total + 1);
    let mut button_areas = Vec::new();
    for (label, button, default) in labels {
        let text = format!("[ {label} ]");
        let w = text.width() as u16;
        let place = Rect {
            x,
            width: w.min(buttons.right().saturating_sub(x)),
            ..buttons
        };
        let style = if default {
            theme::selected()
        } else {
            Style::default().fg(theme::ACCENT)
        };
        frame.render_widget(Paragraph::new(text).style(style), place);
        button_areas.push((place, button));
        x += w + 1;
    }
    frame.render_widget(
        Paragraph::new(format!(" {hint_text}")).style(Style::default().fg(theme::DIM)),
        hint,
    );
    app.areas.service_rows = row_areas;
    app.areas.service_fields = field_areas;
    app.areas.service_buttons = button_areas;
}

/// A field's value as shown: secrets hidden, switches and choices drawn as such.
fn field_value(field: &FieldState, focused: bool, editing: bool) -> String {
    let caret = if focused { "▏" } else { "" };
    match field.kind {
        FieldType::Boolean => {
            let on = matches!(field.value.as_str(), "1" | "true");
            if on { "[x]" } else { "[ ]" }.to_owned()
        }
        FieldType::ListSingle => {
            let chosen = field
                .options
                .iter()
                .find(|(_, v)| v == &field.value)
                .map(|(l, _)| l.clone())
                .unwrap_or_else(|| field.value.clone());
            if editing {
                format!("‹ {chosen} ›")
            } else {
                chosen
            }
        }
        FieldType::ListMulti => field
            .options
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                let on = if field.chosen.contains(value) {
                    "[x]"
                } else {
                    "[ ]"
                };
                let here = if focused && i == field.option_cursor {
                    "›"
                } else {
                    ""
                };
                format!("{here}{on} {label}")
            })
            .collect::<Vec<_>>()
            .join("  "),
        FieldType::TextPrivate => format!("{}{caret}", "•".repeat(field.value.chars().count())),
        FieldType::TextMulti => format!("{}{caret}", field.value.replace('\n', " ⏎ ")),
        _ => format!("{}{caret}", field.value),
    }
}

fn step_label(action: CommandAction) -> &'static str {
    match action {
        CommandAction::Execute => "Выполнить",
        CommandAction::Prev => "Назад",
        CommandAction::Next => "Далее",
        CommandAction::Complete => "Готово",
        CommandAction::Cancel => "Отмена",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, ChatKind};
    use crate::config::Modifier;
    use omtalk_xmpp::BareJid;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app, &mut Pictures::new())).unwrap();
        let buffer = terminal.backend().buffer().clone();
        // A wide character fills two cells; the second holds nothing to read.
        (0..height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += (symbol.width() as u16).max(1);
                }
                row
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn app_with_chat() -> App {
        let mut app = App::new(vec![jid("me@example.org")], Modifier::Ctrl, 26, 30);
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.select(bob);
        let line = |text: &str, at, encrypted| Line::message(at, "bob".into(), text.into(), encrypted, false);
        app.push(bob, line("привет", 1_700_000_000, true));
        app.push(bob, line("это без шифрования", 1_700_000_010, false));
        app
    }

    #[test]
    fn three_panes_with_the_chat_its_lock_and_the_unencrypted_mark() {
        let mut app = app_with_chat();
        let text = screen(&mut app, 120, 20);
        assert!(text.contains("omtalk"), "chat list pane");
        assert!(text.contains("@ bob"), "the chat in the list");
        assert!(text.contains("привет"));
        let mark = format!("{}это без шифрования", theme::NERD.plain_mark);
        assert!(text.contains(&mark), "plain text is marked\n{text}");
        assert!(text.contains(theme::NERD.locked), "the lock on the input");
        assert!(
            !text.chars().any(|c| ('\u{1F300}'..='\u{1FAFF}').contains(&c)),
            "no colour emoji in the interface"
        );
        assert!(
            text.contains("Собеседник") && text.contains("устройства OMEMO"),
            "right pane for a direct chat"
        );
        assert!(app.areas.left.is_some() && app.areas.right.is_some());
    }

    #[test]
    fn narrow_terminals_fold_panes_and_hidden_panes_leave_a_handle() {
        let mut app = app_with_chat();
        screen(&mut app, 70, 20);
        assert!(app.areas.left.is_some(), "the chat list still fits");
        assert!(
            app.areas.right.is_none() && app.areas.show_right.is_some(),
            "participants fold first"
        );
        screen(&mut app, 40, 20);
        assert!(app.areas.left.is_none() && app.areas.show_left.is_some());

        app.show_left = false;
        screen(&mut app, 120, 20);
        assert!(
            app.areas.left.is_none() && app.areas.show_left.is_some(),
            "hidden on purpose, a strip to bring it back"
        );
    }

    #[test]
    fn wrapping_respects_display_width_and_words() {
        assert_eq!(wrap("один два три", 8), ["один два", "три"]);
        assert_eq!(wrap("абвгдежзик", 4), ["абвг", "дежз", "ик"]);
        assert_eq!(wrap("строка\nвторая", 20), ["строка", "вторая"]);
        assert!(
            wrap("🔒🔒🔒", 4).iter().all(|l| l.width() <= 4),
            "wide characters count double"
        );
    }

    #[test]
    fn the_omemo_button_area_matches_what_is_drawn() {
        let mut app = app_with_chat();
        let text = screen(&mut app, 120, 20);
        let button = app.areas.omemo_button;
        let row: String = text.lines().nth(button.y as usize).unwrap().chars().collect();
        assert!(row.contains(theme::NERD.locked));
        assert!(button.width >= theme::NERD.locked.width() as u16);
    }

    #[test]
    fn a_fetched_picture_is_drawn_in_the_chat_and_clickable() {
        let mut app = App::new(vec![jid("me@example.org")], Modifier::Ctrl, 26, 30);
        let me = jid("me@example.org");
        let bob = app.chat(&me, &jid("bob@example.org"), ChatKind::Direct);
        app.select(bob);
        app.push(
            bob,
            Line::message(
                1_700_000_000,
                "bob".into(),
                "https://example.org/cat.png".into(),
                false,
                false,
            ),
        );

        // Not fetched yet: the line names the file and says so.
        let text = screen(&mut app, 120, 24);
        assert!(text.contains("cat.png") && text.contains("загружается"), "{text}");

        app.media_done(
            "https://example.org/cat.png",
            crate::app::MediaState::Ready("/cache/cat.png".into()),
        );
        let mut pictures = Pictures::new();
        let image = image::DynamicImage::new_rgb8(8, 8);
        pictures.insert(
            "https://example.org/cat.png".into(),
            ratatui_image::picker::Picker::halfblocks().new_resize_protocol(image),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app, &mut pictures)).unwrap();
        let (place, path) = app.areas.files.first().expect("the picture is clickable");
        assert_eq!(path.to_str(), Some("/cache/cat.png"));
        assert_eq!(place.height as usize, PICTURE_ROWS);
        assert!(
            app.areas.messages.contains(place.as_position()),
            "inside the conversation"
        );
    }

    #[test]
    fn the_services_window_lists_runs_a_form_and_its_buttons_click() {
        use crate::services::{Listing, Request};
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        use omtalk_xmpp::services::{CommandStep, ServiceItem};

        let mut app = app_with_chat();
        let actions = app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            matches!(&actions[..], [Action::Services { request: Request::Browse { jid, .. }, .. }] if jid.as_str() == "example.org")
        );
        let server: omtalk_xmpp::Jid = "example.org".parse().unwrap();
        app.services.as_mut().unwrap().listed(
            &server,
            &None,
            Ok(Listing {
                info: None,
                items: vec![ServiceItem {
                    jid: "muc.example.org".parse().unwrap(),
                    node: None,
                    name: Some("Комнаты".into()),
                }],
                commands: vec![ServiceItem {
                    jid: server.clone(),
                    node: Some("uptime".into()),
                    name: Some("Get uptime".into()),
                }],
            }),
        );
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("Сервисы · example.org") && text.contains("Комнаты") && text.contains("Get uptime"),
            "{text}"
        );

        // A click on the command runs it.
        let (row, _) = *app.areas.service_rows.iter().find(|(_, i)| *i == 1).unwrap();
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let actions = app.on_mouse(click(row.x + 2, row.y));
        assert!(
            matches!(&actions[..], [Action::Services { request: Request::Execute { node, .. }, title }] if node == "uptime" && title == "Get uptime")
        );

        let form: omtalk_xmpp::Element = r#"<x xmlns="jabber:x:data" type="form"><instructions>Fill it</instructions>
            <field var="secret" type="text-private" label="Пароль" required="true"><required/><value>abc</value></field>
            <field var="on" type="boolean" label="Включить"/></x>"#
            .parse()
            .unwrap();
        let step = CommandStep {
            node: "uptime".into(),
            session: Some("s".into()),
            status: omtalk_xmpp::services::CommandStatus::Executing,
            form: Some(form.try_into().unwrap()),
            notes: Vec::new(),
            actions: vec![CommandAction::Complete],
            default_action: Some(CommandAction::Complete),
        };
        app.services
            .as_mut()
            .unwrap()
            .stepped(server.clone(), "Get uptime".into(), Ok(step.clone()));
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("Пароль*") && text.contains("•••") && !text.contains("abc"),
            "secrets stay hidden: {text}"
        );
        assert!(
            text.contains("[ ]") && text.contains("[ Готово ]") && text.contains("[ Отмена ]"),
            "{text}"
        );

        // A click on the switch turns it; a click on Готово sends the form.
        let (switch, _) = *app.areas.service_fields.iter().find(|(_, i)| *i == 1).unwrap();
        app.on_mouse(click(switch.x + 2, switch.y));
        assert!(screen(&mut app, 120, 30).contains("[x]"));
        let (done, _) = *app
            .areas
            .service_buttons
            .iter()
            .find(|(_, b)| *b == ServiceButton::Step(CommandAction::Complete))
            .unwrap();
        let actions = app.on_mouse(click(done.x + 1, done.y));
        let [
            Action::Services {
                request:
                    Request::Execute {
                        action: CommandAction::Complete,
                        form: Some(sent),
                        ..
                    },
                ..
            },
        ] = &actions[..]
        else {
            panic!("{actions:?}")
        };
        assert_eq!(sent.fields[1].values, ["1"]);

        // Esc on a running command cancels it on the server and closes the window.
        let actions = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(
            &actions[..],
            [Action::Services {
                request: Request::Execute {
                    action: CommandAction::Cancel,
                    ..
                },
                ..
            }]
        ));
        assert!(app.services.as_ref().unwrap().form.is_none());
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_empty()
        );
        assert!(app.services.is_none());

        // So does a click beside the window while a command waits for its form.
        app.open_services(None);
        app.services
            .as_mut()
            .unwrap()
            .stepped(server.clone(), "Get uptime".into(), Ok(step));
        screen(&mut app, 120, 30);
        let actions = app.on_mouse(click(0, 0));
        assert!(
            matches!(
                &actions[..],
                [Action::Services {
                    request: Request::Execute {
                        action: CommandAction::Cancel,
                        form: None,
                        ..
                    },
                    ..
                }]
            ),
            "{actions:?}"
        );
        assert!(app.services.is_none());
    }

    #[test]
    fn a_click_on_a_room_participant_opens_a_chat_with_them() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new(vec![jid("me@example.org")], Modifier::Ctrl, 26, 30);
        let me = jid("me@example.org");
        let room = app.chat(
            &me,
            &jid("room@muc.example.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        let chat = &mut app.chats[room];
        chat.occupants.insert("bob".into(), Some(jid("bob@example.org")));
        chat.occupants.insert("ghost".into(), None);
        chat.occupants.insert("me".into(), Some(me.clone()));
        app.push(
            room,
            Line::message(1_700_000_000, "bob".into(), "привет".into(), true, false),
        );
        screen(&mut app, 120, 20);
        let click = |area: Rect| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        };
        let at = |app: &App, nick: &str, left: bool| {
            let right = app.areas.right.unwrap().x;
            app.areas
                .nicks
                .iter()
                .find(|(r, n)| n == nick && (r.x < right) == left)
                .map(|(r, _)| *r)
        };

        // The sender's name in the chat.
        let name = at(&app, "bob", true).expect("bob's name in the chat is clickable");
        let actions = app.on_mouse(click(name));
        assert_eq!(app.current().unwrap().jid, jid("bob@example.org"));
        assert!(matches!(&actions[..], [Action::Devices { .. }]));

        // The participants pane; one whose address the room hides says so.
        app.select(room);
        screen(&mut app, 120, 20);
        app.on_mouse(click(at(&app, "ghost", false).unwrap()));
        assert_eq!(app.current().unwrap().jid, jid("room@muc.example.org"));
        assert!(app.status.contains("не показывает"), "{}", app.status);
        app.on_mouse(click(at(&app, "bob", false).unwrap()));
        assert_eq!(app.current().unwrap().jid, jid("bob@example.org"));
    }

    #[test]
    fn accounts_head_their_chats_and_fold_away() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let (home, work) = (jid("me@home.org"), jid("me@work.org"));
        let mut app = App::new(vec![home.clone(), work.clone()], Modifier::Ctrl, 26, 30);
        app.connection.insert(home.clone(), "в сети".into());
        let a = app.chat(&home, &jid("anna@home.org"), ChatKind::Direct);
        let b = app.chat(&work, &jid("boss@work.org"), ChatKind::Direct);
        let c = app.chat(&home, &jid("carl@home.org"), ChatKind::Direct);
        app.select(a);
        app.push(
            b,
            Line::message(1_700_000_000, "boss".into(), "срочно".into(), true, false),
        );
        let text = screen(&mut app, 120, 20);
        assert!(
            text.contains("me@home.org") && text.contains("me@work.org"),
            "{text}"
        );

        // The list goes by account, and so do the arrows: anna, carl, then boss.
        assert_eq!(app.visible_chats(), [a, c, b]);
        app.focus = Focus::Chats;
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected, Some(c));

        // A click on the work heading folds it; its unread count stays in sight.
        let (heading, _) = app
            .areas
            .group_rows
            .iter()
            .find(|(_, g)| *g == Group::Account(work.clone()))
            .cloned()
            .unwrap();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: heading.x + 1,
            row: heading.y,
            modifiers: KeyModifiers::NONE,
        });
        let text = screen(&mut app, 120, 20);
        assert!(!text.contains("boss") && text.contains("▸"), "{text}");
        assert_eq!(app.visible_chats(), [a, c]);
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.selected, Some(a), "folded chats are skipped");

        // z: only the current account; again, all of them.
        app.collapsed.clear();
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.visible_chats(), [a, c]);
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.visible_chats(), [a, c, b]);

        // Commands go from the account on the peer's server, else from the one last picked.
        assert_eq!(
            app.account_for(Some(&"x@work.org".parse().unwrap())),
            Some(work.clone())
        );
        assert_eq!(
            app.account_for(Some(&"room@muc.work.org".parse().unwrap())),
            Some(work.clone())
        );
        app.select(a);
        assert_eq!(
            app.account_for(Some(&"x@elsewhere.net".parse().unwrap())),
            Some(home.clone())
        );
        app.select(b);
        assert_eq!(app.account_for(None), Some(work.clone()));
    }

    #[test]
    fn chats_go_into_folders_by_command_and_by_dragging() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let anna = app.chat(&me, &jid("anna@home.org"), ChatKind::Direct);
        let boss = app.chat(&me, &jid("boss@home.org"), ChatKind::Direct);
        let team = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        let mouse = |kind, area: Rect| MouseEvent {
            kind,
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        };
        let type_line = |app: &mut App, text: &str| {
            app.focus = Focus::Input;
            for c in text.chars() {
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        };

        // One account and no folders: no headings at all.
        assert!(!app.list().iter().any(|e| matches!(e, Entry::Heading(_))));

        // A command puts the current chat into a new folder, which is kept.
        app.select(boss);
        let saved = type_line(&mut app, "/folder Рабочие дела");
        assert!(matches!(&saved[..], [Action::SaveFolders(f)] if f[0].name == "Рабочие дела"));
        assert_eq!(
            app.list(),
            [
                Entry::Heading(Group::Account(me.clone())),
                Entry::Heading(Group::Folder(me.clone(), "Рабочие дела".into())),
                Entry::Chat(boss),
                Entry::Chat(anna),
                Entry::Chat(team),
            ]
        );
        let text = screen(&mut app, 120, 20);
        assert!(
            text.contains("Рабочие дела") && text.contains("me@home.org"),
            "{text}"
        );

        // A room dragged on to the folder's heading joins it.
        let row_of = |app: &App, index| app.areas.chat_rows.iter().find(|(_, i)| *i == index).unwrap().0;
        let heading =
            |app: &App, group: &Group| app.areas.group_rows.iter().find(|(_, g)| g == group).unwrap().0;
        let folder = Group::Folder(me.clone(), "Рабочие дела".into());
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), row_of(&app, team)));
        app.on_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            heading(&app, &folder),
        ));
        let saved = app.on_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            heading(&app, &folder),
        ));
        assert!(
            matches!(&saved[..], [Action::SaveFolders(f)] if f[0].chats.len() == 2),
            "{saved:?}"
        );
        assert_eq!(app.visible_chats(), [boss, team, anna]);

        // A plain click on a chat moves nothing.
        screen(&mut app, 120, 20);
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), row_of(&app, anna)));
        assert!(
            app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), row_of(&app, anna)))
                .is_empty()
        );

        // Folded, the folder hides its chats; Space folds the current chat's heading.
        app.toggle_group(&folder);
        assert_eq!(app.visible_chats(), [anna]);
        app.toggle_group(&folder);
        app.select(boss);
        app.focus = Focus::Chats;
        app.on_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(app.collapsed.contains(&folder));
        app.collapsed.clear();

        // z folds other accounts; with one there are none, and nothing hides.
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.visible_chats(), [boss, team, anna]);

        // Renamed; dragged out on to the account; the last one out takes the folder along.
        type_line(&mut app, "/rename-folder Рабочие дела → Работа");
        assert_eq!(app.folders[0].name, "Работа");
        screen(&mut app, 120, 20);
        let account = Group::Account(me.clone());
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), row_of(&app, team)));
        app.on_mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            heading(&app, &account),
        ));
        app.select(boss);
        let saved = type_line(&mut app, "/folder");
        assert!(
            matches!(&saved[..], [Action::SaveFolders(f)] if f.is_empty()),
            "{saved:?}"
        );
        assert!(app.folders.is_empty());
    }

    #[test]
    fn tab_completes_and_the_list_shows_what_fits() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        for (nick, real) in [
            ("Алиса", "alice@home.org"),
            ("alex", "alex@home.org"),
            ("alexey", "alexey@home.org"),
            ("me", "me@home.org"),
        ] {
            app.chats[room].occupants.insert(nick.into(), Some(jid(real)));
        }
        app.focus = Focus::Input;
        let press = |app: &mut App, code| drop(app.on_key(KeyEvent::new(code, KeyModifiers::NONE)));
        let typed = |app: &mut App, text: &str| text.chars().for_each(|c| press(app, KeyCode::Char(c)));

        // A slash brings up the commands with what they do.
        typed(&mut app, "/");
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("/services [АДРЕС]") && text.contains("сервисы и команды сервера"),
            "{text}"
        );
        typed(&mut app, "re");
        let text = screen(&mut app, 120, 30);
        assert!(text.contains("/refuse") && !text.contains("/services"), "{text}");

        // Tab goes round the candidates, Shift+Tab back; typing takes the one shown.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/refuse ");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "/rename-folder ");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.input, "/refuse ");
        typed(&mut app, "ali");
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.input, "/refuse alice@home.org ",
            "addresses of people in the room"
        );

        // Nicks, without one's own, addressed at the start of the line.
        press(&mut app, KeyCode::Esc);
        typed(&mut app, "a");
        press(&mut app, KeyCode::Tab);
        assert_eq!((app.input.as_str(), app.cursor), ("alex: ", 6));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "alexey: ");
        assert!(
            screen(&mut app, 120, 30).contains("alexey"),
            "the candidates are listed"
        );
        press(&mut app, KeyCode::Esc);
        typed(&mut app, "m");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.input, "m", "one's own nick is not offered");

        // With nothing typed Tab still moves the focus; /help opens the list.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Chats);
        app.focus = Focus::Input;
        typed(&mut app, "/help");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input, "/");
        assert!(screen(&mut app, 120, 30).contains("этот список"));
    }

    #[test]
    fn the_chat_list_is_a_tree_of_servers_folders_and_chats() {
        let (home, work) = (jid("me@home.org"), jid("me@work.org"));
        let mut app = App::new(vec![home.clone(), work.clone()], Modifier::Ctrl, 30, 30);
        let anna = app.chat(&home, &jid("anna@home.org"), ChatKind::Direct);
        let bert = app.chat(&home, &jid("bert@home.org"), ChatKind::Direct);
        app.chat(&home, &jid("carl@home.org"), ChatKind::Direct);
        let boss = app.chat(&work, &jid("boss@work.org"), ChatKind::Direct);
        app.move_to_folder(anna, Some("Свои"));
        app.move_to_folder(bert, Some("Свои"));
        app.move_to_folder(boss, Some("Свои"));
        let text = screen(&mut app, 120, 20);
        // The pane's own border is │ too: cut it off by width, not by the character.
        let rows: Vec<String> = text
            .lines()
            .skip(1)
            .take(9)
            .map(|l| {
                l.chars()
                    .skip(1)
                    .take(28)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect();
        let want = [
            "▾ ● me@home.org",
            "├ ▾ \u{f07b} Свои",
            "│ ├ @ anna",
            "│ └ @ bert",
            "└ @ carl",
            "▾ ● me@work.org",
            "└ ▾ \u{f07b} Свои",
            "  └ @ boss",
        ];
        for (row, want) in rows.iter().zip(want) {
            assert!(
                row.starts_with(want),
                "{row:?} should start with {want:?}\n{rows:#?}"
            );
        }

        // Each account's folders are its own: a chat of another account does not go in.
        let (heading, _) = app
            .areas
            .group_rows
            .iter()
            .find(|(_, g)| *g == Group::Folder(home.clone(), "Свои".into()))
            .cloned()
            .unwrap();
        let (row, _) = app
            .areas
            .chat_rows
            .iter()
            .find(|(_, i)| *i == boss)
            .cloned()
            .unwrap();
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mouse = |kind, area: Rect| MouseEvent {
            kind,
            column: area.x + 3,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        };
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), row));
        assert!(
            app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), heading))
                .is_empty()
        );
        assert!(app.status.contains("свои"), "{}", app.status);

        // One account and no folders: a plain list, no branches.
        let mut app = App::new(vec![home.clone()], Modifier::Ctrl, 30, 30);
        app.chat(&home, &jid("anna@home.org"), ChatKind::Direct);
        assert!(!screen(&mut app, 120, 20).contains("└"));
    }

    #[test]
    fn the_input_line_edits_as_in_a_shell_and_the_leader_opens_a_key_list() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.focus = Focus::Input;
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let typed = |app: &mut App, text: &str| {
            text.chars()
                .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))))
        };

        typed(&mut app, "один два три");
        app.on_key(ctrl('w'));
        assert_eq!(app.input, "один два ");
        app.on_key(ctrl('a'));
        assert_eq!(app.cursor, 0);
        app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(app.cursor, 4, "by words");
        app.on_key(ctrl('k'));
        assert_eq!(app.input, "один");
        app.on_key(ctrl('y'));
        assert_eq!(app.input, "один два ", "what was cut comes back");
        app.on_key(ctrl('u'));
        assert_eq!((app.input.as_str(), app.cursor), ("", 0));
        assert!(
            app.show_left && app.selected == Some(bob),
            "Ctrl+B/K are text keys here, not shortcuts"
        );

        // The leader, then a key: the list shows in between; Ctrl+O still works held.
        app.on_key(ctrl('x'));
        assert!(app.leader_pending);
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("Ctrl+X") && text.contains("сервисы сервера"),
            "{text}"
        );
        app.on_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(!app.show_left && !app.leader_pending);
        app.on_key(ctrl('x'));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.show_left && !app.leader_pending, "Esc just cancels");
        app.on_key(ctrl('o'));
        assert!(!app.chats[bob].omemo);

        // The lock of a chat without OMEMO, on the selected row, is not a red block.
        app.show_left = true;
        screen(&mut app, 120, 30);
        let (row, _) = app
            .areas
            .chat_rows
            .iter()
            .find(|(_, i)| *i == bob)
            .cloned()
            .unwrap();
        let backend = ratatui::backend::TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut pictures = Pictures::new();
        terminal.draw(|f| draw(f, &mut app, &mut pictures)).unwrap();
        let buffer = terminal.backend().buffer();
        for x in row.x..row.x + row.width {
            assert_ne!(buffer[(x, row.y)].fg, theme::WARN, "cell {x} of the selected row");
        }

        // /quit leaves.
        app.focus = Focus::Input;
        typed(&mut app, "/quit");
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            [Action::Quit]
        );
    }

    #[test]
    fn text_not_for_this_device_is_said_once_per_chat() {
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let missed = omtalk_omemo::Incoming {
            peer: jid("bob@home.org"),
            sender: jid("bob@home.org"),
            nick: None,
            device: None,
            direction: omtalk_omemo::Direction::Incoming,
            encrypted: true,
            text: None,
            at: None,
            not_for_this_device: true,
            id: None,
            markable: false,
            archive_id: None,
        };
        app.on_incoming(&me, &missed, 1);
        app.on_incoming(&me, &missed, 2);
        let chat = &app.chats[0];
        assert_eq!(chat.lines.len(), 1, "{:?}", chat.lines);
        assert!(chat.lines[0].notice && chat.lines[0].text.contains("не для этого устройства"));
    }

    #[test]
    fn pasted_lines_stay_one_message_and_the_input_grows() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.focus = Focus::Messages;

        app.on_paste("fn main() {\r\n    println!(\"привет\");\r\n}\r\n");
        assert_eq!(app.focus, Focus::Input, "pasting goes to the input");
        let text = screen(&mut app, 100, 30);
        assert!(
            text.contains("    println!(\"привет\");") && text.contains("fn main() {"),
            "{text}"
        );
        assert!(
            app.areas.input.height >= 5,
            "three lines and more show: {:?}",
            app.areas.input
        );

        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        let sent = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let [Action::Send { text, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        assert_eq!(
            text, "fn main() {\n    println!(\"привет\");\n}\nx\ny",
            "one message, breaks kept"
        );
        screen(&mut app, 100, 30);
        assert_eq!(app.areas.input.height, 3, "back to one row");

        // Wrapping and the cursor: a long word fills rows; the cursor sits after it.
        let (rows, cursor) = input_rows("абвгдеж", 7, 5);
        assert_eq!(rows, ["абвг", "деж"]);
        assert_eq!(cursor, (1, 3));
        let (rows, cursor) = input_rows("ab\ncd", 3, 10);
        assert_eq!((rows, cursor), (vec!["ab".to_owned(), "cd".to_owned()], (1, 0)));
    }

    #[test]
    fn own_messages_show_how_far_they_got() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.focus = Focus::Input;
        "ты тут?"
            .chars()
            .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
        let sent = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let [Action::Send { id, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        let state = |app: &App| app.chats[bob].lines.last().unwrap().delivery.clone();
        assert!(screen(&mut app, 100, 20).contains("ты тут? ⋯"));
        app.delivery(&me, id, Delivery::Sent);
        assert!(screen(&mut app, 100, 20).contains("ты тут? ✓"));
        app.delivery(&me, id, Delivery::Read);
        app.delivery(&me, id, Delivery::Delivered);
        assert_eq!(
            state(&app),
            Some(Delivery::Read),
            "a late receipt does not undo the read marker"
        );
        app.delivery(&me, "someone-else", Delivery::Failed("нет".into()));
        assert_eq!(state(&app), Some(Delivery::Read));
        app.delivery(&me, id, Delivery::Failed("сервер недоступен".into()));
        assert!(screen(&mut app, 100, 20).contains("✗ не отправлено: сервер недоступен"));
        // A reason longer than the chat is wide wraps, all of it in sight.
        app.delivery(&me, id, Delivery::Failed("сервер получателя не принимает такие сообщения (policy-violation): Server does not accept other servers".into()));
        let text = screen(&mut app, 70, 20);
        assert!(text.contains("other servers"), "{text}");

        // In a room, the room's copy of one's message is the same message: not shown twice.
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        let sent = app.send_to(room, "всем привет".into());
        let [Action::Send { id, .. }] = &sent[..] else {
            panic!("{sent:?}")
        };
        let echo = omtalk_omemo::Incoming {
            peer: jid("team@muc.home.org"),
            sender: me.clone(),
            nick: Some("me".into()),
            device: None,
            direction: omtalk_omemo::Direction::Outgoing,
            encrypted: false,
            text: Some("всем привет".into()),
            at: None,
            not_for_this_device: false,
            id: Some(id.clone()),
            markable: false,
            archive_id: None,
        };
        app.on_incoming(&me, &echo, 5);
        let lines = &app.chats[room].lines;
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(lines[0].delivery, Some(Delivery::Sent));

        // Bob's markable message: read at once while his chat is open, later otherwise.
        let from_bob = omtalk_omemo::Incoming {
            peer: jid("bob@home.org"),
            sender: jid("bob@home.org"),
            nick: None,
            device: Some(7),
            direction: omtalk_omemo::Direction::Incoming,
            encrypted: true,
            text: Some("тут".into()),
            at: None,
            not_for_this_device: false,
            id: Some("b1".into()),
            markable: true,
            archive_id: Some("a1".into()),
        };
        assert!(
            app.on_incoming(&me, &from_bob, 6).is_empty(),
            "the room is open, not Bob"
        );
        let opened = app.select(bob);
        let read = |marker: Option<&str>, archive: &str| Action::Displayed {
            account: me.clone(),
            to: jid("bob@home.org"),
            room: false,
            marker: marker.map(Into::into),
            archive_id: Some(archive.into()),
        };
        assert!(opened.contains(&read(Some("b1"), "a1")), "{opened:?}");
        assert!(
            !app.select(bob)
                .iter()
                .any(|a| matches!(a, Action::Displayed { .. })),
            "once"
        );
        let again = omtalk_omemo::Incoming {
            id: Some("b2".into()),
            archive_id: Some("a2".into()),
            ..from_bob.clone()
        };
        assert_eq!(app.on_incoming(&me, &again, 7), [read(Some("b2"), "a2")]);
        // Without read markers for senders, one's own clients still learn it.
        app.read_markers = false;
        let third = omtalk_omemo::Incoming {
            id: Some("b3".into()),
            archive_id: Some("a3".into()),
            ..from_bob.clone()
        };
        assert_eq!(app.on_incoming(&me, &third, 8), [read(None, "a3")]);

        // Read on another device: nothing here is unread any more, and nothing is owed.
        app.select(room);
        let fourth = omtalk_omemo::Incoming {
            id: Some("b4".into()),
            archive_id: Some("a4".into()),
            ..from_bob
        };
        app.on_incoming(&me, &fourth, 9);
        assert_eq!(app.chats[bob].unread, 1);
        app.read_elsewhere(&me, &jid("bob@home.org"));
        assert_eq!(app.chats[bob].unread, 0);
        assert!(
            !app.select(bob)
                .iter()
                .any(|a| matches!(a, Action::Displayed { .. }))
        );
    }

    #[test]
    fn the_server_channel_heads_the_account_and_takes_commands_only() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let anna = app.chat(&me, &jid("anna@home.org"), ChatKind::Direct);
        app.server_notice(&me, "пароль не принят", 1);
        let server = app.chats.iter().position(|c| c.kind == ChatKind::Server).unwrap();
        assert_eq!(app.visible_chats(), [server, anna], "first under its account");
        assert_eq!(app.status, "пароль не принят", "and below for a while");
        // The server's own messages land there too.
        assert_eq!(app.chat(&me, &jid("home.org"), ChatKind::Direct), server);

        app.select(server);
        let text = screen(&mut app, 100, 20);
        assert!(
            text.contains("сервер") && text.contains("пароль не принят") && text.contains("Аккаунт"),
            "{text}"
        );
        app.focus = Focus::Input;
        "привет"
            .chars()
            .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
        assert!(
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .is_empty()
        );
        assert!(app.status.contains("только команды"), "{}", app.status);
        assert!(
            app.move_to_folder(server, Some("Папка")).is_empty(),
            "it stays at the top"
        );
    }

    #[test]
    fn dragging_over_the_conversation_copies_its_text_only() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.push(
            bob,
            Line::message(1_700_000_000, "bob".into(), "первая строка".into(), true, false),
        );
        app.push(
            bob,
            Line::message(1_700_000_010, "bob".into(), "вторая строка".into(), true, false),
        );
        screen(&mut app, 100, 20);
        let row_of = |app: &App, text: &str| {
            app.areas
                .message_rows
                .iter()
                .find(|(_, t)| t.contains(text))
                .unwrap()
                .0
        };
        let first = row_of(&app, "первая");
        let second = row_of(&app, "вторая");
        let mouse = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };

        // From "строка" of the first through "вторая" of the second.
        let start = first.x + 2 + "первая ".chars().count() as u16;
        let end = second.x + 2 + "вторая".chars().count() as u16 - 1;
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), start, first.y));
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end, second.y));
        let copied = app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end, second.y));
        assert_eq!(copied, [Action::Copy("строка\nвторая".into())]);

        // What is selected shows so.
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut pictures = Pictures::new();
        terminal.draw(|f| draw(f, &mut app, &mut pictures)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(
            buffer[(start, first.y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert!(
            !buffer[(start - 2, first.y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );

        // A click, not a drag, copies nothing and clears the selection.
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), start, first.y));
        assert!(
            app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), start, first.y))
                .is_empty()
        );
        assert!(app.selection.is_none());

        // The middle button and Ctrl+V paste from the desktop's clipboards.
        assert_eq!(
            app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Middle), start, first.y)),
            [Action::PastePrimary]
        );
        assert_eq!(app.focus, Focus::Input);
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            [Action::PasteClipboard]
        );

        // The leader and m give the mouse to the terminal, and back.
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(!app.mouse && app.status.contains("терминал"));
    }

    #[test]
    fn a_peers_online_clients_show_in_the_chat_and_ones_own_in_the_server_pane() {
        use omtalk_xmpp::presence::{Online, State};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 34);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        assert!(screen(&mut app, 120, 24).contains("не в сети"));
        let client = |who: &str, resource: &str, name: &str, state| Online {
            account: jid(who),
            resource: resource.into(),
            state,
            client: name.into(),
            status: None,
        };
        app.presence(
            &me,
            client("bob@home.org", "gajim.1", "Gajim", Some(State::Away)),
            1,
        );
        app.presence(
            &me,
            client("bob@home.org", "Monal-iOS.2", "Monal", Some(State::Online)),
            1,
        );
        let text = screen(&mut app, 120, 24);
        assert!(
            text.contains("bob@home.org · Monal, Gajim"),
            "the most available first: {text}"
        );
        assert!(
            text.contains("Gajim  отошёл") && text.contains("Monal  в сети"),
            "{text}"
        );

        // Gone: off the list.
        app.presence(&me, client("bob@home.org", "gajim.1", "Gajim", None), 2);
        app.presence(&me, client("bob@home.org", "Monal-iOS.2", "Monal", None), 2);
        assert!(screen(&mut app, 120, 24).contains("не в сети"));

        // One's own clients: in the server pane, and their coming and going in the channel.
        app.presence(
            &me,
            client("me@home.org", "gajim.3", "Gajim", Some(State::Online)),
            3,
        );
        let server = app.server_chat(&me);
        assert!(
            app.chats[server]
                .lines
                .iter()
                .any(|l| l.text.contains("свой клиент: Gajim (gajim.3) в сети"))
        );
        app.select(server);
        let text = screen(&mut app, 120, 24);
        assert!(
            text.contains("свои клиенты") && text.contains("Gajim  в сети"),
            "{text}"
        );
        app.presence(
            &me,
            client("me@home.org", "gajim.3", "Gajim", Some(State::Dnd)),
            4,
        );
        assert_eq!(app.chats[server].lines.len(), 1, "a change of state is not news");
    }

    #[test]
    fn chats_close_by_command_leader_and_middle_click() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let anna = app.chat(&me, &jid("anna@home.org"), ChatKind::Direct);
        app.chat(&me, &jid("bert@home.org"), ChatKind::Direct);
        app.chat(&me, &jid("carl@home.org"), ChatKind::Direct);
        app.select(anna);
        app.focus = Focus::Input;
        "/close"
            .chars()
            .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
        let closed = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            closed,
            [Action::CloseChat {
                account: me.clone(),
                jid: jid("anna@home.org")
            }]
        );
        let titles = |app: &App| app.chats.iter().map(|c| c.title()).collect::<Vec<_>>();
        assert_eq!(titles(&app), ["bert", "carl"]);
        assert_eq!(app.current().unwrap().title(), "bert", "the next one is open");

        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
        assert_eq!(titles(&app), ["carl"]);

        // The middle button on a chat in the list closes it.
        let dave = app.chat(&me, &jid("dave@home.org"), ChatKind::Direct);
        screen(&mut app, 100, 20);
        let (row, _) = *app.areas.chat_rows.iter().find(|(_, i)| *i == dave).unwrap();
        let middle = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Middle),
            column: row.x + 2,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            app.on_mouse(middle),
            [Action::CloseChat {
                account: me.clone(),
                jid: jid("dave@home.org")
            }]
        );
        assert_eq!(titles(&app), ["carl"]);

        // A room is left (it goes when the room says so); the server channel stays.
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        let actions = app.close_chat(room);
        assert!(actions.contains(&Action::Leave {
            account: me.clone(),
            room: jid("team@muc.home.org"),
            nick: "me".into()
        }));
        let server = app.server_chat(&me);
        assert!(app.close_chat(server).is_empty());
        assert!(app.status.contains("не закрывается"));
    }

    #[test]
    fn a_file_is_sent_by_command_drop_or_paperclip_and_shows_its_progress() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("кот.png");
        std::fs::write(&file, b"not really a png").unwrap();
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        let typed = |app: &mut App, text: &str| {
            app.focus = Focus::Input;
            text.chars()
                .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
            app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        };

        assert!(typed(&mut app, "/upload /nowhere/nothing.png").is_empty());
        assert!(app.status.contains("нет такого файла"), "{}", app.status);

        let sent = typed(&mut app, &format!("/upload {}", file.display()));
        let [
            Action::Upload {
                path,
                encrypted: true,
                id,
                ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}")
        };
        assert_eq!(path, &file);
        let id = id.clone();
        assert!(screen(&mut app, 100, 20).contains("кот.png ⇡ 0%"));
        app.delivery(&me, &id, Delivery::Uploading(40));
        assert!(screen(&mut app, 100, 20).contains("кот.png ⇡ 40%"));
        let link = "aesgcm://up.home.org/x/кот.png#000102030405060708090a0b000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        app.uploaded(&me, &id, link);
        app.delivery(&me, &id, Delivery::Sent);
        let line = app.chats[bob].lines.last().unwrap();
        assert_eq!(
            (line.text.as_str(), &line.delivery),
            (link, &Some(Delivery::Sent))
        );
        assert!(
            line.media.as_ref().is_some_and(|m| m.image),
            "a picture, as any shared one"
        );

        // A file dropped on the terminal arrives as its path: offered, not sent.
        app.input.clear();
        app.on_paste(&format!("'{}'\n", file.display()));
        assert_eq!(app.input, format!("/upload {}", file.display()));
        app.input.clear();
        app.on_paste(&format!(
            "file://{}",
            file.display().to_string().replace("кот", "%D0%BA%D0%BE%D1%82")
        ));
        assert_eq!(
            app.input,
            format!("/upload {}", file.display()),
            "file:// with escapes too"
        );
        app.input.clear();
        app.on_paste("просто текст");
        assert_eq!(app.input, "просто текст");

        // The paperclip starts the command.
        app.input.clear();
        screen(&mut app, 100, 20);
        let clip = app.areas.attach_button;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: clip.x + 1,
            row: clip.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.input, "/upload ");
    }
}
