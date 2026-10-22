//! Telling the user a message came: a sound (a command, as `canberra-gtk-play -i bell`),
//! a desktop notification (`notify-send`), and the terminal's bell (`\a`, which tmux and
//! window managers mark windows by). Each is set on its own: for private messages, for
//! one's nick named in a room, for both, for all, or never.

use serde::Deserialize;

/// Which messages a signal is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct When {
    pub private: bool,
    pub mention: bool,
    /// Every message, rooms too.
    pub all: bool,
}

impl When {
    pub const NEVER: When = When {
        private: false,
        mention: false,
        all: false,
    };
    pub const ALL: When = When {
        private: true,
        mention: true,
        all: true,
    };

    /// "all", "private", "mention", "private+mention" (either order), "none".
    pub fn parse(text: &str) -> Result<When, String> {
        let mut when = When::NEVER;
        for part in text.split('+').map(str::trim) {
            match part {
                "all" => when = When::ALL,
                "private" => when.private = true,
                "mention" => when.mention = true,
                "none" | "never" | "" => {}
                other => {
                    return Err(format!(
                        "[notify]: не понимаю {other:?}: all, private, mention, private+mention или none"
                    ));
                }
            }
        }
        Ok(when)
    }

    pub fn applies(self, private: bool, mention: bool) -> bool {
        self.all || (self.private && private) || (self.mention && mention)
    }
}

/// `[notify]` as written.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct NotifyConfig {
    /// The command that plays a sound; empty for none.
    pub sound: String,
    pub sound_on: String,
    /// notify-send.
    pub desktop_on: String,
    /// The terminal's bell.
    pub bell_on: String,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            sound: "canberra-gtk-play -i message-new-instant".into(),
            sound_on: "private+mention".into(),
            desktop_on: "private+mention".into(),
            bell_on: "all".into(),
        }
    }
}

/// What to do, ready.
#[derive(Debug, Clone, PartialEq)]
pub struct Notify {
    pub sound: Vec<String>,
    pub sound_on: When,
    pub desktop_on: When,
    pub bell_on: When,
}

impl Default for Notify {
    fn default() -> Self {
        NotifyConfig::default().ready().expect("the defaults are valid")
    }
}

impl NotifyConfig {
    pub fn ready(&self) -> Result<Notify, String> {
        Ok(Notify {
            sound: self.sound.split_whitespace().map(Into::into).collect(),
            sound_on: When::parse(&self.sound_on)?,
            desktop_on: When::parse(&self.desktop_on)?,
            bell_on: When::parse(&self.bell_on)?,
        })
    }
}

/// The signals one message calls for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Alert {
    pub sound: bool,
    pub desktop: bool,
    pub bell: bool,
}

impl Notify {
    pub fn alert(&self, private: bool, mention: bool) -> Alert {
        Alert {
            sound: !self.sound.is_empty() && self.sound_on.applies(private, mention),
            desktop: self.desktop_on.applies(private, mention),
            bell: self.bell_on.applies(private, mention),
        }
    }
}

/// Whether `text` names `nick`: as a word, any case, as people write "Алиса:" or "@алиса".
pub fn mentions(text: &str, nick: &str) -> bool {
    if nick.trim().is_empty() {
        return false;
    }
    let text = text.to_lowercase();
    let nick = nick.to_lowercase();
    let is_word = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_' && c != '-');
    let mut from = 0;
    while let Some(at) = text[from..].find(&nick) {
        let start = from + at;
        let end = start + nick.len();
        if is_word(text[..start].chars().next_back()) && is_word(text[end..].chars().next()) {
            return true;
        }
        from = start + nick.len().max(1);
        while !text.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// Runs the sound command or notify-send, apart, quietly; reaped when done.
pub fn run(command: &[String]) {
    let Some((program, args)) = command.split_first() else {
        return;
    };
    let child = tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
    }
}

/// As [`run`], from a thread that may wait: the command is waited for there.
pub fn run_blocking(command: &[String]) {
    let Some((program, args)) = command.split_first() else {
        return;
    };
    let _ = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// The notify-send command for a message from `who` (in `room`, if one), with `icon`
/// (an avatar) if there is one. One chat's notifications replace each other where the
/// daemon can (dunst's stack tag), not pile up.
pub fn desktop(
    chat: &str,
    who: &str,
    room: Option<&str>,
    text: &str,
    icon: Option<&std::path::Path>,
) -> Vec<String> {
    let title = match room {
        Some(room) => format!("{who} · {room}"),
        None => who.to_owned(),
    };
    let body: String = text.chars().take(200).collect();
    let mut command = vec![
        "notify-send".into(),
        "--app-name=omtalk".into(),
        format!("--hint=string:x-dunst-stack-tag:omtalk {chat}"),
    ];
    if let Some(icon) = icon {
        command.push(format!("--icon={}", icon.display()));
    }
    command.extend(["--".into(), title, body]);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_is_set_on_its_own() {
        let both = When {
            private: true,
            mention: true,
            all: false,
        };
        assert_eq!(When::parse("private+mention").unwrap(), both);
        assert_eq!(When::parse("mention + private").unwrap(), both);
        assert_eq!(When::parse("all").unwrap(), When::ALL);
        assert_eq!(When::parse("none").unwrap(), When::NEVER);
        assert!(When::parse("sometimes").unwrap_err().contains("sometimes"));

        let notify = NotifyConfig {
            sound: "canberra-gtk-play -i bell".into(),
            sound_on: "mention".into(),
            desktop_on: "private".into(),
            bell_on: "none".into(),
        }
        .ready()
        .unwrap();
        assert_eq!(notify.sound, ["canberra-gtk-play", "-i", "bell"]);
        assert_eq!(
            notify.alert(true, false),
            Alert {
                sound: false,
                desktop: true,
                bell: false
            }
        );
        assert_eq!(
            notify.alert(false, true),
            Alert {
                sound: true,
                desktop: false,
                bell: false
            }
        );
        assert_eq!(
            notify.alert(false, false),
            Alert::default(),
            "a room's chatter: nothing"
        );
        let quiet = NotifyConfig {
            sound: String::new(),
            ..NotifyConfig::default()
        }
        .ready()
        .unwrap();
        assert!(!quiet.alert(true, true).sound, "no command, no sound");
    }

    #[test]
    fn a_nick_is_named_as_a_word_in_any_case() {
        assert!(mentions("Алиса: ты тут?", "алиса"));
        assert!(mentions("привет, @Alice!", "alice"));
        assert!(mentions("alice", "Alice"));
        assert!(!mentions("Алисандра пришла", "Алиса"), "part of another word");
        assert!(!mentions("malice", "alice"));
        assert!(
            mentions("ну и alice-ы… alice", "alice"),
            "the second time it stands alone"
        );
        assert!(!mentions("что угодно", ""));
    }
}
