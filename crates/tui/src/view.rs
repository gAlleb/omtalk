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

use crate::app::{App, Areas, Chat, ChatKind, Focus, Line, MediaState, ServiceButton};
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

    let several = app.accounts.len() > 1;
    let mut y = inner.y;
    for account in app.accounts.clone() {
        let folded = app.collapsed.contains(&account);
        if several && y < inner.bottom() {
            let row = Rect::new(inner.x, y, inner.width, 1);
            frame.render_widget(account_row(app, &account, folded, inner.width), row);
            app.areas.account_rows.push((row, account.clone()));
            y += 1;
        }
        if several && folded {
            continue;
        }
        for (index, chat) in app.chats.iter().enumerate().filter(|(_, c)| c.account == account) {
            if y >= inner.bottom() {
                break;
            }
            let row = Rect::new(inner.x, y, inner.width, 1);
            frame.render_widget(
                chat_row(chat, app.selected == Some(index), inner.width, app.icons),
                row,
            );
            app.areas.chat_rows.push((row, index));
            y += 1;
        }
    }
    if app.chats.is_empty() && inner.height > 2 {
        let hint = Paragraph::new(vec![
            TextLine::from(Span::styled("нет чатов", Style::default().fg(theme::DIM))),
            TextLine::from(Span::styled("/msg или /join", Style::default().fg(theme::DIM))),
        ]);
        frame.render_widget(
            hint,
            Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(1), 2),
        );
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

fn chat_row(chat: &Chat, selected: bool, width: u16, icons: theme::Icons) -> Paragraph<'static> {
    let icon = if chat.is_room() { "# " } else { "@ " };
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
        Span::styled(
            icon,
            base.fg(if chat.is_room() { theme::ACCENT } else { theme::DIM }),
        ),
        Span::styled(title, base),
        Span::styled(lock, base.fg(theme::WARN)),
        Span::styled(" ".repeat(gap), base),
        Span::styled(
            badge,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
    ]))
    .style(base)
}

fn center_pane(frame: &mut Frame, app: &mut App, area: Rect, pictures: &mut Pictures) {
    let [messages, input] = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(area);
    app.areas.messages = messages;
    app.areas.input = input;

    let (title, subtitle) = match app.current() {
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
    input_box(frame, app, input);
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
    }
    rows
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
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::border(app.focus == Focus::Input))
        .title(Span::styled(
            button.clone(),
            Style::default().fg(colour).add_modifier(Modifier::BOLD),
        ))
        .title(
            TextLine::from(Span::styled(" Enter ⏎ ", Style::default().fg(theme::DIM)))
                .alignment(Alignment::Right),
        );
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    app.areas.omemo_button = Rect::new(area.x + 1, area.y, button.width() as u16, 1);

    if app.input.is_empty() {
        let hint = if app.current().is_some() {
            "Сообщение…   /help — команды"
        } else {
            "/msg имя@сервер или /join комната"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(theme::DIM))),
            inner,
        );
    } else {
        // Keep the cursor in sight: show the tail of a long line.
        let before: String = app.input.chars().take(app.cursor).collect();
        let skip = before
            .width()
            .saturating_sub(inner.width.saturating_sub(1) as usize);
        let mut shown = String::new();
        let mut skipped = 0;
        for c in app.input.chars() {
            if skipped < skip {
                skipped += c.width().unwrap_or(0);
                continue;
            }
            shown.push(c);
        }
        frame.render_widget(Paragraph::new(shown), inner);
    }
    if app.focus == Focus::Input {
        let before: String = app.input.chars().take(app.cursor).collect();
        let x = (before.width() as u16).min(inner.width.saturating_sub(1));
        frame.set_cursor_position(Position::new(inner.x + x, inner.y));
    }
}

fn side(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = match app.current() {
        Some(chat) if chat.is_room() => format!("Участники {}", chat.occupants.len()),
        Some(_) => "Устройства".to_owned(),
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
        ChatKind::Direct => {
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
        let prefix = if app.modifier == KeyModifiers::ALT {
            "M-"
        } else if app.modifier == KeyModifiers::SUPER {
            "◆"
        } else {
            "^"
        };
        Span::styled(format!("{prefix}{k}"), Style::default().fg(theme::ACCENT))
    };
    let dim = |t: &str| Span::styled(t.to_owned(), Style::default().fg(theme::DIM));
    let hints = TextLine::from(vec![
        key("O"),
        dim(" OMEMO  "),
        key("B"),
        dim("/"),
        key("R"),
        dim(" панели  "),
        Span::styled("Tab", Style::default().fg(theme::ACCENT)),
        dim(" фокус  "),
        key("Q"),
        dim(" выход "),
    ]);
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
    if !app.status.is_empty() {
        left.push(Span::styled(
            format!("{}  ", app.status),
            Style::default().fg(theme::WARN),
        ));
    }
    let hints_width = hints.width() as u16;
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
        assert!(text.contains("Устройства"), "right pane for a direct chat");
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
            .account_rows
            .iter()
            .find(|(_, acc)| *acc == work)
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
}
