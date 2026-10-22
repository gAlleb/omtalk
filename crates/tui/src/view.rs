//! Draws the model, and remembers where everything went, for the mouse.

use chrono::{Local, TimeZone};
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, Areas, Chat, ChatKind, Focus, Line};
use crate::theme;

/// Narrower than this, the chat list folds away; the participants pane even earlier.
const FOLD_LEFT_BELOW: u16 = 50;
const FOLD_RIGHT_BELOW: u16 = 90;

pub fn draw(frame: &mut Frame, app: &mut App) {
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
    center_pane(frame, app, center);
    if show_right {
        side(frame, app, right);
    } else {
        strip(frame, right, "‹");
        app.areas.show_right = Some(right);
    }
    status_line(frame, app, status);
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
        if several && y < inner.bottom() {
            let header = Span::styled(account.to_string(), Style::default().fg(theme::DIM));
            frame.render_widget(Paragraph::new(header), Rect::new(inner.x, y, inner.width, 1));
            y += 1;
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

fn center_pane(frame: &mut Frame, app: &mut App, area: Rect) {
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
            let rows = message_rows(&app.chats[index].lines, width, app.icons);
            let height = inner.height as usize;
            let max_scroll = rows.len().saturating_sub(height);
            let chat = &mut app.chats[index];
            chat.scroll = chat.scroll.min(max_scroll);
            let end = rows.len() - chat.scroll;
            let start = end.saturating_sub(height);
            // Newest at the bottom, like every chat; a short history sits low too.
            let shown: Vec<_> = rows[start..end].to_vec();
            let top = inner.y + (height - shown.len()) as u16;
            frame.render_widget(
                Paragraph::new(shown),
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
pub fn message_rows(lines: &[Line], width: usize, icons: theme::Icons) -> Vec<TextLine<'static>> {
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
                rows.push(TextLine::from(Span::styled(
                    format!(
                        "{}{label}{}",
                        "─".repeat(side),
                        "─".repeat(width.saturating_sub(side + label.width()))
                    ),
                    Style::default().fg(theme::BORDER),
                )));
            }
            last_day = day;
            last_who = None;
        }
        let clock = time.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
        if line.notice {
            for text in wrap(&line.text, width.saturating_sub(4).max(1)) {
                rows.push(TextLine::from(Span::styled(
                    format!("  · {text}"),
                    Style::default().fg(theme::DIM).add_modifier(Modifier::ITALIC),
                )));
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
            rows.push(TextLine::from(vec![
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
        }
        last_who = Some((&line.who, line.outgoing, line.at));
        let mut first = true;
        let marker = icons.plain_mark;
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
            rows.push(TextLine::from(spans));
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
    match &chat.kind {
        ChatKind::Room { .. } => {
            for (nick, real) in &chat.occupants {
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
        left.push(Span::styled(app.status.clone(), Style::default().fg(theme::WARN)));
    }
    let hints_width = hints.width() as u16;
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(hints_width)]).areas(area);
    frame.render_widget(Paragraph::new(TextLine::from(left)), left_area);
    frame.render_widget(Paragraph::new(hints), right_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ChatKind;
    use crate::config::Modifier;
    use omtalk_xmpp::BareJid;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
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
        let line = |text: &str, at, encrypted| Line {
            at,
            who: "bob".into(),
            text: text.into(),
            encrypted,
            outgoing: false,
            notice: false,
        };
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
}
