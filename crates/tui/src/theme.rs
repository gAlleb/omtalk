//! Colours and symbols, in one place.
//!
//! By default only the terminal's own palette is used: the 16 ANSI colours and the
//! default foreground and background. Tools like pywal recolour exactly those, so omtalk
//! follows the wallpaper with the rest of the desktop. `[theme]` in the config sets any
//! role to a colour of one's own (a name, `#rrggbb`, or 0–255); the rest stay the
//! terminal's.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

/// The colour of each role.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub accent: Color,
    pub text: Color,
    /// `Reset`: the terminal's own background.
    pub background: Color,
    pub dim: Color,
    pub border: Color,
    pub ok: Color,
    pub warn: Color,
    pub outgoing: Color,
    /// `None`: the selected row is the terminal's colours swapped.
    pub selection: Option<(Color, Color)>,
    /// One per name, the same on every start.
    pub nicks: Vec<Color>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            accent: Color::Blue,
            text: Color::Reset,
            background: Color::Reset,
            dim: Color::DarkGray,
            border: Color::DarkGray,
            ok: Color::Green,
            warn: Color::Red,
            outgoing: Color::Cyan,
            selection: None,
            nicks: vec![
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightMagenta,
            ],
        }
    }
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

/// Sets the colours, once, at start; later calls change nothing.
pub fn set(palette: Palette) {
    let _ = PALETTE.set(palette);
}

fn palette() -> &'static Palette {
    PALETTE.get_or_init(Palette::default)
}

pub fn accent() -> Color {
    palette().accent
}
pub fn text() -> Color {
    palette().text
}
pub fn background() -> Color {
    palette().background
}
pub fn dim() -> Color {
    palette().dim
}
pub fn border_colour() -> Color {
    palette().border
}
pub fn ok() -> Color {
    palette().ok
}
pub fn warn() -> Color {
    palette().warn
}
pub fn outgoing() -> Color {
    palette().outgoing
}

/// A colour as written in the config: a name ("blue", "bright-red", "gray"), "#rrggbb",
/// a number 0–255, or "default" for the terminal's own.
pub fn parse_colour(text: &str) -> Result<Color, String> {
    let lower = text.trim().to_lowercase();
    if lower == "default" || lower == "terminal" {
        return Ok(Color::Reset);
    }
    lower.parse().map_err(|_| {
        trf!(
            "cannot read colour {text:?}: a name (blue, bright-red), #rrggbb or 0–255",
            "не понимаю цвет {text:?}: имя (blue, bright-red), #rrggbb или 0–255"
        )
    })
}

/// A selected row: the terminal's own colours swapped, or the theme's selection colours.
pub fn selected() -> Style {
    match palette().selection {
        Some((fg, bg)) => Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        None => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
    }
}

pub fn nick(name: &str) -> Style {
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
    let nicks = &palette().nicks;
    Style::default()
        .fg(nicks[hash as usize % nicks.len().max(1)])
        .add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    Style::default().fg(if focused { accent() } else { border_colour() })
}

/// Symbols drawn in the text colour, so they follow the theme like the rest. Nerd Font
/// glyphs by default; plain text for terminals without them (`icons = "text"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icons {
    /// On the input box while OMEMO is on.
    pub locked: &'static str,
    /// On the input box while it is off.
    pub unlocked: &'static str,
    /// Before a message that came without encryption.
    pub plain_mark: &'static str,
    /// The same in a chat with OMEMO off, where every message is so: shorter.
    pub plain_mark_open_chat: &'static str,
    /// After a chat's name in the list when its OMEMO is off.
    pub chat_unlocked: &'static str,
    /// Before a shared picture's or file's name.
    pub picture: &'static str,
    pub file: &'static str,
    /// Before a service and a command in the services window.
    pub service: &'static str,
    pub command: &'static str,
    /// Before a folder's name in the chat list.
    pub folder: &'static str,
    /// The button that sends a file, on the input box.
    pub attach: &'static str,
    pub bell: &'static str,
    pub muted: &'static str,
    pub bot: &'static str,
}

/// U+F023 lock, U+F09C unlock, U+F233 server, U+F120 terminal, U+F07B folder, U+F0C6 paperclip.
/// Made when asked: some carry words, in the interface's language.
pub fn nerd() -> Icons {
    Icons {
        locked: "\u{f023} OMEMO",
        unlocked: tr!("\u{f09c} unencrypted", "\u{f09c} без шифрования"),
        plain_mark: tr!("\u{f09c} unencrypted · ", "\u{f09c} без шифрования · "),
        plain_mark_open_chat: "\u{f09c} ",
        chat_unlocked: " \u{f09c}",
        picture: "\u{f03e}",
        file: "\u{f15b}",
        service: "\u{f233}",
        command: "\u{f120}",
        folder: "\u{f07b}",
        attach: "\u{f0c6}",
        bell: "\u{f0f3}",
        muted: "\u{f1f6}",
        bot: "\u{f06a9}",
    }
}

pub fn text_icons() -> Icons {
    Icons {
        locked: "[OMEMO]",
        unlocked: tr!("[unencrypted]", "[без шифрования]"),
        plain_mark: tr!("[unencrypted] · ", "[без шифрования] · "),
        plain_mark_open_chat: tr!("[unencrypted] · ", "[без шифрования] · "),
        chat_unlocked: tr!(" [open]", " [откр.]"),
        picture: tr!("[picture]", "[картинка]"),
        file: tr!("[file]", "[файл]"),
        service: "›",
        command: "$",
        folder: "/",
        attach: tr!("[file]", "[файл]"),
        bell: tr!("[notify]", "[увед.]"),
        muted: tr!("[muted]", "[тихо]"),
        bot: tr!("[bot]", "[бот]"),
    }
}
