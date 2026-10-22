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
    /// Identity key first seen for each device. Trusted blindly until the user decides
    /// otherwise (BTBV, as Gajim and Conversations do); a device whose key changes
    /// later is refused.
    pub identities: HashMap<(BareJid, DeviceId), [u8; 32]>,
    /// The last message id read from each archive (the account's own, by its JID).
    pub archive_positions: HashMap<BareJid, String>,
}

impl State {
    pub fn new(keys: OwnKeys) -> Self {
        Self {
            keys,
            device_lists: HashMap::new(),
            sessions: HashMap::new(),
            identities: HashMap::new(),
            archive_positions: HashMap::new(),
        }
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
