//! When each OMEMO device was last heard from: the time of its last message, as Gajim
//! shows it beside a device. Kept so that a device silent for months stands out.

use std::collections::HashMap;

use omtalk_xmpp::BareJid;
use rusqlite::params;

use crate::{Error, Store};

/// Schema 8; listed in `MIGRATIONS`.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE omemo_seen (
        account TEXT NOT NULL,
        jid     TEXT NOT NULL,
        device  INTEGER NOT NULL,
        at      INTEGER NOT NULL,
        PRIMARY KEY (account, jid, device)
    );
";

impl Store {
    /// Device `device` of `jid` wrote at `at` (Unix seconds); an older time (a message
    /// read late from an archive) changes nothing.
    pub fn device_seen(&self, account: &BareJid, jid: &BareJid, device: u32, at: i64) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "INSERT INTO omemo_seen (account, jid, device, at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (account, jid, device) DO UPDATE SET at = max(at, excluded.at)",
            params![account.as_str(), jid.as_str(), device, at],
        )?;
        Ok(())
    }

    /// When each device of `jid` last wrote, by device id.
    pub fn devices_seen(&self, account: &BareJid, jid: &BareJid) -> Result<HashMap<u32, i64>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT device, at FROM omemo_seen WHERE account = ?1 AND jid = ?2")?;
        let rows = query
            .query_map(params![account.as_str(), jid.as_str()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_message_of_a_device_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let (me, bob): (BareJid, BareJid) = (
            "me@example.org".parse().unwrap(),
            "bob@example.org".parse().unwrap(),
        );
        store.device_seen(&me, &bob, 7, 100).unwrap();
        store.device_seen(&me, &bob, 7, 50).unwrap();
        store.device_seen(&me, &bob, 8, 70).unwrap();
        let seen = store.devices_seen(&me, &bob).unwrap();
        assert_eq!(
            seen.get(&7),
            Some(&100),
            "a late archive message does not turn the clock back"
        );
        assert_eq!(seen.get(&8), Some(&70));
        assert!(store.devices_seen(&bob, &me).unwrap().is_empty(), "per account");
    }
}
