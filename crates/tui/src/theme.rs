//! Colours and symbols, in one place.
//!
//! Only the terminal's own palette is used: the 16 ANSI colours and the default
//! foreground and background. Tools like pywal recolour exactly those, so omtalk follows
//! the wallpaper with the rest of the desktop. No fixed RGB values here.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Blue;
pub const TEXT: Color = Color::Reset;
pub const DIM: Color = Color::DarkGray;
pub const BORDER: Color = Color::DarkGray;
pub const OK: Color = Color::Green;
pub const WARN: Color = Color::Red;
pub const OUTGOING: Color = Color::Cyan;

/// A selected row: the terminal's own colours, swapped.
pub fn selected() -> Style {
    Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

/// Nick colours: one per name, the same on every start.
const NICKS: [Color; 10] = [
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
];

pub fn nick(name: &str) -> Style {
    let hash = name
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
    Style::default()
        .fg(NICKS[hash as usize % NICKS.len()])
        .add_modifier(Modifier::BOLD)
}

pub fn border(focused: bool) -> Style {
    Style::default().fg(if focused { ACCENT } else { BORDER })
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
}

/// U+F023 lock, U+F09C unlock, U+F233 server, U+F120 terminal, U+F07B folder.
pub const NERD: Icons = Icons {
    locked: "\u{f023} OMEMO",
    unlocked: "\u{f09c} без шифрования",
    plain_mark: "\u{f09c} без шифрования · ",
    chat_unlocked: " \u{f09c}",
    picture: "\u{f03e}",
    file: "\u{f15b}",
    service: "\u{f233}",
    command: "\u{f120}",
    folder: "\u{f07b}",
};

pub const TEXT_ICONS: Icons = Icons {
    locked: "[OMEMO]",
    unlocked: "[без шифрования]",
    plain_mark: "[без шифрования] · ",
    chat_unlocked: " [откр.]",
    picture: "[картинка]",
    file: "[файл]",
    service: "›",
    command: "$",
    folder: "/",
};
