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
//! modifier = "ctrl"     # ctrl, alt or super: the key for every shortcut
//! icons = "nerd"        # nerd (Nerd Font glyphs) or text
//! viewer = "xdg-open"   # opens a shared file on click
//! left = 26             # width of the chat list
//! right = 30            # width of the participants pane
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
    pub icons: IconSet,
    pub left: u16,
    pub right: u16,
    /// The program a click on a shared file opens it with.
    pub viewer: String,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            modifier: Modifier::Ctrl,
            icons: IconSet::Nerd,
            left: 26,
            right: 30,
            viewer: "xdg-open".into(),
        }
    }
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
}
