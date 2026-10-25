//! `~/.config/omtalk/config.toml`: the accounts, and how the interface behaves.
//!
//! ```toml
//! [[account]]
//! jid = "me@example.org"
//! password_command = "pass show xmpp/me"   # or asked for on start
//! # host = "xmpp.example.org"  port = 5222  # STARTTLS to a fixed host
//! # direct_tls = "xmpp.example.org:5223"
//!
//! [ui]
//! leader = "ctrl+x"     # press it, let go, then a letter (as tmux's prefix); a list shows
//! modifier = "ctrl"     # ctrl, alt, super or none: held with a letter, the same shortcuts
//! icons = "nerd"        # nerd (Nerd Font glyphs) or text
//! viewer = "xdg-open"   # opens a shared file on click
//! left = 26             # width of the chat list
//! right = 30            # width of the participants pane
//! auto_download = true  # false: shared files wait for a click ("скачать")
//! image_links = false   # true: preview standalone HTTPS image links
//! avatars = false       # true: people's and rooms' pictures in the right pane
//! compact = false       # true: time and name on the message's own line, not above it
//! language = "en"       # the interface's language: en or ru
//! mouse = true          # false: the mouse is the terminal's (selection as in tmux)
//! read_markers = true   # let senders see that their messages were read
//! typing = true         # let chats see that one is typing
//! bell = true           # older switch: false is [notify] bell_on = "none"
//!
//! [notify]              # each: all, private, mention, private+mention or none
//! sound = "canberra-gtk-play -i message-new-instant"   # a command; "" for no sound
//! sound_on = "private+mention"
//! desktop_on = "private+mention"   # notify-send
//! bell_on = "all"                  # the terminal's bell (\a), which tmux marks windows by
//!
//! [theme]               # optional: any role in a colour of one's own; the rest stay
//! accent = "#7aa2f7"    # the terminal's (pywal and the like keep working for them)
//! # text, background, dim, border, ok, warn, outgoing = names ("bright-red"),
//! # "#rrggbb" or 0–255; selection_text + selection_background; nicks = ["red", …]
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "account")]
    pub accounts: Vec<AccountEntry>,
    #[serde(default)]
    pub ui: Ui,
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub notify: crate::notify::NotifyConfig,
}

/// `[theme]`: colours of one's own for any role; the rest stay the terminal's.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub accent: Option<String>,
    pub text: Option<String>,
    pub background: Option<String>,
    pub dim: Option<String>,
    pub border: Option<String>,
    pub ok: Option<String>,
    pub warn: Option<String>,
    pub outgoing: Option<String>,
    /// The selected row; both or neither.
    pub selection_text: Option<String>,
    pub selection_background: Option<String>,
    pub nicks: Option<Vec<String>>,
}

impl ThemeConfig {
    pub fn palette(&self) -> Result<crate::theme::Palette, String> {
        use crate::theme::parse_colour;
        let mut palette = crate::theme::Palette::default();
        let roles = [
            (&self.accent, &mut palette.accent),
            (&self.text, &mut palette.text),
            (&self.background, &mut palette.background),
            (&self.dim, &mut palette.dim),
            (&self.border, &mut palette.border),
            (&self.ok, &mut palette.ok),
            (&self.warn, &mut palette.warn),
            (&self.outgoing, &mut palette.outgoing),
        ];
        for (written, colour) in roles {
            if let Some(written) = written {
                *colour = parse_colour(written).map_err(|e| format!("[theme]: {e}"))?;
            }
        }
        palette.selection = match (&self.selection_text, &self.selection_background) {
            (Some(fg), Some(bg)) => Some((parse_colour(fg)?, parse_colour(bg)?)),
            (None, None) => None,
            _ => {
                return Err(tr!(
                    "[theme]: selection_text and selection_background go together",
                    "[theme]: selection_text и selection_background задаются вместе"
                )
                .into());
            }
        };
        if let Some(nicks) = &self.nicks {
            if nicks.is_empty() {
                return Err(tr!(
                    "[theme]: nicks needs at least one colour",
                    "[theme]: nicks — хотя бы один цвет"
                )
                .into());
            }
            palette.nicks = nicks.iter().map(|n| parse_colour(n)).collect::<Result<_, _>>()?;
        }
        Ok(palette)
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct AccountEntry {
    pub jid: String,
    /// A shell command printing the password, so it need not sit in this file.
    pub password_command: Option<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub direct_tls: Option<String>,
    /// Unencrypted TCP, for the local test server only.
    #[cfg(feature = "insecure-tcp")]
    pub plaintext: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    #[default]
    Ctrl,
    Alt,
    Super,
    /// Shortcuts only through the leader key.
    None,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum IconSet {
    /// Nerd Font glyphs, drawn in the theme's colours.
    #[default]
    Nerd,
    /// Plain text, for terminals without a Nerd Font.
    Text,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct Ui {
    pub modifier: Modifier,
    /// The prefix key, as "ctrl+x", "alt+a" or "ctrl+space".
    pub leader: String,
    pub icons: IconSet,
    pub left: u16,
    pub right: u16,
    /// The program a click on a shared file opens it with.
    pub viewer: String,
    /// Tell senders when their messages were read here (XEP-0333), as others do.
    pub read_markers: bool,
    /// The mouse is omtalk's; `false` leaves it to the terminal (and tmux), which then
    /// selects and pastes as for any program. The leader key and m switch it.
    pub mouse: bool,
    /// Tell chats that one is typing (XEP-0085).
    pub typing: bool,
    /// Ring the terminal's bell for a message while the window is not in front.
    pub bell: bool,
    /// Fetch shared files and pictures as they come; `false`: on a click.
    pub auto_download: bool,
    /// Show standalone HTTPS image links as pictures, even without an upload marker.
    pub image_links: bool,
    /// Show people's and rooms' avatars (and fetch them).
    pub avatars: bool,
    /// Time and name on the first line of a message, as IRC clients have it, not on a
    /// line of their own above it.
    pub compact: bool,
    /// The interface's language: "en" (the default) or "ru".
    pub language: crate::lang::Language,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            modifier: Modifier::Ctrl,
            leader: "ctrl+x".into(),
            icons: IconSet::Nerd,
            left: 26,
            right: 30,
            viewer: "xdg-open".into(),
            read_markers: true,
            mouse: true,
            typing: true,
            bell: true,
            auto_download: true,
            image_links: false,
            avatars: false,
            compact: false,
            language: crate::lang::Language::English,
        }
    }
}

/// A key as written in the config: "ctrl+x", "alt+a", "ctrl+space", "f2".
pub fn parse_key(text: &str) -> Result<(crossterm::event::KeyCode, crossterm::event::KeyModifiers), String> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut modifiers = KeyModifiers::NONE;
    let lower = text.trim().to_lowercase();
    let mut parts: Vec<&str> = lower.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    for part in parts {
        modifiers |= match part {
            "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
            "alt" | "meta" | "m" => KeyModifiers::ALT,
            "super" => KeyModifiers::SUPER,
            "shift" => KeyModifiers::SHIFT,
            other => {
                return Err(trf!(
                    "leader = {text:?}: unknown modifier {other:?}",
                    "leader = {text:?}: неизвестный модификатор {other:?}"
                ));
            }
        };
    }
    let code = match key {
        "space" => KeyCode::Char(' '),
        "esc" | "escape" => KeyCode::Esc,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => KeyCode::F(f[1..].parse().unwrap()),
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next().unwrap()),
        _ => {
            return Err(trf!(
                "leader = {text:?}: cannot read key {key:?}",
                "leader = {text:?}: не понимаю клавишу {key:?}"
            ));
        }
    };
    Ok((code, modifiers))
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omtalk")
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omtalk")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("omtalk")
}

/// Reads the file; a missing file is an empty configuration, a broken one an error.
pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Runs `password_command` and takes its first line.
pub fn run_password_command(command: &str) -> Result<String, String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .map_err(|e| format!("password_command: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "password_command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "password_command printed no text".to_owned())?;
    Ok(text.lines().next().unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_defaults_fill_in() {
        let config: Config = toml::from_str(
            r#"
            [[account]]
            jid = "me@example.org"
            password_command = "echo secret"

            [[account]]
            jid = "work@example.com"
            host = "xmpp.example.com"

            [ui]
            modifier = "alt"
            "#,
        )
        .unwrap();
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[1].host.as_deref(), Some("xmpp.example.com"));
        assert_eq!(config.ui.modifier, Modifier::Alt);
        assert_eq!(
            (config.ui.left, config.ui.right),
            (26, 30),
            "unset widths keep their defaults"
        );
        assert_eq!(
            run_password_command(config.accounts[0].password_command.as_deref().unwrap()).unwrap(),
            "secret"
        );
    }

    #[test]
    fn a_typo_is_an_error_not_a_silent_default() {
        assert!(toml::from_str::<Config>("[ui]\nmodifer = \"alt\"").is_err());
    }

    #[test]
    fn keys_in_the_config_are_read() {
        use crossterm::event::{KeyCode, KeyModifiers};
        assert_eq!(
            parse_key("ctrl+x"),
            Ok((KeyCode::Char('x'), KeyModifiers::CONTROL))
        );
        assert_eq!(
            parse_key("Ctrl+Space"),
            Ok((KeyCode::Char(' '), KeyModifiers::CONTROL))
        );
        assert_eq!(parse_key("alt+a"), Ok((KeyCode::Char('a'), KeyModifiers::ALT)));
        assert_eq!(parse_key("f2"), Ok((KeyCode::F(2), KeyModifiers::NONE)));
        assert!(parse_key("hyper+x").is_err() && parse_key("ctrl+xx").is_err());
    }

    #[test]
    fn a_theme_sets_roles_of_ones_own_and_leaves_the_rest_to_the_terminal() {
        use ratatui::style::Color;
        let config: Config = toml::from_str(
            r##"
            [theme]
            accent = "#7aa2f7"
            warn = "bright-red"
            dim = "244"
            background = "default"
            selection_text = "black"
            selection_background = "yellow"
            nicks = ["cyan", "#ff8800"]
            "##,
        )
        .unwrap();
        let palette = config.theme.palette().unwrap();
        assert_eq!(palette.accent, Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(palette.warn, Color::LightRed);
        assert_eq!(palette.dim, Color::Indexed(244));
        assert_eq!(palette.background, Color::Reset);
        assert_eq!(palette.selection, Some((Color::Black, Color::Yellow)));
        assert_eq!(palette.nicks, [Color::Cyan, Color::Rgb(0xff, 0x88, 0x00)]);
        assert_eq!(palette.ok, Color::Green, "unset: the terminal's green");

        let empty: Config = toml::from_str("").unwrap();
        assert_eq!(empty.theme.palette().unwrap(), crate::theme::Palette::default());
        let wrong: Config = toml::from_str("[theme]\naccent = \"blurple\"").unwrap();
        assert!(wrong.theme.palette().unwrap_err().contains("blurple"));
        let half: Config = toml::from_str("[theme]\nselection_text = \"black\"").unwrap();
        assert!(half.theme.palette().is_err(), "a selection is two colours");
        assert!(
            toml::from_str::<Config>("[theme]\naccnt = \"red\"").is_err(),
            "a misspelt role is said"
        );
        assert!(
            empty.ui.auto_download,
            "files come by themselves unless asked not to"
        );
        let manual: Config = toml::from_str("[ui]\nauto_download = false").unwrap();
        assert!(!manual.ui.auto_download);
    }

    #[test]
    fn the_example_in_the_readme_is_a_valid_config() {
        let readme = include_str!("../../../README.md");
        let start = readme
            .find("```toml\n# ~/.config/omtalk/config.toml")
            .expect("the example is there");
        let body = &readme[start + "```toml\n".len()..];
        let example = &body[..body.find("```").unwrap()];
        let config: Config = toml::from_str(example).expect("the README's example parses");
        assert_eq!(config.accounts.len(), 1);
        assert!(config.ui.auto_download && config.ui.bell);
        config.theme.palette().expect("its colours are colours");
    }
}
