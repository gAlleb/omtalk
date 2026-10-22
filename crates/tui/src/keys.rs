//! The keys window: a peer's OMEMO devices and one's own, with fingerprints and what one
//! decided about each, as Gajim's. A model only; the view draws it, the runtime asks.

use crossterm::event::{KeyCode, KeyEvent};
use omtalk_omemo::Trust;
use omtalk_xmpp::BareJid;

/// One device as shown.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyRow {
    pub owner: BareJid,
    pub device: u32,
    /// `None`: its bundle is missing or broken, a dead device.
    pub fingerprint: Option<String>,
    pub trust: Trust,
    pub this_device: bool,
    /// When it last wrote (Unix seconds), if it ever did here.
    pub seen: Option<i64>,
}

/// What the window asks the runtime to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Load {
        account: BareJid,
        peer: Option<BareJid>,
    },
    Trust {
        account: BareJid,
        owner: BareJid,
        device: u32,
        trust: Trust,
    },
    Remove {
        account: BareJid,
        device: u32,
    },
}

/// A button of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyButton {
    Verify,
    Distrust,
    Blind,
    Remove,
    Close,
}

pub struct Keys {
    pub account: BareJid,
    /// Whose devices besides one's own; `None` for one's own only.
    pub peer: Option<BareJid>,
    /// The peer's first, then one's own.
    pub rows: Vec<KeyRow>,
    pub cursor: usize,
    pub loading: bool,
    pub problem: Option<String>,
    /// Remove was asked once for this device: a second time removes it.
    pub confirm_remove: Option<u32>,
}

/// What a key or a click did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    Ask(Request),
}

impl Keys {
    pub fn open(account: BareJid, peer: Option<BareJid>) -> (Self, Request) {
        let request = Request::Load {
            account: account.clone(),
            peer: peer.clone(),
        };
        let keys = Self {
            account,
            peer,
            rows: Vec::new(),
            cursor: 0,
            loading: true,
            problem: None,
            confirm_remove: None,
        };
        (keys, request)
    }

    pub fn loaded(&mut self, rows: Result<Vec<KeyRow>, String>) {
        self.loading = false;
        match rows {
            Ok(rows) => {
                self.rows = rows;
                self.problem = None;
            }
            Err(problem) => self.problem = Some(problem),
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&KeyRow> {
        self.rows.get(self.cursor)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = self.cursor.saturating_sub(1);
                self.confirm_remove = None;
                Outcome::Stay
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
                self.confirm_remove = None;
                Outcome::Stay
            }
            KeyCode::Char('v') => self.press(KeyButton::Verify),
            KeyCode::Char('d') => self.press(KeyButton::Distrust),
            KeyCode::Char('b') => self.press(KeyButton::Blind),
            KeyCode::Char('x') | KeyCode::Delete => self.press(KeyButton::Remove),
            _ => Outcome::Stay,
        }
    }

    pub fn click_row(&mut self, row: usize) -> Outcome {
        if row < self.rows.len() {
            self.cursor = row;
            self.confirm_remove = None;
        }
        Outcome::Stay
    }

    /// Which buttons make sense for the selected device.
    pub fn buttons(&self) -> Vec<KeyButton> {
        let mut buttons = Vec::new();
        if let Some(row) = self.selected().filter(|r| !r.this_device) {
            if row.trust != Trust::Verified {
                buttons.push(KeyButton::Verify);
            }
            if row.trust != Trust::Distrusted {
                buttons.push(KeyButton::Distrust);
            }
            if row.trust != Trust::Blind {
                buttons.push(KeyButton::Blind);
            }
            if row.owner == self.account {
                buttons.push(KeyButton::Remove);
            }
        }
        buttons.push(KeyButton::Close);
        buttons
    }

    pub fn press(&mut self, button: KeyButton) -> Outcome {
        if button == KeyButton::Close {
            return Outcome::Close;
        }
        if !self.buttons().contains(&button) {
            return Outcome::Stay;
        }
        let Some(row) = self.selected().cloned() else {
            return Outcome::Stay;
        };
        let trust = match button {
            KeyButton::Verify => Trust::Verified,
            KeyButton::Distrust => Trust::Distrusted,
            KeyButton::Blind => Trust::Blind,
            KeyButton::Remove => {
                // Removing is for good: asked twice.
                if self.confirm_remove != Some(row.device) {
                    self.confirm_remove = Some(row.device);
                    return Outcome::Stay;
                }
                self.confirm_remove = None;
                self.loading = true;
                return Outcome::Ask(Request::Remove {
                    account: self.account.clone(),
                    device: row.device,
                });
            }
            KeyButton::Close => unreachable!(),
        };
        self.loading = true;
        Outcome::Ask(Request::Trust {
            account: self.account.clone(),
            owner: row.owner,
            device: row.device,
            trust,
        })
    }
}

/// When a device last wrote, in words: "сегодня 14:05", "вчера 09:12", "12.10.2026".
pub fn seen_words(at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let (Some(at), Some(now)) = (
        Local.timestamp_opt(at, 0).single(),
        Local.timestamp_opt(now, 0).single(),
    ) else {
        return String::new();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => trf!("wrote today {}", "писало сегодня {}", at.format("%H:%M")),
        1 => trf!("wrote yesterday {}", "писало вчера {}", at.format("%H:%M")),
        _ => trf!("wrote {}", "писало {}", at.format("%d.%m.%Y")),
    }
}

pub fn trust_words(trust: Trust) -> &'static str {
    match trust {
        Trust::Blind => tr!("blind trust", "вслепую"),
        Trust::Verified => tr!("verified", "проверено"),
        Trust::Distrusted => tr!("distrusted", "не доверяю"),
        Trust::Undecided => tr!("undecided", "не решено"),
    }
}

pub fn button_words(button: KeyButton) -> &'static str {
    match button {
        KeyButton::Verify => tr!("Verified", "Проверено"),
        KeyButton::Distrust => tr!("Distrust", "Не доверять"),
        KeyButton::Blind => tr!("Blind trust", "Вслепую"),
        KeyButton::Remove => tr!("Remove", "Удалить"),
        KeyButton::Close => tr!("Close", "Закрыть"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    fn keys() -> Keys {
        let (mut keys, _) = Keys::open(jid("me@home.org"), Some(jid("bob@home.org")));
        let row = |owner: &str, device, trust, this_device| KeyRow {
            owner: jid(owner),
            device,
            fingerprint: Some("aaaa bbbb".into()),
            trust,
            this_device,
            seen: None,
        };
        keys.loaded(Ok(vec![
            row("bob@home.org", 1, Trust::Blind, false),
            row("me@home.org", 2, Trust::Blind, true),
            row("me@home.org", 3, Trust::Undecided, false),
        ]));
        keys
    }

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn when_a_device_last_wrote_is_said_by_the_day() {
        use chrono::{Local, TimeZone};
        let now = Local
            .with_ymd_and_hms(2026, 10, 23, 12, 0, 0)
            .unwrap()
            .timestamp();
        let at = |d, h, m| Local.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().timestamp();
        assert_eq!(seen_words(at(23, 9, 5), now), "писало сегодня 09:05");
        assert_eq!(seen_words(at(22, 23, 59), now), "писало вчера 23:59");
        assert_eq!(seen_words(at(1, 10, 0), now), "писало 01.10.2026");
    }

    #[test]
    fn decisions_go_to_the_selected_device_and_removal_is_asked_twice() {
        let mut keys = keys();
        assert_eq!(
            keys.on_key(key('v')),
            Outcome::Ask(Request::Trust {
                account: jid("me@home.org"),
                owner: jid("bob@home.org"),
                device: 1,
                trust: Trust::Verified
            })
        );
        assert!(
            !keys.buttons().contains(&KeyButton::Remove),
            "a peer's device is not ours to remove"
        );

        // This device: nothing to decide, nothing to remove.
        keys.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(keys.buttons(), [KeyButton::Close]);
        assert_eq!(keys.on_key(key('x')), Outcome::Stay);

        // An old one of ours: removed on the second press only.
        keys.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(keys.on_key(key('x')), Outcome::Stay);
        assert_eq!(keys.confirm_remove, Some(3));
        assert_eq!(
            keys.on_key(key('x')),
            Outcome::Ask(Request::Remove {
                account: jid("me@home.org"),
                device: 3
            })
        );
        assert_eq!(
            keys.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Outcome::Close
        );
    }
}
