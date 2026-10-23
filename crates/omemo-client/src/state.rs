use std::collections::{BTreeSet, HashMap};

use omemo::{DeviceId, OwnKeys, Session};
use omtalk_xmpp::BareJid;

/// Everything OMEMO knows for one account on this device. Persist it after every change
/// that matters: a lost session means undecryptable messages until it is rebuilt.
pub struct State {
    pub keys: OwnKeys,
    /// Latest known device list per account, the own one included. A cache, not saved:
    /// after a restart it is read from the server again.
    pub device_lists: HashMap<BareJid, BTreeSet<DeviceId>>,
    pub sessions: HashMap<(BareJid, DeviceId), Session>,
    /// Sessions with a device that a newer one replaced, newest first: the peer may
    /// still write in one (both sides started a session at once, a message was late).
    pub previous_sessions: HashMap<(BareJid, DeviceId), Vec<Session>>,
    /// Identity key first seen for each device. Trusted blindly until the user decides
    /// otherwise (BTBV, as Gajim and Conversations do); a device whose key changes
    /// later is refused.
    pub identities: HashMap<(BareJid, DeviceId), [u8; 32]>,
    /// The last message id read from each archive (the account's own, by its JID).
    pub archive_positions: HashMap<BareJid, String>,
    /// What the user decided about each device; one not here is trusted blindly.
    pub trust: HashMap<(BareJid, DeviceId), Trust>,
}

/// Whether messages go to a device, as Gajim keeps it (BTBV, "blind trust before
/// verification"): a new device is trusted blindly until the user has verified any
/// device of that account; from then on a new one waits for a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Trusted without its fingerprint compared.
    Blind,
    /// Its fingerprint compared with the owner's, and found right.
    Verified,
    /// Refused: nothing is encrypted for it, what it sends is marked.
    Distrusted,
    /// New, after the user began verifying this account's devices: refused until decided.
    Undecided,
}

impl Trust {
    /// Whether messages are encrypted for it.
    pub fn receives(self) -> bool {
        matches!(self, Trust::Blind | Trust::Verified)
    }

    pub fn code(self) -> u8 {
        match self {
            Trust::Blind => 0,
            Trust::Verified => 1,
            Trust::Distrusted => 2,
            Trust::Undecided => 3,
        }
    }

    pub fn from_code(code: u8) -> Option<Trust> {
        Some(match code {
            0 => Trust::Blind,
            1 => Trust::Verified,
            2 => Trust::Distrusted,
            3 => Trust::Undecided,
            _ => return None,
        })
    }
}

/// How many replaced sessions are kept per device.
const PREVIOUS_SESSIONS: usize = 5;

impl State {
    /// The sessions with a device to try a message in: the current one, then those it
    /// replaced.
    pub(crate) fn sessions_with(&self, key: &(BareJid, DeviceId)) -> Vec<&Session> {
        self.sessions
            .get(key)
            .into_iter()
            .chain(self.previous_sessions.get(key).into_iter().flatten())
            .collect()
    }

    /// Makes `session` the current one with a device. `continued` is which of
    /// [`State::sessions_with`] it went on from (`None`: a new one); the current one it
    /// replaces is kept among the previous. Whether those changed.
    pub(crate) fn set_session(
        &mut self,
        key: &(BareJid, DeviceId),
        session: Session,
        continued: Option<usize>,
    ) -> bool {
        let replaced = match continued {
            Some(0) => None,
            Some(index) => {
                // The previous one it went on from comes back as the current one.
                if let Some(previous) = self.previous_sessions.get_mut(key) {
                    previous.remove(index - 1);
                }
                self.sessions.remove(key)
            }
            None => self.sessions.remove(key),
        };
        self.sessions.insert(key.clone(), session);
        match replaced {
            Some(old) => {
                let previous = self.previous_sessions.entry(key.clone()).or_default();
                previous.insert(0, old);
                previous.truncate(PREVIOUS_SESSIONS);
                true
            }
            None => matches!(continued, Some(index) if index > 0),
        }
    }

    pub fn new(keys: OwnKeys) -> Self {
        Self {
            keys,
            device_lists: HashMap::new(),
            sessions: HashMap::new(),
            previous_sessions: HashMap::new(),
            identities: HashMap::new(),
            archive_positions: HashMap::new(),
            trust: HashMap::new(),
        }
    }

    pub fn trust_of(&self, jid: &BareJid, device: DeviceId) -> Trust {
        self.trust
            .get(&(jid.clone(), device))
            .copied()
            .unwrap_or(Trust::Blind)
    }

    /// The trust a device seen for the first time starts with.
    pub(crate) fn first_trust(&self, jid: &BareJid) -> Trust {
        let verifying = self
            .trust
            .iter()
            .any(|((j, _), t)| j == jid && *t == Trust::Verified);
        if verifying { Trust::Undecided } else { Trust::Blind }
    }

    /// Records the identity key of a device, or checks it against the recorded one.
    pub(crate) fn check_identity(&mut self, jid: &BareJid, device: DeviceId, key: &[u8; 32]) -> Identity {
        match self.identities.get(&(jid.clone(), device)) {
            Some(known) if known == key => Identity::Known,
            Some(_) => Identity::Changed,
            None => {
                self.identities.insert((jid.clone(), device), *key);
                Identity::New
            }
        }
    }
}

pub(crate) enum Identity {
    Known,
    New,
    Changed,
}
