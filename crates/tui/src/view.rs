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
    frame.render_widget(
        Block::default().style(Style::default().fg(theme::text()).bg(theme::background())),
        area,
    );

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
        side(frame, app, right, pictures);
    } else {
        strip(frame, right, "‹");
        app.areas.show_right = Some(right);
    }
    status_line(frame, app, status);
    services_window(frame, app, area);
    keys_window(frame, app, area);
    settings_window(frame, app, area);
}

fn strip(frame: &mut Frame, area: Rect, arrow: &str) {
    let mid = Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    };
    frame.render_widget(
        Paragraph::new(arrow).style(Style::default().fg(theme::dim())),
        mid,
    );
}

fn pane(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(focused))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ))
}

fn chats(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = pane("omtalk", app.focus == Focus::Chats).title(
        TextLine::from(Span::styled(" ‹ ", Style::default().fg(theme::dim()))).alignment(Alignment::Right),
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.areas.left = Some(area);
    app.areas.hide_left = Some(Rect::new(area.x + area.width.saturating_sub(4), area.y, 3, 1));

    let mut y = inner.y;
    let rows = app.tree();
    // Longer than the pane: scrolled so that the chat picked is in sight, and no further
    // than the end.
    let height = inner.height as usize;
    let picked = rows
        .iter()
        .position(|(entry, _)| matches!(entry, Entry::Chat(i) if Some(*i) == app.selected));
    let mut scroll = app.chats_scroll;
    if let Some(row) = picked {
        if row < scroll {
            scroll = row;
        } else if row >= scroll + height {
            scroll = row + 1 - height;
        }
    }
    scroll = scroll.min(rows.len().saturating_sub(height));
    app.chats_scroll = scroll;
    // Whether the branch at each depth goes on below the current row.
    let mut open: Vec<bool> = Vec::new();
    // Whether the heading at each depth is a folder: its chats hang from under its
    // symbol, a step in from where the folder's own branch is.
    let mut folder: Vec<bool> = Vec::new();
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
        for level in 1..=depth {
            if folder.get(level - 1).copied().unwrap_or(false) {
                branch.push_str("  ");
            }
            branch.push_str(if level < depth {
                if open.get(level).copied().unwrap_or(false) {
                    "│ "
                } else {
                    "  "
                }
            } else if last {
                "└ "
            } else {
                "├ "
            });
        }
        open.resize(depth + 1, false);
        open[depth] = !last;
        folder.resize(depth + 1, false);
        folder[depth] = matches!(entry, Entry::Heading(Group::Folder(..)));
        // Above the scroll: counted for the branches, not drawn.
        if at < scroll {
            continue;
        }
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
                        !app.typing_now(
                            &app.chats[index].account,
                            &app.chats[index].jid,
                            chrono::Utc::now().timestamp(),
                        )
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
            TextLine::from(Span::styled(
                tr!("no chats", "нет чатов"),
                Style::default().fg(theme::dim()),
            )),
            TextLine::from(Span::styled(
                tr!("/msg or /join", "/msg или /join"),
                Style::default().fg(theme::dim()),
            )),
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
    let online = app
        .connection
        .get(account)
        .is_some_and(crate::app::Connection::online);
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
        Span::styled(mark, Style::default().fg(theme::dim())),
        Span::styled(
            "● ",
            Style::default().fg(if online { theme::ok() } else { theme::dim() }),
        ),
        Span::styled(name, {
            // The account commands go to stands out.
            let style = Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD);
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
                .fg(theme::accent())
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
        Span::styled(branch.to_owned(), Style::default().fg(theme::border_colour())),
        Span::styled(mark, Style::default().fg(theme::dim())),
        Span::styled(icon, Style::default().fg(theme::accent())),
        Span::styled(
            name,
            Style::default().fg(theme::text()).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::accent())
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
    typing: bool,
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
    // Someone types there: a mark after the name.
    let lock = if typing {
        format!(" …{lock}")
    } else {
        lock.to_owned()
    };
    let room_for_title = (width as usize).saturating_sub(icon.width() + badge.width() + lock.width() + 1);
    let title = truncate(&chat.title(), room_for_title);
    let gap = (width as usize).saturating_sub(icon.width() + title.width() + lock.width() + badge.width());
    let base = if selected {
        theme::selected()
    } else if chat.unread > 0 {
        Style::default().fg(theme::text()).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::text())
    };
    Paragraph::new(TextLine::from(vec![
        Span::styled(branch.to_owned(), Style::default().fg(theme::border_colour())),
        Span::styled(
            icon,
            if selected {
                base
            } else {
                // A person's mark is green while any of their clients is online.
                base.fg(match chat.kind {
                    ChatKind::Room { .. } => theme::accent(),
                    ChatKind::Direct if online => theme::ok(),
                    _ => theme::dim(),
                })
            },
        ),
        Span::styled(title, base),
        // On the selected row the colours are swapped: a coloured lock would turn into a
        // coloured block there.
        Span::styled(
            lock,
            if selected {
                base
            } else if typing {
                base.fg(theme::accent())
            } else {
                base.fg(theme::warn())
            },
        ),
        Span::styled(" ".repeat(gap), base),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::accent())
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
                tr!("offline", "не в сети").to_owned()
            } else {
                clients.join(", ")
            };
            (chat.title(), format!("{} · {seen}", chat.jid))
        }
        // A room: its topic beside its name, else its address.
        Some(chat) if chat.is_room() => {
            let room = chat.topic.clone().unwrap_or_else(|| chat.jid.to_string());
            (chat.title(), truncate(&room, (area.width as usize / 2).max(10)))
        }
        Some(chat) => (chat.title(), chat.jid.to_string()),
        None => ("omtalk".to_owned(), String::new()),
    };
    let scrolled = app.current().is_some_and(|c| c.scroll > 0);
    let mut block = pane(&title, app.focus == Focus::Messages).title(
        TextLine::from(Span::styled(
            format!(" {subtitle} "),
            Style::default().fg(theme::dim()),
        ))
        .alignment(Alignment::Right),
    );
    if scrolled {
        block = block.title_bottom(
            TextLine::from(Span::styled(" ↓ End ", Style::default().fg(theme::accent())))
                .alignment(Alignment::Center),
        );
    }
    // Who is typing here, at the foot of the conversation.
    if let Some(chat) = app.current() {
        let names = app.typing_now(&chat.account, &chat.jid, chrono::Utc::now().timestamp());
        if !names.is_empty() {
            let verb = if names.len() == 1 {
                tr!("is typing", "печатает")
            } else {
                tr!("are typing", "печатают")
            };
            block = block.title_bottom(
                TextLine::from(Span::styled(
                    format!(" {} {verb}… ", names.join(", ")),
                    Style::default().fg(theme::accent()),
                ))
                .alignment(Alignment::Left),
            );
        }
    }
    let inner = block.inner(messages);
    frame.render_widget(block, messages);

    match app.selected {
        Some(index) => {
            let width = inner.width as usize;
            let ready = |url: &str| pictures.contains_key(url);
            let chat = &app.chats[index];
            // "/me waves" from here names one as others see one: the nick in a room,
            // the account's name in a direct chat.
            let own = match &chat.kind {
                ChatKind::Room { nick } if !nick.is_empty() => nick.clone(),
                _ => chat
                    .account
                    .node()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| chat.account.to_string()),
            };
            let rows = message_rows(&chat.lines, width, app.icons, &ready, &own);
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
                if let Some(line) = row.line {
                    app.areas
                        .message_lines
                        .push((Rect::new(inner.x, y, inner.width, 1), line));
                }
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
                                format!("  {}", picture.name),
                                Style::default().fg(theme::dim()),
                            ));
                        }
                        _ => {}
                    }
                } else if let Some(link) = &row.fetch {
                    app.areas
                        .fetches
                        .push((Rect::new(inner.x, y, inner.width, 1), link.clone()));
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
    replying(frame, app, input);
    suggestions(frame, app, input, messages);
    leader_keys(frame, app, input);
}

/// A client's presence, in words.
fn state_words(state: omtalk_xmpp::presence::State) -> &'static str {
    use omtalk_xmpp::presence::State;
    match state {
        State::Chat => tr!("free to chat", "готов общаться"),
        State::Online => tr!("online", "в сети"),
        State::Away => tr!("away", "отошёл"),
        State::Dnd => tr!("do not disturb", "не беспокоить"),
        State::Xa => tr!("away for long", "давно отошёл"),
    }
}

/// Connected clients, one a row: the program, its state, its own words.
fn client_rows(rows: &mut Vec<TextLine<'static>>, clients: &[&omtalk_xmpp::presence::Online], width: u16) {
    use omtalk_xmpp::presence::State;
    for client in clients {
        let state = client.state.unwrap_or(State::Online);
        let colour = match state {
            State::Chat | State::Online => theme::ok(),
            State::Away | State::Xa => theme::accent(),
            State::Dnd => theme::warn(),
        };
        let mut words = state_words(state).to_owned();
        if let Some(status) = &client.status {
            words = format!("{words} · {status}");
        }
        let name = if client.client.is_empty() {
            tr!("client", "клиент")
        } else {
            client.client.as_str()
        };
        let line = format!("{name}  {words}");
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

/// Empties an area for a window over the rest, in the theme's background.
fn clear(frame: &mut Frame, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().fg(theme::text()).bg(theme::background())),
        area,
    );
}

/// Just above the input, the message a reply answers, while it is written.
fn replying(frame: &mut Frame, app: &App, input: Rect) {
    let Some(reply) = &app.replying else { return };
    if input.y == 0 {
        return;
    }
    let place = Rect::new(input.x + 1, input.y - 1, input.width.saturating_sub(2), 1);
    let quote = reply.quote.lines().next().unwrap_or_default();
    let line = TextLine::from(vec![
        Span::styled(
            format!("↩ {}: ", reply.who),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate(
                quote,
                (place.width as usize).saturating_sub(reply.who.width() + 16),
            ),
            Style::default().fg(theme::dim()),
        ),
        Span::styled(
            tr!("  Esc cancels", "  Esc — отменить"),
            Style::default().fg(theme::dim()),
        ),
    ]);
    clear(frame, place);
    frame.render_widget(Paragraph::new(line), place);
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
    let left = LEADER_KEYS.iter().map(|(k, ..)| k.width()).max().unwrap_or(0);
    let width = (LEADER_KEYS
        .iter()
        .map(|(_, en, ru)| left + 3 + if crate::lang::russian() { ru } else { en }.width())
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
            Style::default().fg(theme::accent()),
        ));
    let inner = block.inner(area);
    clear(frame, area);
    frame.render_widget(block, area);
    let lines: Vec<TextLine> = LEADER_KEYS
        .iter()
        .map(|(key, en, ru)| {
            let about = if crate::lang::russian() { ru } else { en };
            TextLine::from(vec![
                Span::styled(
                    format!("{key:<left$}"),
                    Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   {about}"), Style::default().fg(theme::dim())),
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
                let about = complete::about(&text).map(|c| c.about()).unwrap_or("");
                (text, about, i == c.index)
            })
            .collect(),
        _ => complete::commands_for(&app.input)
            .into_iter()
            .map(|c| {
                (
                    format!("{} {}", c.name, c.args()).trim_end().to_owned(),
                    c.about(),
                    false,
                )
            })
            .collect(),
    };
    if rows.is_empty() || app.focus != Focus::Input {
        return;
    }
    // As tall as the commands need, up to the chat's height: all of them for /help.
    let height = (rows.len() as u16 + 2).min(above.height.saturating_sub(1));
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
        .title(Span::styled(
            tr!(" Tab completes ", " Tab — дописать "),
            Style::default().fg(theme::dim()),
        ));
    let inner = block.inner(area);
    clear(frame, area);
    frame.render_widget(block, area);
    let room = (inner.width as usize).saturating_sub(left + 3);
    let lines: Vec<TextLine> = rows
        .into_iter()
        .map(|(text, about, chosen)| {
            let style = if chosen {
                theme::selected()
            } else {
                Style::default().fg(theme::accent())
            };
            TextLine::from(vec![
                Span::styled(format!("{text:<left$}"), style),
                Span::styled(
                    format!("   {}", truncate(about, room)),
                    Style::default().fg(theme::dim()),
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
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        )),
        TextLine::from(""),
        TextLine::from(Span::styled(
            tr!("/msg name@server to write", "/msg имя@сервер — написать"),
            Style::default().fg(theme::dim()),
        )),
        TextLine::from(Span::styled(
            tr!(
                "/join room@conference.server to join a group",
                "/join комната@conference.сервер — войти в группу"
            ),
            Style::default().fg(theme::dim()),
        )),
        TextLine::from(Span::styled(
            tr!("/help for all commands", "/help — все команды"),
            Style::default().fg(theme::dim()),
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
    /// A file not fetched yet: a click on this row fetches it, from this link.
    pub fetch: Option<String>,
    /// Which message (its index among the chat's lines) this row belongs to.
    pub line: Option<usize>,
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
            fetch: None,
            line: None,
        }
    }
}

pub fn message_rows(
    lines: &[Line],
    width: usize,
    icons: theme::Icons,
    ready: &dyn Fn(&str) -> bool,
    own: &str,
) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut last_day = None;
    let mut last_who: Option<(&str, bool, i64)> = None;
    let body_width = width.saturating_sub(2).max(1);
    // Where each message's rows begin, to tell afterwards which rows are whose.
    let mut starts = Vec::with_capacity(lines.len());
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
                        Style::default().fg(theme::border_colour()),
                    ))
                    .into(),
                );
            }
            last_day = day;
            last_who = None;
        }
        starts.push(rows.len());
        let clock = time.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
        if line.notice {
            for text in wrap(&line.text, width.saturating_sub(4).max(1)) {
                rows.push(
                    TextLine::from(Span::styled(
                        format!("  · {text}"),
                        Style::default().fg(theme::dim()).add_modifier(Modifier::ITALIC),
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
                Style::default()
                    .fg(theme::outgoing())
                    .add_modifier(Modifier::BOLD)
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
                Span::styled(format!("  {clock}"), Style::default().fg(theme::dim())),
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
                        fetch: None,
                        line: None,
                    });
                }
                push_delivery(&mut rows, line, body_width, true);
                continue;
            }
            let (icon, state, colour) = match &slot.state {
                MediaState::Offered => (
                    if slot.image { icons.picture } else { icons.file },
                    tr!("download", "скачать").to_owned(),
                    theme::accent(),
                ),
                MediaState::Downloading(percent) => (
                    if slot.image { icons.picture } else { icons.file },
                    trf!("downloading {percent}%", "загружается {percent}%"),
                    theme::dim(),
                ),
                MediaState::Loading => (
                    if slot.image { icons.picture } else { icons.file },
                    tr!("downloading…", "загружается…").to_owned(),
                    theme::dim(),
                ),
                MediaState::Ready(_) if slot.image => {
                    (icons.picture, tr!("open", "открыть").to_owned(), theme::accent())
                }
                MediaState::Ready(_) => (icons.file, tr!("open", "открыть").to_owned(), theme::accent()),
                MediaState::Failed(e) => (
                    icons.file,
                    trf!("not downloaded: {e}", "не загрузилось: {e}"),
                    theme::warn(),
                ),
            };
            let mut spans = vec![Span::raw("  ")];
            if !line.encrypted {
                spans.push(Span::styled(marker, Style::default().fg(theme::warn())));
            }
            spans.push(Span::styled(
                format!("{icon} {} ", slot.name),
                Style::default().fg(theme::text()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(state, Style::default().fg(colour)));
            rows.push(Row {
                text: TextLine::from(spans),
                open: path,
                picture: None,
                who: None,
                fetch: (slot.state == MediaState::Offered).then(|| slot.link.clone()),
                line: None,
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
        // An action, "/me waves": "* bob waves", as IRC and Gajim show it (XEP-0245).
        if let Some(action) = crate::app::action_of(&line.text) {
            let who_style = if line.outgoing {
                Style::default()
                    .fg(theme::outgoing())
                    .add_modifier(Modifier::BOLD)
            } else {
                theme::nick(&line.who)
            };
            let text_style = Style::default().fg(theme::text()).add_modifier(Modifier::ITALIC);
            let who = if line.outgoing { own } else { line.who.as_str() };
            let lead = format!("* {who} ");
            let mut first = true;
            for part in wrap(&format!("{lead}{action}"), text_width) {
                let mut spans = vec![Span::raw("  ")];
                if !line.encrypted {
                    let mark = if first {
                        marker.to_owned()
                    } else {
                        " ".repeat(marker.width())
                    };
                    spans.push(Span::styled(mark, Style::default().fg(theme::warn())));
                }
                match part.strip_prefix(&lead).filter(|_| first) {
                    Some(rest) => {
                        spans.push(Span::styled("* ", text_style));
                        spans.push(Span::styled(
                            who.to_owned(),
                            who_style.add_modifier(Modifier::ITALIC),
                        ));
                        spans.push(Span::styled(format!(" {rest}"), text_style));
                    }
                    None => spans.push(Span::styled(part, text_style)),
                }
                rows.push(TextLine::from(spans).into());
                first = false;
            }
            push_delivery(&mut rows, line, body_width, false);
            continue;
        }
        // A reply: whom it answers, if that message is here, then its quote as a block.
        if let Some(answered) = &line.reply_to
            && let Some(original) = lines
                .iter()
                .find(|l| l.id.as_ref() == Some(answered) || l.archive_id.as_ref() == Some(answered))
        {
            rows.push(
                TextLine::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("↩ {}", original.who),
                        Style::default().fg(theme::accent()),
                    ),
                ])
                .into(),
            );
        }
        let (mut quote, said) = split_quote(&line.text);
        // "Алиса:" opening the quote says what the header says already.
        if let Some(answered) = &line.reply_to
            && let Some(original) = lines
                .iter()
                .find(|l| l.id.as_ref() == Some(answered) || l.archive_id.as_ref() == Some(answered))
            && quote
                .first()
                .is_some_and(|first| *first == format!("{}:", original.who))
        {
            quote.remove(0);
        }
        let quoted = Style::default().fg(theme::dim());
        for q in &quote {
            for styled in styled_wrap(q, text_width.saturating_sub(2).max(1), quoted) {
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled("▎ ", Style::default().fg(theme::accent())),
                ];
                spans.extend(styled);
                rows.push(TextLine::from(spans).into());
            }
        }
        // The rest as XEP-0393 has it: styled text, quotes, preformatted blocks.
        for block in crate::styling::blocks(said) {
            let (lead, parts): (Vec<Span<'static>>, Vec<Vec<Span<'static>>>) = match block {
                crate::styling::Block::Text(text) => (
                    Vec::new(),
                    styled_wrap(text, text_width, Style::default().fg(theme::text())),
                ),
                crate::styling::Block::Quote(depth, text) => (
                    vec![Span::styled(
                        "▎ ".repeat(depth),
                        Style::default().fg(theme::accent()),
                    )],
                    styled_wrap(text, text_width.saturating_sub(2 * depth).max(1), quoted),
                ),
                crate::styling::Block::Pre(text) => (
                    vec![Span::raw("  ")],
                    wrap(text, text_width.saturating_sub(2).max(1))
                        .into_iter()
                        .map(|t| vec![Span::styled(t, Style::default().fg(theme::accent()))])
                        .collect(),
                ),
                crate::styling::Block::Fence(text) => (
                    Vec::new(),
                    wrap(text, text_width)
                        .into_iter()
                        .map(|t| vec![Span::styled(t, Style::default().fg(theme::dim()))])
                        .collect(),
                ),
            };
            for styled in parts {
                let mut spans = vec![Span::raw("  ")];
                if !line.encrypted && first {
                    spans.push(Span::styled(marker, Style::default().fg(theme::warn())));
                } else if !line.encrypted {
                    spans.push(Span::raw(" ".repeat(marker.width())));
                }
                // From a device refused or not decided on.
                if line.untrusted && first {
                    spans.push(Span::styled(
                        tr!("⚠ unverified device · ", "⚠ непроверенное устройство · "),
                        Style::default().fg(theme::warn()),
                    ));
                }
                spans.extend(lead.iter().cloned());
                spans.extend(styled);
                rows.push(TextLine::from(spans).into());
                first = false;
            }
        }
        push_delivery(&mut rows, line, body_width, false);
    }
    // Every row of a message knows which message it is.
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(rows.len());
        for row in &mut rows[*start..end] {
            row.line = Some(index);
        }
    }
    rows
}

/// [`split_quote`] for the app: a reply does not quote a quote.
pub fn split_quote_pub(text: &str) -> (Vec<&str>, &str) {
    split_quote(text)
}

/// A message's leading "> " lines (a quote, as replies begin) apart from the rest.
fn split_quote(text: &str) -> (Vec<&str>, &str) {
    let mut quote = Vec::new();
    let mut rest = text;
    while let Some(line) = rest.split_inclusive('\n').next() {
        let Some(quoted) = line
            .strip_prefix("> ")
            .or_else(|| (line.trim_end() == ">").then_some(""))
        else {
            break;
        };
        if !line.ends_with('\n') {
            break;
        }
        quote.push(quoted.trim_end_matches('\n'));
        rest = &rest[line.len()..];
    }
    (quote, rest)
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
    let dim = Style::default().fg(theme::dim());
    match delivery {
        Delivery::Uploading(percent) => (format!("⇡ {percent}%"), dim),
        Delivery::Sending => ("⋯".into(), dim),
        Delivery::Queued => (tr!("⋯ waiting for the connection", "⋯ ждёт связи").into(), dim),
        Delivery::Sent => ("✓".into(), dim),
        Delivery::Delivered => ("✓✓".into(), dim),
        Delivery::Read => (
            "✓✓".into(),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ),
        Delivery::Failed(reason) => (
            trf!("✗ not sent: {reason}", "✗ не отправлено: {reason}"),
            Style::default().fg(theme::warn()),
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

/// `text` (one line of a message) wrapped to `width`, each row as spans styled by
/// XEP-0393 over `base`: strong bold, emphasis italic, strike crossed out, code in the
/// accent colour, the directives dimmed.
fn styled_wrap(text: &str, width: usize, base: Style) -> Vec<Vec<Span<'static>>> {
    let marks = crate::styling::marks(text);
    let chars: Vec<char> = text.chars().collect();
    let style = |m: crate::styling::Mark| {
        let mut style = base;
        if m.directive {
            return style.fg(theme::dim());
        }
        if m.strong {
            style = style.add_modifier(Modifier::BOLD);
        }
        if m.emphasis {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if m.strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if m.code {
            style = style.fg(theme::accent());
        }
        style
    };
    let mut at = 0;
    wrap(text, width)
        .into_iter()
        .map(|row| {
            // Where wrap broke the line, the spaces it dropped.
            if !row.is_empty() && !row.starts_with(' ') {
                while chars.get(at) == Some(&' ') {
                    at += 1;
                }
            }
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut run = String::new();
            let mut run_mark = None;
            for c in row.chars() {
                let mark = marks.get(at).copied().unwrap_or_default();
                at += 1;
                if run_mark.is_some_and(|m| m != mark) {
                    spans.push(Span::styled(std::mem::take(&mut run), style(run_mark.unwrap())));
                }
                run_mark = Some(mark);
                run.push(c);
            }
            if let Some(mark) = run_mark {
                spans.push(Span::styled(run, style(mark)));
            }
            spans
        })
        .collect()
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
        (app.icons.locked, theme::ok())
    } else {
        (app.icons.unlocked, theme::warn())
    };
    let button = format!(" {label} ");
    let attach = format!(" {} ", app.icons.attach);
    // The hint as long as the border has room for beside the lock.
    let hint = [
        tr!(
            " Enter ⏎ · Alt+Enter new line ",
            " Enter ⏎ · Alt+Enter — новая строка "
        ),
        tr!(" ⏎ · Alt+⏎ line ", " ⏎ · Alt+⏎ строка "),
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
        .title(Span::styled(attach.clone(), Style::default().fg(theme::accent())))
        .title(
            TextLine::from(Span::styled(hint, Style::default().fg(theme::dim()))).alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    clear(frame, area);
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
            ""
        } else {
            tr!(
                "/msg name@server or /join room",
                "/msg имя@сервер или /join комната"
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(theme::dim()))),
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

/// How wide an avatar is drawn, in cells; half as many rows make it square.
const AVATAR_WIDTH: u16 = 12;

fn side(frame: &mut Frame, app: &mut App, area: Rect, pictures: &mut Pictures) {
    let title = match app.current() {
        Some(chat) if chat.is_room() => trf!("Participants {}", "Участники {}", chat.occupants.len()),
        Some(chat) if chat.kind == ChatKind::Server => tr!("Account", "Аккаунт").to_owned(),
        Some(_) => tr!("Contact", "Собеседник").to_owned(),
        None => "—".to_owned(),
    };
    let picking = app.focus == Focus::People;
    let block = pane(&title, picking).title(
        TextLine::from(Span::styled(" › ", Style::default().fg(theme::dim()))).alignment(Alignment::Right),
    );
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    app.areas.right = Some(area);
    app.areas.hide_right = Some(Rect::new(area.x + area.width.saturating_sub(4), area.y, 3, 1));

    let Some(chat) = app.current() else { return };
    // The avatar on top, round, the rest below it.
    let width = AVATAR_WIDTH.min(inner.width);
    let height = width / 2;
    // A row of air above it, as below.
    let place = Rect::new(inner.x + (inner.width - width) / 2, inner.y + 1, width, height);
    let fits = inner.height > height + 5;
    let drawn = match app.avatar_of(&chat.jid) {
        Some(key) if fits => match (app.avatar_blocks.get(&key), pictures.get_mut(&key)) {
            (Some(image), _) => {
                crate::avatar::draw_blocks(image, place, frame.buffer_mut());
                true
            }
            (None, Some(protocol)) => {
                frame.render_stateful_widget(StatefulImage::default(), place, protocol);
                true
            }
            (None, None) => false,
        },
        _ => false,
    };
    if drawn {
        inner.y += height + 2;
        inner.height -= height + 2;
    }
    let mut rows: Vec<TextLine> = Vec::new();
    let mut nicks = Vec::new();
    let mut devices_from = None;
    let mut people = Vec::new();
    let mut picked_row = None;
    match &chat.kind {
        ChatKind::Room { .. } => {
            // What the room is: its topic in full, and its address.
            if let Some(topic) = &chat.topic {
                for part in wrap(topic, inner.width as usize) {
                    rows.push(TextLine::from(Span::styled(
                        part,
                        Style::default().fg(theme::text()),
                    )));
                }
            }
            rows.push(TextLine::from(Span::styled(
                chat.jid.to_string(),
                Style::default().fg(theme::dim()),
            )));
            rows.push(TextLine::default());
            // The participant picked with the keys, counted as the pane lists them.
            let mut person = 0;
            for (nick, real) in &chat.occupants {
                let y = inner.y + rows.len() as u16;
                nicks.push((Rect::new(inner.x, y, inner.width, 1), nick.clone()));
                let picked = picking && person == app.people_cursor;
                if picked {
                    picked_row = Some(rows.len());
                }
                person += 1;
                let mut spans = vec![
                    Span::styled("● ", Style::default().fg(theme::ok())),
                    Span::styled(nick.clone(), theme::nick(nick)),
                ];
                if let Some(real) = real
                    && real.node().is_some_and(|n| n.as_str() != nick)
                {
                    spans.push(Span::styled(
                        format!(" {real}"),
                        Style::default().fg(theme::dim()),
                    ));
                }
                let line = TextLine::from(spans);
                rows.push(if picked {
                    line.style(theme::selected())
                } else {
                    line
                });
            }
            // Members who are away, below: their account's name, a click away from a chat.
            let away = app.selected.map(|i| app.away_members(i)).unwrap_or_default();
            if !away.is_empty() {
                rows.push(TextLine::default());
                rows.push(TextLine::from(Span::styled(
                    trf!("offline {}", "не в сети {}", away.len()),
                    Style::default().fg(theme::dim()),
                )));
                for member in away {
                    let y = inner.y + rows.len() as u16;
                    people.push((Rect::new(inner.x, y, inner.width, 1), member.clone()));
                    let picked = picking && person == app.people_cursor;
                    if picked {
                        picked_row = Some(rows.len());
                    }
                    person += 1;
                    let name = member
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| member.to_string());
                    let line = TextLine::from(vec![
                        Span::styled("○ ", Style::default().fg(theme::dim())),
                        Span::styled(name, Style::default().fg(theme::dim())),
                        Span::styled(
                            format!(" {}", member.domain()),
                            Style::default().fg(theme::border_colour()),
                        ),
                    ]);
                    rows.push(if picked {
                        line.style(theme::selected())
                    } else {
                        line
                    });
                }
            }
            // What can be done with the room, so nobody has to look for it.
            rows.push(TextLine::default());
            for command in [
                tr!("/topic topic", "/topic тема"),
                tr!("/name title", "/name название"),
                tr!("/invite address", "/invite адрес"),
                tr!("/nick nick", "/nick ник"),
                "/commands",
                "/close",
            ] {
                rows.push(TextLine::from(Span::styled(
                    command,
                    Style::default().fg(theme::accent()),
                )));
            }
        }
        ChatKind::Server => {
            let state = app
                .connection
                .get(&chat.account)
                .cloned()
                .unwrap_or(crate::app::Connection::Connecting);
            let colour = if state.online() {
                theme::ok()
            } else {
                theme::warn()
            };
            rows.push(TextLine::from(vec![
                Span::styled("● ", Style::default().fg(colour)),
                Span::styled(
                    chat.account.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            rows.push(TextLine::from(Span::styled(
                format!("  {}", state.words()),
                Style::default().fg(theme::dim()),
            )));
            rows.push(TextLine::default());
            let chats = app.chats.iter().filter(|c| c.account == chat.account);
            let rooms = chats.clone().filter(|c| c.is_room()).count();
            let direct = chats.filter(|c| c.kind == ChatKind::Direct).count();
            let label = |name: &str, value: String| {
                TextLine::from(vec![
                    Span::styled(format!("{name:<8}"), Style::default().fg(theme::dim())),
                    Span::raw(value),
                ])
            };
            rows.push(label(tr!("server", "сервер"), chat.account.domain().to_string()));
            rows.push(label(tr!("chats", "чаты"), direct.to_string()));
            rows.push(label(tr!("rooms", "комнаты"), rooms.to_string()));
            rows.push(TextLine::default());
            for command in [
                "/services",
                tr!("/add address", "/add адрес"),
                tr!("/join room", "/join комната"),
            ] {
                rows.push(TextLine::from(Span::styled(
                    command,
                    Style::default().fg(theme::accent()),
                )));
            }
            rows.push(TextLine::default());
            rows.push(TextLine::from(Span::styled(
                tr!("your clients", "свои клиенты"),
                Style::default().fg(theme::dim()),
            )));
            let own = app.own_resource.get(&chat.account);
            let mut clients: Vec<omtalk_xmpp::presence::Online> = app
                .clients(&chat.account, &chat.account)
                .into_iter()
                .cloned()
                .collect();
            for client in &mut clients {
                if Some(&client.resource) == own {
                    client.client = trf!("{} (this)", "{} (это)", client.client);
                }
            }
            client_rows(&mut rows, &clients.iter().collect::<Vec<_>>(), inner.width);
        }
        ChatKind::Direct => {
            rows.push(TextLine::from(Span::styled(
                tr!("online", "в сети"),
                Style::default().fg(theme::dim()),
            )));
            let clients = app.clients(&chat.account, &chat.jid);
            if clients.is_empty() {
                rows.push(TextLine::from(Span::styled(
                    tr!("  nobody", "  никого"),
                    Style::default().fg(theme::dim()),
                )));
            }
            client_rows(&mut rows, &clients, inner.width);
            rows.push(TextLine::default());
            devices_from = Some(rows.len());
            rows.push(TextLine::from(Span::styled(
                tr!("OMEMO devices · keys", "устройства OMEMO · ключи"),
                Style::default().fg(theme::dim()),
            )));
            let devices = match &chat.devices {
                None => {
                    rows.push(TextLine::from(Span::styled(
                        "…",
                        Style::default().fg(theme::dim()),
                    )));
                    &[][..]
                }
                Some(devices) if devices.is_empty() => {
                    rows.push(TextLine::from(Span::styled(
                        tr!("no OMEMO devices:", "нет OMEMO-устройств:"),
                        Style::default().fg(theme::warn()),
                    )));
                    rows.push(TextLine::from(Span::styled(
                        tr!("unencrypted: ^O", "без шифрования: ^O"),
                        Style::default().fg(theme::dim()),
                    )));
                    &[][..]
                }
                Some(devices) => devices.as_slice(),
            };
            for device in devices {
                let (dot, colour) = match (&device.fingerprint, device.trust) {
                    (None, _) => ("○", theme::dim()),
                    (Some(_), omtalk_omemo::Trust::Verified) => ("✓", theme::ok()),
                    (Some(_), omtalk_omemo::Trust::Blind) => ("●", theme::accent()),
                    (Some(_), _) => ("✗", theme::warn()),
                };
                let mut header = vec![
                    Span::styled(format!("{dot} "), Style::default().fg(colour)),
                    Span::styled(device.device.to_string(), Style::default().fg(theme::text())),
                    Span::styled(
                        format!(" {}", crate::keys::trust_words(device.trust)),
                        Style::default().fg(theme::dim()),
                    ),
                ];
                if device.this_device {
                    header.push(Span::styled(
                        tr!(" this device", " это устройство"),
                        Style::default().fg(theme::accent()),
                    ));
                }
                rows.push(TextLine::from(header));
                let detail = match &device.fingerprint {
                    Some(fp) => fp.clone(),
                    None => tr!("no keys", "нет ключей").to_owned(),
                };
                for part in wrap(&detail, inner.width.saturating_sub(2) as usize) {
                    rows.push(TextLine::from(Span::styled(
                        format!("  {part}"),
                        Style::default().fg(theme::dim()),
                    )));
                }
                if let Some(at) = device.seen.filter(|_| !device.this_device) {
                    let words = crate::keys::seen_words(at, chrono::Utc::now().timestamp());
                    for part in wrap(&words, inner.width.saturating_sub(2) as usize) {
                        rows.push(TextLine::from(Span::styled(
                            format!("  {part}"),
                            Style::default().fg(theme::dim()),
                        )));
                    }
                }
            }
        }
    }
    // The device list is a way into the keys window.
    if let Some(from) = devices_from {
        let top = inner.y + from as u16;
        if top < inner.bottom() {
            app.areas.side_devices = Some(Rect::new(inner.x, top, inner.width, inner.bottom() - top));
        }
    }
    // The picked participant kept in sight: the pane scrolls to it.
    let scroll = picked_row.map_or(0, |row| (row as u16 + 1).saturating_sub(inner.height));
    frame.render_widget(Paragraph::new(rows).scroll((scroll, 0)), inner);
    let shown = |r: &Rect| r.y >= inner.y + scroll && r.y < inner.bottom() + scroll;
    let up = |r: Rect| Rect { y: r.y - scroll, ..r };
    app.areas.nicks.extend(
        nicks
            .into_iter()
            .filter(|(r, _)| shown(r))
            .map(|(r, n)| (up(r), n)),
    );
    app.areas.people = people
        .into_iter()
        .filter(|(r, _)| shown(r))
        .map(|(r, p)| (up(r), p))
        .collect();
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
        Span::styled(text, Style::default().fg(theme::accent()))
    };
    let dim = |t: &str| Span::styled(t.to_owned(), Style::default().fg(theme::dim()));
    let hints = TextLine::from(vec![
        Span::styled(leader_label(app), Style::default().fg(theme::accent())),
        dim(tr!(" keys  ", " клавиши  ")),
        key("O"),
        dim(" OMEMO  "),
        Span::styled("Tab", Style::default().fg(theme::accent())),
        dim(tr!(" complete/focus  ", " дописать/фокус  ")),
        Span::styled("/", Style::default().fg(theme::accent())),
        dim(tr!(" commands  ", " команды  ")),
        key("Q"),
        dim(tr!(" quit ", " выход ")),
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
                    Style::default().fg(if state.online() {
                        theme::ok()
                    } else {
                        theme::warn()
                    }),
                )
            })
            .collect();
        spans.push(Span::raw(" "));
        let room = (area.width as usize).saturating_sub(spans.iter().map(|s| s.width()).sum());
        spans.push(Span::styled(
            truncate(&app.status, room),
            Style::default().fg(theme::warn()),
        ));
        frame.render_widget(Paragraph::new(TextLine::from(spans)), area);
        return;
    }
    let mut left = Vec::new();
    for (account, state) in &app.connection {
        let colour = if state.online() {
            theme::ok()
        } else {
            theme::warn()
        };
        left.push(Span::styled("● ", Style::default().fg(colour)));
        left.push(Span::styled(
            format!("{account} {}  ", state.words()),
            Style::default().fg(theme::dim()),
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
                    Style::default().fg(if state.online() {
                        theme::ok()
                    } else {
                        theme::warn()
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

/// The keys window, over the middle of the screen.
fn keys_window(frame: &mut Frame, app: &mut App, screen: Rect) {
    let Some(keys) = app.keys.as_ref() else { return };
    // Wide enough for a whole fingerprint (8 groups of 8) on one row.
    let width = screen.width.saturating_sub(4).min(82);
    let height = screen.height.saturating_sub(3).min(26);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height.saturating_sub(1) - height) / 2,
        width,
        height,
    };
    app.areas.key_window = area;
    clear(frame, area);
    let title = match &keys.peer {
        Some(peer) => trf!("OMEMO keys · {peer}", "Ключи OMEMO · {peer}"),
        None => tr!("OMEMO keys", "Ключи OMEMO").to_owned(),
    };
    let block = pane(&title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 20 {
        return;
    }
    let [body, status, buttons, hint] = Layout::vertical([
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

    // Each owner's devices under a heading; a device takes two rows: its state, its
    // fingerprint.
    let mut lines: Vec<(TextLine, Option<usize>)> = Vec::new();
    let mut owner: Option<&BareJid> = None;
    for (index, row) in keys.rows.iter().enumerate() {
        if owner != Some(&row.owner) {
            owner = Some(&row.owner);
            if !lines.is_empty() {
                lines.push((TextLine::default(), None));
            }
            let heading = if row.owner == keys.account {
                trf!("your devices · {}", "ваши устройства · {}", row.owner)
            } else {
                row.owner.to_string()
            };
            lines.push((
                TextLine::styled(
                    heading,
                    Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                ),
                None,
            ));
        }
        let (dot, colour) = match (&row.fingerprint, row.trust) {
            (None, _) => ("○", theme::dim()),
            (Some(_), omtalk_omemo::Trust::Verified) => ("✓", theme::ok()),
            (Some(_), omtalk_omemo::Trust::Blind) => ("●", theme::accent()),
            (Some(_), _) => ("✗", theme::warn()),
        };
        let mut state = crate::keys::trust_words(row.trust).to_owned();
        if row.this_device {
            state = tr!("this device", "это устройство").into();
        } else if let Some(at) = row.seen {
            state = format!(
                "{state} · {}",
                crate::keys::seen_words(at, chrono::Utc::now().timestamp())
            );
        }
        let selected = index == keys.cursor;
        let style = if selected {
            theme::selected()
        } else {
            Style::default()
        };
        lines.push((
            TextLine::from(vec![
                Span::styled(format!(" {dot} "), Style::default().fg(colour)),
                Span::styled(format!("{:<12}", row.device), style),
                Span::styled(format!(" {state}"), Style::default().fg(colour)),
            ]),
            Some(index),
        ));
        let fingerprint = row
            .fingerprint
            .clone()
            .unwrap_or_else(|| tr!("no keys", "нет ключей").into());
        lines.push((
            TextLine::styled(format!("   {fingerprint}"), Style::default().fg(theme::dim())),
            Some(index),
        ));
    }
    if keys.rows.is_empty() && !keys.loading {
        lines.push((
            TextLine::styled(
                tr!("no devices", "устройств нет"),
                Style::default().fg(theme::dim()),
            ),
            None,
        ));
    }
    let cursor_line = lines
        .iter()
        .rposition(|(_, r)| *r == Some(keys.cursor))
        .unwrap_or(0);
    let skip = cursor_line.saturating_sub(body.height as usize - 1);
    let mut row_areas = Vec::new();
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

    let (words, style) = match (&keys.problem, keys.loading, keys.confirm_remove) {
        (Some(problem), _, _) => (problem.clone(), Style::default().fg(theme::warn())),
        (None, true, _) => (
            tr!("loading…", "загрузка…").to_owned(),
            Style::default().fg(theme::dim()),
        ),
        (None, false, Some(device)) => (
            trf!(
                "remove {device}? Once more removes it",
                "удалить {device}? Ещё раз — удалить"
            ),
            Style::default().fg(theme::warn()),
        ),
        _ => (String::new(), Style::default()),
    };
    frame.render_widget(Paragraph::new(format!(" {words}")).style(style), status);

    let labels = keys.buttons();
    let total: u16 = labels
        .iter()
        .map(|b| crate::keys::button_words(*b).width() as u16 + 5)
        .sum();
    let mut x = buttons.x + buttons.width.saturating_sub(total + 1);
    let mut button_areas = Vec::new();
    for button in labels {
        let text = format!("[ {} ]", crate::keys::button_words(button));
        let w = text.width() as u16;
        let place = Rect {
            x,
            width: w.min(buttons.right().saturating_sub(x)),
            ..buttons
        };
        let style = if button == crate::keys::KeyButton::Remove {
            Style::default().fg(theme::warn())
        } else {
            Style::default().fg(theme::accent())
        };
        frame.render_widget(Paragraph::new(text).style(style), place);
        button_areas.push((place, button));
        x += w + 1;
    }
    frame.render_widget(
        Paragraph::new(tr!(
            " v verified · d distrust · b blind trust · x remove your own · Esc",
            " v проверено · d не доверять · b вслепую · x удалить своё · Esc"
        ))
        .style(Style::default().fg(theme::dim())),
        hint,
    );
    app.areas.key_rows = row_areas;
    app.areas.key_buttons = button_areas;
}

/// The settings window, over the middle of the screen: a row per setting, under the
/// table it is in.
fn settings_window(frame: &mut Frame, app: &mut App, screen: Rect) {
    let Some(settings) = app.settings.as_ref() else {
        return;
    };
    let width = screen.width.saturating_sub(4).min(64);
    let height = screen
        .height
        .saturating_sub(3)
        .min(settings.rows.len() as u16 + 8);
    let area = Rect {
        x: screen.x + (screen.width - width) / 2,
        y: screen.y + (screen.height.saturating_sub(1) - height) / 2,
        width,
        height,
    };
    app.areas.settings_window = area;
    clear(frame, area);
    let block = pane(tr!("Settings", "Настройки"), true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 20 {
        return;
    }
    let [body, status, hint] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    let body = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(2),
        ..body
    };
    let label_width = settings.rows.iter().map(|r| r.label.width()).max().unwrap_or(0) + 2;

    let mut lines: Vec<(TextLine, Option<usize>)> = Vec::new();
    let mut section = "";
    for (index, row) in settings.rows.iter().enumerate() {
        if row.section != section {
            section = row.section;
            if !lines.is_empty() {
                lines.push((TextLine::default(), None));
            }
            let heading = if section == "notify" {
                tr!("Notifications", "Уведомления")
            } else {
                tr!("Interface", "Интерфейс")
            };
            lines.push((
                TextLine::styled(
                    heading,
                    Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                ),
                None,
            ));
        }
        let selected = index == settings.cursor;
        let label = format!(" {:<w$}", row.label, w = label_width);
        let label_style = if selected {
            theme::selected()
        } else {
            Style::default()
        };
        let room = (body.width as usize).saturating_sub(label.width() + 4);
        let value = match (&settings.editing, selected) {
            (Some((text, at)), true) => {
                // The cursor as a reversed cell; the text scrolled to keep it in view.
                let chars: Vec<char> = text.chars().collect();
                let from = at.saturating_sub(room.saturating_sub(1));
                let before: String = chars[from..*at].iter().collect();
                let under: String = chars.get(*at).map_or(" ".into(), |c| c.to_string());
                let after: String = chars.iter().skip(at + 1).collect();
                vec![
                    Span::raw(before),
                    Span::styled(under, Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(truncate(&after, room.saturating_sub(at - from + 1))),
                ]
            }
            _ => {
                let words = truncate(&row.value.words(), room);
                let colour = match row.value {
                    crate::settings::Value::Bool(false) => theme::dim(),
                    _ => theme::accent(),
                };
                let words = match row.value {
                    crate::settings::Value::Choice(..) | crate::settings::Value::Number { .. }
                        if selected =>
                    {
                        format!("‹ {words} ›")
                    }
                    _ => words,
                };
                vec![Span::styled(words, Style::default().fg(colour))]
            }
        };
        let mut spans = vec![Span::styled(label, label_style)];
        spans.extend(value);
        lines.push((TextLine::from(spans), Some(index)));
    }
    let cursor_line = lines
        .iter()
        .position(|(_, r)| *r == Some(settings.cursor))
        .unwrap_or(0);
    let skip = cursor_line.saturating_sub(body.height as usize - 1);
    let mut row_areas = Vec::new();
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
    let (words, style) = match &settings.problem {
        Some(problem) => (problem.clone(), Style::default().fg(theme::warn())),
        None => (
            tr!(
                "takes effect at once and is saved in config.toml",
                "меняется сразу и сохраняется в config.toml"
            )
            .to_owned(),
            Style::default().fg(theme::dim()),
        ),
    };
    frame.render_widget(Paragraph::new(format!(" {words}")).style(style), status);
    let keys = if settings.editing.is_some() {
        tr!(" Enter saves · Esc cancels", " Enter сохранить · Esc отмена")
    } else {
        tr!(
            " ↑↓ pick · Enter or a click changes · ← → value · Esc",
            " ↑↓ выбрать · Enter или клик изменить · ← → значение · Esc"
        )
    };
    frame.render_widget(
        Paragraph::new(keys).style(Style::default().fg(theme::dim())),
        hint,
    );
    app.areas.settings_rows = row_areas;
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
    clear(frame, area);
    let place = match &services.node {
        Some(node) => format!("{} · {node}", services.jid),
        None => services.jid.to_string(),
    };
    let title = match &services.form {
        Some(form) => format!("{} · {}", form.title, form.jid),
        None => trf!("Services · {place}", "Сервисы · {place}"),
    };
    let block = pane(&title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    // What the server said and the focused field's description are wrapped, up to three
    // rows each: servers write whole sentences.
    let (status, problem_style) = match (&services.problem, services.loading) {
        (Some(text), _) => (text.clone(), Style::default().fg(theme::warn())),
        (None, true) => (
            tr!("loading…", "загрузка…").to_owned(),
            Style::default().fg(theme::dim()),
        ),
        (None, false) => (String::new(), Style::default()),
    };
    let text_width = inner.width.saturating_sub(2) as usize;
    let mut status_lines = wrap(&status, text_width);
    status_lines.truncate(3);
    let field_desc = services
        .form
        .as_ref()
        .filter(|f| !f.finished())
        .and_then(|f| f.fields.get(f.focus))
        .and_then(|f| f.desc.clone());
    let mut desc_lines = field_desc.map(|d| wrap(&d, text_width)).unwrap_or_default();
    desc_lines.truncate(3);
    let [body, problem, buttons, hint] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(status_lines.len().max(1) as u16),
        Constraint::Length(1),
        Constraint::Length(desc_lines.len().max(1) as u16),
    ])
    .areas(inner);
    let body = Rect {
        x: body.x + 1,
        width: body.width.saturating_sub(2),
        ..body
    };
    frame.render_widget(
        Paragraph::new(
            status_lines
                .iter()
                .map(|l| TextLine::raw(format!(" {l}")))
                .collect::<Vec<_>>(),
        )
        .style(problem_style),
        problem,
    );

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
                                Style::default().fg(theme::dim()),
                            ),
                        ]),
                        None,
                    ));
                }
            }
            let listing = services.listing.clone().unwrap_or_default();
            let mut row = 0;
            for (heading, entries, icon, commands) in [
                (
                    tr!("Services", "Сервисы"),
                    &listing.items,
                    app.icons.service,
                    false,
                ),
                (
                    tr!("Commands", "Команды"),
                    &listing.commands,
                    app.icons.command,
                    true,
                ),
            ] {
                if entries.is_empty() {
                    continue;
                }
                lines.push((TextLine::default(), None));
                lines.push((
                    TextLine::styled(
                        heading,
                        Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
                    ),
                    None,
                ));
                for entry in entries {
                    let name = entry
                        .name
                        .clone()
                        .unwrap_or_else(|| entry.node.clone().unwrap_or_else(|| entry.jid.to_string()));
                    let address = match (&entry.node, commands) {
                        (_, true) => String::new(),
                        (Some(node), _) => format!("  {} · {node}", entry.jid),
                        (None, _) if name != entry.jid.as_str() => format!("  {}", entry.jid),
                        (None, _) => String::new(),
                    };
                    // Too long for one row: the name wrapped, the address below it.
                    let selected = row == services.cursor;
                    let style = |line: TextLine<'static>| {
                        if selected {
                            line.style(theme::selected())
                        } else {
                            line
                        }
                    };
                    let width = body.width as usize;
                    if 3 + name.width() + address.width() <= width {
                        lines.push((
                            style(TextLine::from(vec![
                                Span::raw(format!(" {icon} ")),
                                Span::raw(name),
                                Span::styled(address, Style::default().fg(theme::dim())),
                            ])),
                            Some(row),
                        ));
                    } else {
                        for (i, part) in wrap(&name, width.saturating_sub(3)).into_iter().enumerate() {
                            let lead = if i == 0 { format!(" {icon} ") } else { "   ".into() };
                            lines.push((
                                style(TextLine::from(vec![Span::raw(lead), Span::raw(part)])),
                                Some(row),
                            ));
                        }
                        for part in wrap(address.trim(), width.saturating_sub(3)) {
                            lines.push((
                                style(TextLine::from(vec![
                                    Span::raw("   "),
                                    Span::styled(part, Style::default().fg(theme::dim())),
                                ])),
                                Some(row),
                            ));
                        }
                    }
                    row += 1;
                }
            }
            if services.listing.is_some() && row == 0 && services.problem.is_none() {
                lines.push((
                    TextLine::styled(
                        tr!("nothing here", "здесь ничего нет"),
                        Style::default().fg(theme::dim()),
                    ),
                    None,
                ));
            }
            let cursor_line = lines
                .iter()
                .rposition(|(_, r)| *r == Some(services.cursor))
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
                (tr!("Back", "Назад").into(), ServiceButton::Back, false),
                (tr!("Reload", "Обновить").into(), ServiceButton::Reload, false),
                (tr!("Close", "Закрыть").into(), ServiceButton::Close, false),
            ];
            hint_text = tr!(
                "Enter opens · ← back · r reload · Esc closes",
                "Enter открыть · ← назад · r обновить · Esc закрыть"
            )
            .into();
        }
        Some(form) => {
            let mut lines: Vec<(TextLine, Option<usize>)> = Vec::new();
            // Which line shows which option of a many-choice list, for clicks.
            let mut options: HashMap<usize, usize> = HashMap::new();
            let instructions = form.step.form.as_ref().and_then(|f| f.instructions.clone());
            for text in instructions.into_iter() {
                for part in wrap(&text, body.width as usize) {
                    lines.push((TextLine::styled(part, Style::default().fg(theme::dim())), None));
                }
            }
            for note in form.notes() {
                let style = match note.kind {
                    NoteKind::Info => Style::default(),
                    NoteKind::Warn | NoteKind::Error => Style::default().fg(theme::warn()),
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
                        lines.push((TextLine::styled(part, Style::default().fg(theme::dim())), None));
                    }
                    continue;
                }
                let mark = if field.required { "*" } else { "" };
                let label = format!("{}{mark}", field.label);
                let values = field_lines(field, focused, running);
                let label_style = if focused {
                    Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::dim())
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
                // Each value on its rows, wrapped to the column; the first beside the label.
                let value_width = (body.width as usize).saturating_sub(label_width + 1).max(8);
                let indent = " ".repeat(label_width + 1);
                let mut first = Some(label);
                for (value, option) in values {
                    for part in wrap(&value, value_width) {
                        let lead = first.take().unwrap_or_else(|| indent.clone());
                        if let Some(option) = option {
                            options.insert(lines.len(), option);
                        }
                        lines.push((
                            TextLine::from(vec![
                                Span::styled(lead, label_style),
                                Span::styled(part, value_style),
                            ]),
                            Some(index),
                        ));
                    }
                }
                if let Some(label) = first {
                    lines.push((TextLine::from(Span::styled(label, label_style)), Some(index)));
                }
            }
            if let Some(table) = &form.step.table {
                if !lines.is_empty() {
                    lines.push((TextLine::default(), None));
                }
                lines.extend(
                    table_lines(table, body.width as usize)
                        .into_iter()
                        .map(|l| (l, None)),
                );
            }
            // Filling in: the focused field kept in sight. A result: scrolled by the keys,
            // never past its end.
            let skip = if running {
                let focus_line = lines
                    .iter()
                    .rposition(|(_, r)| *r == Some(form.focus))
                    .unwrap_or(0);
                focus_line.saturating_sub(body.height as usize - 1)
            } else {
                form.scroll.min(lines.len().saturating_sub(body.height as usize))
            };
            for (at, (line, field)) in lines
                .into_iter()
                .enumerate()
                .skip(skip)
                .take(body.height as usize)
            {
                let place = Rect {
                    y: body.y + (at - skip) as u16,
                    height: 1,
                    ..body
                };
                if let Some(field) = field {
                    field_areas.push((place, (field, options.get(&at).copied())));
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
                    .chain([(tr!("Cancel", "Отмена").to_owned(), ServiceButton::Close, false)])
                    .collect();
                hint_text = tr!(
                    "Tab field · Space/←→ choose · Enter next · Esc cancel",
                    "Tab поле · Пробел/←→ выбрать · Enter дальше · Esc отменить"
                )
                .into();
            } else {
                let done = match form.step.status {
                    CommandStatus::Canceled => tr!("cancelled", "отменено"),
                    _ => tr!("done", "выполнено"),
                };
                labels = vec![
                    (
                        trf!("Back to the list ({done})", "К списку ({done})"),
                        ServiceButton::Dismiss,
                        true,
                    ),
                    (tr!("Close", "Закрыть").into(), ServiceButton::Close, false),
                ];
                hint_text = tr!(
                    "↑↓ scroll · Enter back to the list · Esc closes",
                    "↑↓ листать · Enter к списку · Esc закрыть"
                )
                .into();
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
            Style::default().fg(theme::accent())
        };
        frame.render_widget(Paragraph::new(text).style(style), place);
        button_areas.push((place, button));
        x += w + 1;
    }
    // The focused field's description, where it has one; else what the keys do.
    let hint_lines: Vec<TextLine> = if desc_lines.is_empty() {
        vec![TextLine::raw(format!(" {hint_text}"))]
    } else {
        desc_lines
            .iter()
            .map(|l| TextLine::raw(format!(" {l}")))
            .collect()
    };
    frame.render_widget(
        Paragraph::new(hint_lines).style(Style::default().fg(theme::dim())),
        hint,
    );
    app.areas.service_rows = row_areas;
    app.areas.service_fields = field_areas;
    app.areas.service_buttons = button_areas;
}

/// A result's table: a header, a rule, a row an item. Each column as wide as its widest
/// cell, the wide ones shrunk (cut with …) until the table fits `width`.
fn table_lines(table: &omtalk_xmpp::services::Table, width: usize) -> Vec<TextLine<'static>> {
    const GAP: usize = 2;
    let columns = table.columns.len();
    if columns == 0 {
        return Vec::new();
    }
    let mut widths: Vec<usize> = (0..columns)
        .map(|c| {
            table
                .rows
                .iter()
                .filter_map(|r| r.get(c))
                .map(|v| v.width())
                .chain([table.columns[c].width()])
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    // The widest gives way first, a cell at a time, down to a few cells each.
    let room = width.saturating_sub(GAP * (columns - 1));
    while widths.iter().sum::<usize>() > room {
        let (widest, w) = widths
            .iter()
            .enumerate()
            .max_by_key(|(_, w)| **w)
            .map(|(i, w)| (i, *w))
            .unwrap();
        if w <= 4 {
            break;
        }
        widths[widest] -= 1;
    }
    let row = |cells: Vec<String>, style: Style| {
        let mut spans = Vec::new();
        for (c, cell) in cells.into_iter().enumerate() {
            let w = widths[c];
            let text = truncate(&cell, w);
            let pad = w.saturating_sub(text.width()) + if c + 1 < columns { GAP } else { 0 };
            spans.push(Span::styled(format!("{text}{}", " ".repeat(pad)), style));
        }
        TextLine::from(spans)
    };
    let mut lines = vec![
        row(
            table.columns.clone(),
            Style::default().fg(theme::accent()).add_modifier(Modifier::BOLD),
        ),
        TextLine::styled(
            "─".repeat((widths.iter().sum::<usize>() + GAP * (columns - 1)).min(width)),
            Style::default().fg(theme::border_colour()),
        ),
    ];
    for cells in &table.rows {
        let cells = (0..columns)
            .map(|c| cells.get(c).cloned().unwrap_or_default())
            .collect();
        lines.push(row(cells, Style::default()));
    }
    if table.rows.is_empty() {
        lines.push(TextLine::styled(
            tr!("nothing", "пусто"),
            Style::default().fg(theme::dim()),
        ));
    }
    lines
}

/// A field's value as rows: a multi-line text by its lines, addresses and chosen options
/// one a row (as Gajim shows results); the field being typed in on one row, as typed.
fn field_lines(field: &FieldState, focused: bool, editing: bool) -> Vec<(String, Option<usize>)> {
    let caret = if focused { "▏" } else { "" };
    let lines: Vec<(String, Option<usize>)> = match field.kind {
        FieldType::TextMulti => {
            let mut lines: Vec<(String, Option<usize>)> =
                field.value.split('\n').map(|l| (l.to_owned(), None)).collect();
            if let Some((last, _)) = lines.last_mut() {
                last.push_str(caret);
            }
            lines
        }
        FieldType::JidMulti if !focused => field
            .value
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| (v.to_owned(), None))
            .collect(),
        FieldType::ListMulti => field
            .options
            .iter()
            .enumerate()
            .filter(|(_, (_, value))| editing || field.chosen.contains(value))
            .map(|(i, (label, value))| {
                if editing {
                    let here = if focused && i == field.option_cursor {
                        "›"
                    } else {
                        " "
                    };
                    let on = if field.chosen.contains(value) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    (format!("{here}{on} {label}"), Some(i))
                } else {
                    (label.clone(), None)
                }
            })
            .collect(),
        _ => vec![(field_value(field, focused, editing), None)],
    };
    if lines.is_empty() {
        vec![(String::new(), None)]
    } else {
        lines
    }
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
        FieldType::TextPrivate => format!("{}{caret}", "•".repeat(field.value.chars().count())),
        _ => format!("{}{caret}", field.value),
    }
}

fn step_label(action: CommandAction) -> &'static str {
    match action {
        CommandAction::Execute => tr!("Run", "Выполнить"),
        CommandAction::Prev => tr!("Back", "Назад"),
        CommandAction::Next => tr!("Next", "Далее"),
        CommandAction::Complete => tr!("Done", "Готово"),
        CommandAction::Cancel => tr!("Cancel", "Отмена"),
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
        let mark = format!("{}это без шифрования", theme::nerd().plain_mark);
        assert!(text.contains(&mark), "plain text is marked\n{text}");
        assert!(text.contains(theme::nerd().locked), "the lock on the input");
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
        assert!(row.contains(theme::nerd().locked));
        assert!(button.width >= theme::nerd().locked.width() as u16);
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
            table: None,
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
        let (switch, _) = *app
            .areas
            .service_fields
            .iter()
            .find(|(_, (i, _))| *i == 1)
            .unwrap();
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
        assert!(
            matches!(&actions[..], [Action::History { peer, .. }, Action::Devices { .. }] if peer.as_str() == "bob@example.org"),
            "a new chat asks the archive for its history: {actions:?}"
        );
        // Asked once: opened again, nothing more is read.
        app.select(room);
        let index = app
            .chats
            .iter()
            .position(|c| c.jid == jid("bob@example.org"))
            .unwrap();
        assert!(
            !app.select(index)
                .iter()
                .any(|a| matches!(a, Action::History { .. }))
        );

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
        app.connection
            .insert(home.clone(), crate::app::Connection::Online);
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

        // With nothing typed Tab still moves the focus (in a room, to its participants);
        // /help opens the list.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::People);
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
            "│   ├ @ anna",
            "│   └ @ bert",
            "└ @ carl",
            "▾ ● me@work.org",
            "└ ▾ \u{f07b} Свои",
            "    └ @ boss",
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
            assert_ne!(
                buffer[(x, row.y)].fg,
                theme::warn(),
                "cell {x} of the selected row"
            );
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
            untrusted: false,
            reply_to: None,
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
            untrusted: false,
            reply_to: None,
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
            untrusted: false,
            reply_to: None,
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
        app.connection.insert(me.clone(), crate::app::Connection::Online);
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
        // This side offline: what was heard may be stale, nobody is shown online.
        app.connection.insert(
            me.clone(),
            crate::app::Connection::Reconnecting("the server does not answer".into()),
        );
        assert!(screen(&mut app, 120, 24).contains("bob@home.org · не в сети"));
        app.connection.insert(me.clone(), crate::app::Connection::Online);

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
                .any(|l| l.text.contains("ваш Gajim (gajim.3) подключился"))
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
            app.input.clear();
            app.cursor = 0;
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
        // A terminal that types the dropped path in, key by key: the same offer.
        app.input.clear();
        app.cursor = 0;
        app.focus = Focus::Input;
        for c in file.display().to_string().chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.input, format!("/upload {}", file.display()));

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

    #[test]
    fn typing_is_told_once_per_change_and_shown_when_others_type() {
        use crossterm::event::{KeyCode, KeyEvent};
        use omtalk_xmpp::chatstates::ChatState;
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        let anna = app.chat(&me, &jid("anna@home.org"), ChatKind::Direct);
        app.select(bob);
        app.focus = Focus::Input;
        let key = |app: &mut App, c| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        let state = |state| Action::ChatState {
            account: me.clone(),
            to: jid("bob@home.org"),
            room: false,
            state,
        };

        assert_eq!(key(&mut app, 'п'), [state(ChatState::Composing)]);
        assert!(key(&mut app, 'р').is_empty(), "once, not per key");
        // Ten seconds without a key: paused; typing again: composing again.
        assert_eq!(app.typing_tick(app.typed_at + 10), [state(ChatState::Paused)]);
        assert!(app.typing_tick(app.typed_at + 20).is_empty());
        assert_eq!(key(&mut app, 'и'), [state(ChatState::Composing)]);
        // Sent: the message itself says "active".
        let sent = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&sent[..], [Action::Send { .. }]), "{sent:?}");
        // A command is not typing.
        assert!("/help".chars().all(|c| {
            !key(&mut app, c)
                .iter()
                .any(|a| matches!(a, Action::ChatState { .. }))
        }));
        app.input.clear();
        // Leaving a chat mid-word tells it the typing paused.
        key(&mut app, 'x');
        let left = app.select(anna);
        assert!(left.contains(&state(ChatState::Paused)), "{left:?}");
        app.input.clear();
        app.send_typing = false;
        app.select(bob);
        assert!(key(&mut app, 'y').is_empty(), "turned off");

        // Bob types: at the foot of his chat and beside it in the list; his message ends it.
        app.typing(
            &me,
            &jid("bob@home.org"),
            "bob".into(),
            ChatState::Composing,
            chrono::Utc::now().timestamp(),
        );
        let text = screen(&mut app, 100, 20);
        assert!(text.contains("bob печатает…") && text.contains("bob …"), "{text}");
        let from_bob = omtalk_omemo::Incoming {
            peer: jid("bob@home.org"),
            sender: jid("bob@home.org"),
            nick: None,
            device: Some(1),
            direction: omtalk_omemo::Direction::Incoming,
            encrypted: true,
            text: Some("вот".into()),
            at: None,
            not_for_this_device: false,
            id: None,
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
        };
        app.on_incoming(&me, &from_bob, 1);
        assert!(!screen(&mut app, 100, 20).contains("печатает"));
        // Silent for 30 s after saying so: no longer.
        app.typing(
            &me,
            &jid("bob@home.org"),
            "bob".into(),
            ChatState::Composing,
            1000,
        );
        assert!(app.typing_now(&me, &jid("bob@home.org"), 1031).is_empty());
        // In a room, one's own state told back is not someone typing.
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.typing(
            &me,
            &jid("team@muc.home.org"),
            "me".into(),
            ChatState::Composing,
            5,
        );
        app.typing(
            &me,
            &jid("team@muc.home.org"),
            "Алиса".into(),
            ChatState::Composing,
            5,
        );
        assert_eq!(app.typing_now(&me, &app.chats[room].jid.clone(), 6), ["Алиса"]);
    }

    #[test]
    fn a_window_behind_reads_nothing_and_rings_and_reads_on_return() {
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        let from_bob = |id: &str| omtalk_omemo::Incoming {
            peer: jid("bob@home.org"),
            sender: jid("bob@home.org"),
            nick: None,
            device: Some(1),
            direction: omtalk_omemo::Direction::Incoming,
            encrypted: true,
            text: Some("ты тут?".into()),
            at: None,
            not_for_this_device: false,
            id: Some(id.into()),
            markable: true,
            archive_id: Some(format!("a-{id}")),
            untrusted: false,
            reply_to: None,
        };
        assert_eq!(
            app.alert_for(&me, &from_bob("1")),
            Default::default(),
            "in front, chat open: nothing"
        );

        assert!(app.on_focus(false).is_empty());
        let alert = app.alert_for(&me, &from_bob("2"));
        assert!(
            alert.bell && alert.sound && alert.desktop,
            "private, window behind: {alert:?}"
        );
        assert!(
            app.on_incoming(&me, &from_bob("2"), 1).is_empty(),
            "behind: not marked read"
        );
        assert_eq!(app.chats[bob].unread, 1, "unread though the chat is open");

        let back = app.on_focus(true);
        assert_eq!(
            back,
            [Action::Displayed {
                account: me.clone(),
                to: jid("bob@home.org"),
                room: false,
                marker: Some("2".into()),
                archive_id: Some("a-2".into()),
            }]
        );
        assert_eq!(app.chats[bob].unread, 0);
        app.notify.bell_on = crate::notify::When::NEVER;
        app.on_focus(false);
        let alert = app.alert_for(&me, &from_bob("3"));
        assert!(!alert.bell && alert.sound, "the bell alone turned off: {alert:?}");

        // A room: its chatter rings only the bell; one's nick named, all of them.
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.notify.bell_on = crate::notify::When::ALL;
        let said = |text: &str| omtalk_omemo::Incoming {
            peer: jid("team@muc.home.org"),
            nick: Some("Алиса".into()),
            text: Some(text.into()),
            ..from_bob("4")
        };
        let chatter = app.alert_for(&me, &said("всем привет"));
        assert!(chatter.bell && !chatter.sound && !chatter.desktop, "{chatter:?}");
        // A message in the room, which makes the chat "a room" without a nick, keeps the
        // nick known: the mention after it is still one.
        app.on_incoming(&me, &said("всем привет"), 1);
        assert_eq!(app.chats[room].kind, ChatKind::Room { nick: "me".into() });
        assert!(
            app.alert_for(&me, &said("me")).sound,
            "a one-letter-short nick alone"
        );
        app.chats[room].name = Some("Команда".into());
        assert_eq!(
            app.notice_names(&me, &said("me, глянь")),
            ("Алиса".into(), Some("Команда".into())),
            "the room by its name, not its address"
        );
        let named = app.alert_for(&me, &said("me, глянь"));
        assert!(named.bell && named.sound && named.desktop, "{named:?}");
        // History read from the archive (a room's, on entering): nothing at all.
        let history = omtalk_omemo::Incoming {
            at: Some(1),
            ..said("me, глянь")
        };
        assert_eq!(app.alert_for(&me, &history), Default::default());
        let _ = room;
    }

    #[test]
    fn a_file_not_fetched_by_itself_is_fetched_with_a_click() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        let link = "https://up.home.org/x/отчёт.pdf";
        app.push(
            bob,
            Line::message(1_700_000_000, "bob".into(), link.into(), true, false),
        );
        app.media_done(link, crate::app::MediaState::Offered);
        let text = screen(&mut app, 100, 20);
        assert!(text.contains("отчёт.pdf скачать"), "{text}");
        let (row, _) = app.areas.fetches[0].clone();
        let clicked = app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 3,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(clicked, [Action::Fetch(link.into())]);
        app.media_done(link, crate::app::MediaState::Loading);
        assert!(screen(&mut app, 100, 20).contains("загружается…"));
        assert!(app.areas.fetches.is_empty(), "nothing to click while it comes");
    }

    #[test]
    fn the_keys_window_shows_devices_and_decides_about_them() {
        use crate::keys::{KeyButton, KeyRow, Request};
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        use omtalk_omemo::Trust;
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 34);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        let yesterday = chrono::Utc::now().timestamp() - 86_400;
        app.chats[bob].devices = Some(vec![crate::app::DeviceRow {
            device: 11,
            seen: Some(yesterday),
            fingerprint: Some("aaaa bbbb".into()),
            has_session: true,
            this_device: false,
            trust: Trust::Blind,
        }]);
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("писало вчера"),
            "when the device last wrote: {text}"
        );
        let devices = app.areas.side_devices.expect("the devices beside the chat");
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let opened = app.on_mouse(click(devices.x + 1, devices.y + 1));
        assert_eq!(
            opened,
            [Action::Keys(Request::Load {
                account: me.clone(),
                peer: Some(jid("bob@home.org"))
            })]
        );

        let row = |owner: &str, device, trust, this_device| KeyRow {
            owner: jid(owner),
            device,
            fingerprint: Some("05ded421 707f60dc".into()),
            trust,
            this_device,
            seen: (device == 12).then_some(yesterday),
        };
        app.keys.as_mut().unwrap().loaded(Ok(vec![
            row("bob@home.org", 11, Trust::Blind, false),
            row("bob@home.org", 12, Trust::Undecided, false),
            row("me@home.org", 21, Trust::Blind, true),
        ]));
        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("Ключи OMEMO · bob@home.org") && text.contains("ваши устройства"),
            "{text}"
        );
        assert!(text.contains("не решено · писало вчера"), "{text}");
        assert!(
            text.contains("не решено")
                && text.contains("это устройство")
                && text.contains("05ded421 707f60dc"),
            "{text}"
        );

        // A click on the second device, then on its button: verified.
        let (place, _) = *app.areas.key_rows.iter().find(|(_, i)| *i == 1).unwrap();
        app.on_mouse(click(place.x + 2, place.y));
        screen(&mut app, 120, 30);
        let (verify, _) = *app
            .areas
            .key_buttons
            .iter()
            .find(|(_, b)| *b == KeyButton::Verify)
            .unwrap();
        let asked = app.on_mouse(click(verify.x + 1, verify.y));
        assert_eq!(
            asked,
            [Action::Keys(Request::Trust {
                account: me.clone(),
                owner: jid("bob@home.org"),
                device: 12,
                trust: Trust::Verified
            })]
        );
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)), []);
        assert!(app.keys.is_none());

        // The leader and e open it from the keyboard.
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(matches!(
            &app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE))[..],
            [Action::Keys(_)]
        ));

        // A message from an undecided device is marked.
        app.keys = None;
        let mut line = Line::message(1_700_000_000, "bob".into(), "привет".into(), true, false);
        line.untrusted = true;
        app.push(bob, line);
        assert!(screen(&mut app, 120, 30).contains("⚠ непроверенное устройство · привет"));
    }

    #[test]
    fn a_rooms_members_who_are_away_show_below_and_open_a_chat() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 34);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        let opened = app.select(room);
        assert!(
            opened.contains(&Action::Members {
                account: me.clone(),
                room: jid("team@muc.home.org")
            }),
            "{opened:?}"
        );
        let chat = &mut app.chats[room];
        chat.occupants.insert("Алиса".into(), Some(jid("alice@home.org")));
        chat.occupants.insert("me".into(), Some(me.clone()));
        chat.members = vec![jid("alice@home.org"), jid("carl@work.org"), me.clone()];
        let text = screen(&mut app, 120, 24);
        assert!(
            text.contains("не в сети 1") && text.contains("○ carl work.org"),
            "{text}"
        );
        assert_eq!(
            app.away_members(room),
            [jid("carl@work.org")],
            "not the present, not oneself"
        );

        let (place, _) = app.areas.people[0].clone();
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: place.x + 2,
            row: place.y,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.current().unwrap().jid, jid("carl@work.org"));
    }

    #[test]
    fn a_reply_quotes_what_it_answers_and_shows_as_a_block() {
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        use omtalk_xmpp::replies::ReplyTo;
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        let mut from_bob = Line::message(1_700_000_000, "bob".into(), "как дела?".into(), true, false);
        from_bob.id = Some("b1".into());
        app.push(bob, from_bob);
        screen(&mut app, 100, 20);

        // A double click on his message: the reply begins, shown above the input.
        let (row, _) = *app
            .areas
            .message_lines
            .iter()
            .find(|(r, _)| {
                app.areas
                    .message_rows
                    .iter()
                    .any(|(m, t)| m == r && t.contains("как дела?"))
            })
            .unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 3,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        };
        let up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..click
        };
        app.on_mouse(click);
        app.on_mouse(up);
        app.on_mouse(click);
        let reply = app.replying.clone().expect("replying");
        assert_eq!((reply.who.as_str(), reply.quote.as_str()), ("bob", "как дела?"));
        assert_eq!(
            reply.to,
            Some(ReplyTo {
                id: "b1".into(),
                to: None
            })
        );
        assert!(screen(&mut app, 100, 20).contains("↩ bob: как дела?"));

        // Sent: the quote first, the reply named; shown as a block under whom it answers.
        "отлично"
            .chars()
            .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
        let sent = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let [
            Action::Send {
                text,
                reply: Some((to, length)),
                ..
            },
        ] = &sent[..]
        else {
            panic!("{sent:?}")
        };
        assert_eq!(text, "> bob:\n> как дела?\nотлично", "the quote says whose it is");
        assert_eq!(
            (to.id.as_str(), *length),
            ("b1", "> bob:\n> как дела?\n".chars().count())
        );
        assert!(app.replying.is_none());
        let text = screen(&mut app, 100, 20);
        assert!(
            text.contains("↩ bob") && text.contains("▎ как дела?") && text.contains("отлично"),
            "{text}"
        );
        assert!(
            !text.contains("> как дела?"),
            "the quote is drawn, not its marks: {text}"
        );
        assert!(
            !text.contains("▎ bob:"),
            "the name is in the header, not twice: {text}"
        );

        // Esc drops a reply before it is sent.
        app.reply_here();
        assert!(app.replying.is_some());
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.replying.is_none());

        // In a room the reply names the room's id for the message and who wrote it.
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        let mut said = Line::message(
            1_700_000_100,
            "Алиса".into(),
            "встреча в пять, не опаздывать".into(),
            true,
            false,
        );
        said.id = Some("a-own".into());
        said.archive_id = Some("r-9".into());
        app.push(room, said);
        app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let reply = app.replying.clone().unwrap();
        assert_eq!(
            reply.to,
            Some(ReplyTo {
                id: "r-9".into(),
                to: Some("team@muc.home.org/Алиса".parse().unwrap())
            })
        );

        // A selected piece is what is quoted.
        app.replying = None;
        screen(&mut app, 100, 20);
        let (row, text) = app
            .areas
            .message_rows
            .iter()
            .find(|(_, t)| t.contains("встреча"))
            .cloned()
            .unwrap();
        let start = row.x + text.find("встреча").map(|b| text[..b].chars().count()).unwrap() as u16;
        app.selection = Some((
            (start, row.y),
            (start + "встреча в пять".chars().count() as u16 - 1, row.y),
        ));
        app.reply_here();
        assert_eq!(app.replying.unwrap().quote, "встреча в пять");
    }

    #[test]
    fn a_rooms_name_and_topic_show_and_commands_set_them() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        // Told on entering: quiet. Changed by someone: said.
        app.topic(&me, &jid("team@muc.home.org"), None, "старая тема".into(), 1);
        assert!(app.chats[room].lines.is_empty());
        app.topic(
            &me,
            &jid("team@muc.home.org"),
            Some("Алиса".into()),
            "новая тема".into(),
            2,
        );
        assert!(
            app.chats[room].lines[0]
                .text
                .contains("Алиса сменил(а) тему: новая тема")
        );
        app.chats[room].name = Some("Команда".into());
        let text = screen(&mut app, 120, 20);
        assert!(text.contains("Команда") && text.contains("новая тема"), "{text}");

        let typed = |app: &mut App, line: &str| {
            app.focus = Focus::Input;
            line.chars()
                .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
            let mut actions = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            actions.retain(|a| !matches!(a, Action::ChatState { .. }));
            actions
        };
        let room_jid = jid("team@muc.home.org");
        assert_eq!(
            typed(&mut app, "/topic про релиз"),
            [Action::SetTopic {
                account: me.clone(),
                room: room_jid.clone(),
                topic: "про релиз".into()
            }]
        );
        assert_eq!(
            typed(&mut app, "/name Команда релиза"),
            [Action::SetRoomName {
                account: me.clone(),
                room: room_jid.clone(),
                name: "Команда релиза".into()
            }]
        );
        assert_eq!(
            typed(&mut app, "/invite bob@home.org"),
            [Action::Invite {
                account: me.clone(),
                room: room_jid,
                jid: jid("bob@home.org")
            }]
        );
        let made = typed(&mut app, "/create planning Планёрка");
        assert!(
            matches!(&made[..], [Action::CreateRoom { name: Some(n), .. }] if n == "Планёрка"),
            "{made:?}"
        );
    }

    #[test]
    fn nicks_change_in_place_and_the_room_pane_says_what_the_room_is() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 34);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        app.chats[room].name = Some("Команда".into());
        app.chats[room].topic = Some("релиз в пятницу".into());
        app.chats[room]
            .occupants
            .insert("гость".into(), Some(jid("xff@home.org")));
        app.chats[room].occupants.insert("me".into(), Some(me.clone()));

        app.rename_occupant(room, "гость", "mrak", 1);
        let chat = &app.chats[room];
        assert_eq!(
            chat.occupants.get("mrak"),
            Some(&Some(jid("xff@home.org"))),
            "the same person"
        );
        assert!(!chat.occupants.contains_key("гость"));
        assert!(chat.lines.last().unwrap().text.contains("гость теперь mrak"));
        app.rename_occupant(room, "me", "Стефан", 2);
        assert_eq!(
            app.chats[room].kind,
            ChatKind::Room {
                nick: "Стефан".into()
            }
        );

        let text = screen(&mut app, 120, 30);
        assert!(
            text.contains("релиз в пятницу") && text.contains("team@muc.home.org"),
            "topic and address: {text}"
        );
        assert!(
            text.contains("/topic тема") && text.contains("/nick ник"),
            "{text}"
        );

        app.focus = Focus::Input;
        "/nick Стеф"
            .chars()
            .for_each(|c| drop(app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
        let mut actions = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        actions.retain(|a| !matches!(a, Action::ChatState { .. }));
        assert_eq!(
            actions,
            [Action::Nick {
                account: me.clone(),
                room: jid("team@muc.home.org"),
                nick: "Стеф".into()
            }]
        );

        // /help: every command, the last ones too.
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        let text = screen(&mut app, 120, 40);
        assert!(text.contains("/create") && text.contains("/quit"), "{text}");
    }

    #[test]
    fn settings_change_at_once_and_are_written_down() {
        use crate::settings::Written;
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.focus = Focus::Input;
        "/settings"
            .chars()
            .for_each(|c| drop(app.on_key(key(KeyCode::Char(c)))));
        app.on_key(key(KeyCode::Enter));
        assert!(app.settings.is_some());
        let text = screen(&mut app, 120, 34);
        assert!(
            text.contains("Настройки") && text.contains("Уведомления"),
            "{text}"
        );
        assert!(text.contains("личные и упоминания"), "{text}");

        // The mouse, by a click on its row and a second one: off now, and written.
        let row = |app: &App, key: &str| {
            let settings = app.settings.as_ref().unwrap();
            let index = settings.rows.iter().position(|r| r.key == key).unwrap();
            app.areas
                .settings_rows
                .iter()
                .find(|(_, i)| *i == index)
                .unwrap()
                .0
        };
        let place = row(&app, "mouse");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: place.x + 2,
            row: place.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(app.on_mouse(click).is_empty(), "the first click selects");
        assert_eq!(
            app.on_mouse(click),
            [Action::Setting {
                section: "ui",
                key: "mouse",
                value: Written::Bool(false)
            }]
        );
        assert!(!app.mouse);

        // The modifier, by keys: Alt now holds the shortcuts.
        let modifier = app
            .settings
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|r| r.key == "modifier")
            .unwrap();
        app.settings.as_mut().unwrap().cursor = modifier;
        let actions = app.on_key(key(KeyCode::Right));
        assert_eq!(app.modifier, KeyModifiers::ALT, "{actions:?}");

        // Sound for everything: the alert for a room's chatter follows.
        let sound = app
            .settings
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|r| r.key == "sound_on")
            .unwrap();
        app.settings.as_mut().unwrap().cursor = sound;
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left));
        assert_eq!(app.notify.sound_on, crate::notify::When::ALL);

        // A prefix that is no key: said, not taken.
        let leader = app
            .settings
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|r| r.key == "leader")
            .unwrap();
        app.settings.as_mut().unwrap().cursor = leader;
        app.on_key(key(KeyCode::Enter));
        app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        "hyper+x"
            .chars()
            .for_each(|c| drop(app.on_key(key(KeyCode::Char(c)))));
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.settings.as_ref().unwrap().problem.is_some());
        assert_eq!(app.leader.0, KeyCode::Char('x'), "the old prefix stays");

        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert!(app.settings.is_none());
        // Opened again, it shows what was set.
        app.open_settings();
        assert_eq!(
            app.settings.as_ref().unwrap().get("ui", "mouse"),
            Some(&crate::settings::Value::Bool(false))
        );
    }

    #[test]
    fn avatars_are_off_unless_turned_on_then_asked_once_and_drawn_on_the_right() {
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        let asked = |actions: &[Action]| actions.iter().any(|a| matches!(a, Action::Avatar { .. }));
        assert!(!asked(&app.select(bob)), "off by default: nothing fetched");

        // Turned on in the settings: the open chat's avatar is asked for at once.
        app.open_settings();
        let row = app
            .settings
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .position(|r| r.key == "avatars")
            .unwrap();
        app.settings.as_mut().unwrap().cursor = row;
        let actions = app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            KeyModifiers::NONE,
        ));
        assert!(
            actions.contains(&Action::Avatar {
                account: me.clone(),
                jid: jid("bob@home.org"),
                room: false
            }),
            "{actions:?}"
        );
        app.settings = None;
        assert!(!asked(&app.select(bob)), "once");
        assert!(app.select(room).contains(&Action::Avatar {
            account: me.clone(),
            jid: jid("team@muc.home.org"),
            room: true
        }));

        // Ready: drawn on top of the right pane, the rest below it.
        app.select(bob);
        app.avatars.insert(jid("bob@home.org"), "abc".into());
        let mut pictures = Pictures::new();
        pictures.insert(
            "avatar:abc".into(),
            ratatui_image::picker::Picker::halfblocks()
                .new_resize_protocol(image::DynamicImage::new_rgb8(8, 8)),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|f| draw(f, &mut app, &mut pictures)).unwrap();
        let right = app.areas.right.unwrap();
        let text: Vec<String> = terminal
            .backend()
            .buffer()
            .content
            .chunks(120)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
        let first_words = text
            .iter()
            .position(|l| {
                let pane: String = l.chars().skip(right.x as usize).collect();
                pane.starts_with("│в сети")
            })
            .unwrap();
        assert!(
            first_words as u16 > right.y + 1 + AVATAR_WIDTH / 2,
            "below the picture:\n{}",
            text.join("\n")
        );

        // Without pictures in the terminal: drawn round in block characters, the corners
        // left to the background.
        app.avatar_blocks.insert(
            "avatar:abc".into(),
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([200, 0, 0, 255]),
            )),
        );
        terminal
            .draw(|f| draw(f, &mut app, &mut Pictures::new()))
            .unwrap();
        let inner_x = right.x + 1 + (right.width - 2 - AVATAR_WIDTH) / 2;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(inner_x, right.y + 2)].symbol(), " ", "a corner");
        assert!(
            (0..right.width)
                .all(|x| buffer[(right.x + x, right.y + 1)].fg != ratatui::style::Color::Rgb(200, 0, 0)),
            "a row of air above"
        );
        assert_eq!(
            buffer[(inner_x + AVATAR_WIDTH / 2, right.y + 2 + AVATAR_WIDTH / 4)].fg,
            ratatui::style::Color::Rgb(200, 0, 0),
            "the middle"
        );

        // A room's notice: the room's avatar and, when the room tells who wrote, theirs,
        // fetched once.
        app.avatars.insert(jid("team@muc.home.org"), "room".into());
        app.chats[room]
            .occupants
            .insert("Алиса".into(), Some(jid("alice@home.org")));
        let said = omtalk_omemo::Incoming {
            peer: jid("team@muc.home.org"),
            sender: jid("team@muc.home.org"),
            nick: Some("Алиса".into()),
            device: None,
            direction: omtalk_omemo::Direction::Incoming,
            encrypted: true,
            text: Some("привет".into()),
            at: None,
            not_for_this_device: false,
            id: None,
            markable: false,
            archive_id: None,
            untrusted: false,
            reply_to: None,
        };
        assert_eq!(app.notice_avatars(&me, &said), (None, Some("room".into())));
        assert_eq!(
            app.ask_writer_avatar(&me, &said),
            Some(Action::Avatar {
                account: me.clone(),
                jid: jid("alice@home.org"),
                room: false
            })
        );
        assert_eq!(app.ask_writer_avatar(&me, &said), None, "once");
        app.avatars.insert(jid("alice@home.org"), "alice".into());
        assert_eq!(
            app.notice_avatars(&me, &said),
            (Some("alice".into()), Some("room".into()))
        );

        // Turned off: not drawn.
        app.show_avatars = false;
        assert_eq!(app.notice_avatars(&me, &said), (None, None), "nor on notices");
        assert_eq!(app.avatar_of(&jid("bob@home.org")), None);
    }

    #[test]
    fn service_results_and_long_rows_wrap_instead_of_being_cut() {
        use crate::services::Listing;
        use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
        use omtalk_xmpp::services::{CommandStatus, CommandStep, ServiceItem};

        let mut app = app_with_chat();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let server: omtalk_xmpp::Jid = "example.org".parse().unwrap();
        let long = "Очень длинное название службы, которое никак не помещается в одну строку окна";
        app.services.as_mut().unwrap().listed(
            &server,
            &None,
            Ok(Listing {
                info: None,
                items: vec![ServiceItem {
                    jid: "pubsub.example.org".parse().unwrap(),
                    node: Some("news".into()),
                    name: Some(long.into()),
                }],
                commands: Vec::new(),
            }),
        );
        let text = screen(&mut app, 70, 30);
        assert!(
            text.contains("помещается") && text.contains("одну строку окна"),
            "all of it: {text}"
        );
        assert!(text.contains("pubsub.example.org · news"), "{text}");

        // A result: each address and each line of text on a row of its own.
        let result: omtalk_xmpp::Element = r#"<x xmlns="jabber:x:data" type="result">
            <field var="online" type="jid-multi" label="Online users">
              <value>dave@example.org</value><value>erin@example.org</value><value>frank@example.org</value></field>
            <field var="text" type="text-multi" label="Uptime"><value>up 3 days</value><value>load 0.1</value></field></x>"#
            .parse()
            .unwrap();
        let done = CommandStep {
            node: "online".into(),
            session: None,
            status: CommandStatus::Completed,
            form: Some(result.try_into().unwrap()),
            notes: Vec::new(),
            table: None,
            actions: Vec::new(),
            default_action: None,
        };
        app.services
            .as_mut()
            .unwrap()
            .stepped(server.clone(), "Online".into(), Ok(done));
        let text = screen(&mut app, 100, 30);
        let row_of = |what: &str| {
            text.lines()
                .position(|l| l.contains(what))
                .unwrap_or_else(|| panic!("{what}: {text}"))
        };
        assert!(
            row_of("dave@") < row_of("erin@") && row_of("erin@") < row_of("frank@"),
            "{text}"
        );
        assert!(row_of("up 3 days") < row_of("load 0.1"), "{text}");

        // A many-choice list: an option a row, ticked by a click on it.
        let form: omtalk_xmpp::Element = r#"<x xmlns="jabber:x:data" type="form">
            <field var="mods" type="list-multi" label="Modules"
              ><option label="pep"><value>pep</value></option><option label="mam"><value>mam</value></option></field></x>"#
            .parse()
            .unwrap();
        let step = CommandStep {
            node: "load".into(),
            session: Some("s".into()),
            status: CommandStatus::Executing,
            form: Some(form.try_into().unwrap()),
            notes: Vec::new(),
            table: None,
            actions: vec![CommandAction::Complete],
            default_action: Some(CommandAction::Complete),
        };
        app.services
            .as_mut()
            .unwrap()
            .stepped(server.clone(), "Load".into(), Ok(step));
        screen(&mut app, 100, 30);
        let (row, _) = *app
            .areas
            .service_fields
            .iter()
            .find(|(_, (_, option))| *option == Some(1))
            .expect("the second option has its row");
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: row.x + 2,
            row: row.y,
            modifiers: KeyModifiers::NONE,
        });
        let text = screen(&mut app, 100, 30);
        assert!(text.contains("[ ] pep") && text.contains("[x] mam"), "{text}");
    }

    #[test]
    fn room_participants_are_picked_with_the_keys() {
        use crossterm::event::{KeyCode, KeyEvent};
        let me = jid("me@example.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let room = app.chat(
            &me,
            &jid("room@muc.example.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(room);
        let chat = &mut app.chats[room];
        chat.occupants.insert("bob".into(), Some(jid("bob@example.org")));
        chat.occupants.insert("me".into(), Some(me.clone()));
        chat.members = vec![jid("carol@example.org")];
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.focus = Focus::Input;
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::People, "after the input line, the participants");
        assert_eq!(
            app.room_people(),
            [
                crate::app::Person::Here("bob".into()),
                crate::app::Person::Here("me".into()),
                crate::app::Person::Away(jid("carol@example.org"))
            ]
        );
        let text = screen(&mut app, 120, 24);
        assert!(text.contains("bob"), "{text}");

        // Down twice: carol, who is away; Enter opens a chat with her.
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Down));
        assert_eq!(app.people_cursor, 2, "not past the last");
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.current().unwrap().jid, jid("carol@example.org"));
        assert_eq!(app.focus, Focus::Input);

        // In a direct chat there is nobody to pick: Tab goes round without them.
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Chats);
    }

    #[test]
    fn messages_are_styled_as_conversations_styles_them() {
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.push(
            bob,
            Line::message(
                1_700_000_000,
                "bob".into(),
                "*жирный* и ~старое~ `код`\nтекст\n> цитата\n```\nlet *ы* = 1;\n```".into(),
                true,
                false,
            ),
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| draw(f, &mut app, &mut Pictures::new()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let find = |what: &str| {
            let text: Vec<String> = buffer
                .content
                .chunks(120)
                .map(|row| row.iter().map(|c| c.symbol()).collect())
                .collect();
            let (y, row) = text
                .iter()
                .enumerate()
                .find(|(_, r)| r.contains(what))
                .unwrap_or_else(|| panic!("{what}"));
            let x = row[..row.find(what).unwrap()].chars().count();
            &buffer[(x as u16, y as u16)]
        };
        assert!(
            find("жирный").modifier.contains(ratatui::style::Modifier::BOLD),
            "strong"
        );
        assert!(
            find("старое")
                .modifier
                .contains(ratatui::style::Modifier::CROSSED_OUT),
            "strike"
        );
        assert_eq!(find("код").fg, theme::accent(), "code");
        assert_eq!(find("*жирный").fg, theme::dim(), "the directive stays, dimmed");
        assert!(
            !find("текст").modifier.contains(ratatui::style::Modifier::BOLD),
            "plain"
        );
        assert_eq!(find("▎ цитата").fg, theme::accent(), "a quote has its bar");
        assert!(
            !find("ы*").modifier.contains(ratatui::style::Modifier::BOLD),
            "nothing styled in a block of code"
        );
        assert_eq!(find("let").fg, theme::accent());
    }

    #[test]
    fn in_english_no_screen_says_a_word_of_russian() {
        use crate::services::Listing;
        use crossterm::event::{KeyCode, KeyEvent};
        use omtalk_xmpp::services::{CommandStatus, CommandStep, ServiceItem};
        crate::lang::for_this_thread(crate::lang::Language::English);
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        app.icons = crate::theme::nerd();
        let server = app.server_chat(&me);
        app.server_log(&me, "x", 1);
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        let room = app.chat(
            &me,
            &jid("team@muc.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.chats[room]
            .occupants
            .insert("ann".into(), Some(jid("ann@home.org")));
        app.chats[room].members = vec![jid("carl@home.org")];
        app.chats[bob].devices = Some(vec![crate::app::DeviceRow {
            device: 1,
            seen: Some(1),
            fingerprint: Some("aaaa".into()),
            has_session: true,
            this_device: false,
            trust: omtalk_omemo::Trust::Undecided,
        }]);
        app.push(
            bob,
            Line::message(1_700_000_000, "bob".into(), "hello".into(), false, false),
        );
        app.rename_occupant(room, "ann", "anna", 2);
        let cyrillic = |text: &str| {
            text.chars()
                .find(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
        };
        let check = |app: &mut App, what: &str| {
            // The language's own row says it in both, on purpose.
            let text = screen(app, 130, 40).replace("Язык", "").replace("Русский", "");
            assert_eq!(cyrillic(&text), None, "{what}:\n{text}");
        };
        for index in [server, bob, room] {
            app.select(index);
            check(&mut app, "a chat");
        }
        app.focus = Focus::Input;
        app.on_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        check(&mut app, "the commands");
        app.input.clear();
        app.leader_pending = true;
        check(&mut app, "the prefix keys");
        app.leader_pending = false;
        app.open_settings();
        check(&mut app, "the settings");
        app.settings = None;
        app.select(bob);
        app.open_keys(None);
        app.keys.as_mut().unwrap().loaded(Ok(Vec::new()));
        check(&mut app, "the keys");
        app.keys = None;
        app.open_services(None);
        let host: omtalk_xmpp::Jid = "home.org".parse().unwrap();
        app.services.as_mut().unwrap().listed(
            &host,
            &None,
            Ok(Listing {
                info: None,
                items: vec![ServiceItem {
                    jid: "muc.home.org".parse().unwrap(),
                    node: None,
                    name: None,
                }],
                commands: vec![ServiceItem {
                    jid: host.clone(),
                    node: Some("x".into()),
                    name: Some("X".into()),
                }],
            }),
        );
        check(&mut app, "the services");
        let form: omtalk_xmpp::Element =
            r#"<x xmlns="jabber:x:data" type="form"><field var="a" type="boolean" label="A"/></x>"#
                .parse()
                .unwrap();
        let step = CommandStep {
            node: "x".into(),
            session: Some("s".into()),
            status: CommandStatus::Executing,
            form: Some(form.try_into().unwrap()),
            notes: Vec::new(),
            table: None,
            actions: vec![CommandAction::Complete],
            default_action: Some(CommandAction::Complete),
        };
        app.services.as_mut().unwrap().stepped(host, "X".into(), Ok(step));
        check(&mut app, "a form");
    }

    #[test]
    fn a_result_with_items_is_drawn_as_a_table_that_scrolls() {
        use crossterm::event::{KeyCode, KeyEvent};
        use omtalk_xmpp::services::{CommandStatus, CommandStep, Table};
        let mut app = app_with_chat();
        app.on_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let server: omtalk_xmpp::Jid = "example.org".parse().unwrap();
        let mut rows: Vec<Vec<String>> = (0..40)
            .map(|i| vec![format!("user{i:02}@example.org"), "pc".into()])
            .collect();
        rows[0][1] = "a resource name far too long to fit beside the address in this window".into();
        let done = CommandStep {
            node: "list".into(),
            session: None,
            status: CommandStatus::Completed,
            form: None,
            notes: Vec::new(),
            table: Some(Table {
                columns: vec!["Address".into(), "Resources".into()],
                rows,
            }),
            actions: Vec::new(),
            default_action: None,
        };
        app.services
            .as_mut()
            .unwrap()
            .stepped(server, "Online".into(), Ok(done));
        let text = screen(&mut app, 90, 30);
        let header = text.lines().find(|l| l.contains("Address")).expect("a header");
        assert!(header.contains("Resources"), "{text}");
        let first = text
            .lines()
            .find(|l| l.contains("user00@"))
            .expect("the first row");
        assert!(
            first.contains("a resource") && first.contains('…'),
            "cut to fit: {text}"
        );
        assert!(!text.contains("user39@"), "not all in sight: {text}");

        // Scrolled to the end, and no further.
        for _ in 0..10 {
            app.on_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        }
        let text = screen(&mut app, 90, 30);
        assert!(text.contains("user39@") && !text.contains("user00@"), "{text}");
    }

    #[test]
    fn a_chats_commands_open_as_gajim_opens_them() {
        use crate::services::{Outcome, Request};
        use omtalk_xmpp::presence::{Online, State};
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let browsed = |actions: &[Action]| match actions {
            [
                Action::Services {
                    request: Request::Browse { jid, .. },
                    ..
                },
            ] => jid.to_string(),
            other => panic!("{other:?}"),
        };

        // A biboumi channel: its own commands, and back to its IRC server's.
        let channel = app.chat(
            &me,
            &jid("#rust%irc.libera.chat@biboumi.home.org"),
            ChatKind::Room { nick: "me".into() },
        );
        app.select(channel);
        assert_eq!(
            browsed(&app.open_chat_commands()),
            "#rust%irc.libera.chat@biboumi.home.org"
        );
        let back = app.services.as_mut().unwrap().go_back();
        assert!(
            matches!(&back, Outcome::Request(Request::Browse { jid, .. }, _) if jid.as_str() == "irc.libera.chat@biboumi.home.org"),
            "{back:?}"
        );

        // A contact online: their client's commands, at its full address.
        let bob = app.chat(&me, &jid("bob@home.org"), ChatKind::Direct);
        app.select(bob);
        app.connection.insert(me.clone(), crate::app::Connection::Online);
        app.online
            .entry((me.clone(), jid("bob@home.org")))
            .or_default()
            .insert(
                "gajim.x1".into(),
                Online {
                    account: me.clone(),
                    resource: "gajim.x1".into(),
                    state: Some(State::Online),
                    client: "Gajim".into(),
                    status: None,
                },
            );
        assert_eq!(browsed(&app.open_chat_commands()), "bob@home.org/gajim.x1");

        // The server channel, by the prefix and c: one's own server, as /services.
        app.services = None;
        let server = app.server_chat(&me);
        app.select(server);
        assert_eq!(
            browsed(
                &app.on_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('x'),
                    KeyModifiers::CONTROL,
                ))
                .into_iter()
                .chain(app.on_key(crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('c'),
                    KeyModifiers::NONE
                )))
                .collect::<Vec<_>>()
            ),
            "home.org"
        );
    }

    #[test]
    fn me_is_sent_as_written_and_shown_as_an_action() {
        use crossterm::event::{KeyCode, KeyEvent};
        assert!(crate::app::is_action("/me waves"));
        assert!(!crate::app::is_action("/me"), "nothing to say");
        assert!(!crate::app::is_action("/meet"), "another word");
        assert_eq!(crate::app::action_of("/me машет рукой"), Some("машет рукой"));

        let mut app = app_with_chat();
        app.focus = Focus::Input;
        for c in "/me машет рукой".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let actions = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, Action::Send { text, .. } if text == "/me машет рукой")),
            "sent as written, for every client to show its way: {actions:?}"
        );
        let index = app.selected.unwrap();
        app.push(
            index,
            Line::message(1_700_000_100, "bob".into(), "/me смеётся".into(), true, false),
        );
        let text = screen(&mut app, 120, 24);
        assert!(text.contains("* bob смеётся"), "{text}");
        assert!(
            text.contains("* me машет рукой"),
            "one's own, by the account's name: {text}"
        );
        assert!(!text.contains("/me"), "{text}");
    }

    #[test]
    fn up_and_down_go_through_what_was_sent_as_in_a_shell() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = app_with_chat();
        app.focus = Focus::Input;
        let key = |app: &mut App, code| drop(app.on_key(KeyEvent::new(code, KeyModifiers::NONE)));
        let send = |app: &mut App, text: &str| {
            for c in text.chars() {
                key(app, KeyCode::Char(c));
            }
            key(app, KeyCode::Enter);
        };
        send(&mut app, "первое");
        send(&mut app, "второе");
        send(&mut app, "второе");
        "черно".chars().for_each(|c| key(&mut app, KeyCode::Char(c)));

        key(&mut app, KeyCode::Up);
        assert_eq!(app.input, "второе", "the latest, once though sent twice");
        key(&mut app, KeyCode::Up);
        assert_eq!(app.input, "первое");
        key(&mut app, KeyCode::Up);
        assert_eq!(app.input, "первое", "no further back");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.input, "второе");
        key(&mut app, KeyCode::Down);
        assert_eq!(app.input, "черно", "past the newest: the draft again");
        assert_eq!(app.cursor, 5);

        // Several lines: the arrows move between them first.
        app.input = "раз\nдва".into();
        app.cursor = 6;
        key(&mut app, KeyCode::Up);
        assert_eq!(
            (app.input.as_str(), app.cursor),
            ("раз\nдва", 2),
            "to the line above"
        );
        key(&mut app, KeyCode::Up);
        assert_eq!(app.input, "второе", "from the first line: the history");
    }

    #[test]
    fn a_long_chat_list_scrolls_to_keep_the_chat_picked_in_sight() {
        let me = jid("me@home.org");
        let mut app = App::new(vec![me.clone()], Modifier::Ctrl, 26, 30);
        let chats: Vec<usize> = (0..40)
            .map(|i| app.chat(&me, &jid(&format!("p{i:02}@home.org")), ChatKind::Direct))
            .collect();
        app.select(chats[0]);
        let text = screen(&mut app, 100, 20);
        assert!(text.contains("p00") && !text.contains("p39"), "{text}");
        app.select(chats[39]);
        let text = screen(&mut app, 100, 20);
        assert!(text.contains("p39"), "the chat picked is in sight: {text}");
        assert!(!text.contains("p00 "), "the top is scrolled away: {text}");
        let (place, _) = *app.areas.chat_rows.iter().find(|(_, i)| *i == chats[39]).unwrap();
        assert!(place.y < 19, "and can be clicked");
        // Back up: it follows.
        app.select(chats[3]);
        assert!(screen(&mut app, 100, 20).contains("p03"));
    }

    #[test]
    fn a_refused_command_leaves_the_line_to_put_right() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut app = app_with_chat();
        app.focus = Focus::Input;
        let typed = "/upload /no/such/path/kitten.png";
        for c in typed.chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!app.status.is_empty(), "said why");
        assert_eq!(app.input, typed, "the line is back, to be put right");
        assert_eq!(app.cursor, typed.chars().count());

        // One that works empties the line, as before.
        app.input.clear();
        app.cursor = 0;
        for c in "/settings".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.input.is_empty());
    }
}
