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

pub fn trust_words(trust: Trust) -> &'static str {
    match trust {
        Trust::Blind => "вслепую",
        Trust::Verified => "проверено",
        Trust::Distrusted => "не доверяю",
        Trust::Undecided => "не решено",
    }
}

pub fn button_words(button: KeyButton) -> &'static str {
    match button {
        KeyButton::Verify => "Проверено",
        KeyButton::Distrust => "Не доверять",
        KeyButton::Blind => "Вслепую",
        KeyButton::Remove => "Удалить",
        KeyButton::Close => "Закрыть",
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
