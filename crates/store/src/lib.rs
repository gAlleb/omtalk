//! omtalk's local data in one SQLite file: for now the OMEMO state of every account.
//!
//! Secrets are stored unencrypted, as Gajim and Conversations store theirs; the file is
//! created readable by its owner only.

mod history;

pub use history::{ChatSummary, StoredMessage};

use std::path::Path;
use std::sync::{Arc, Mutex};

use omemo::{DeviceId, OwnKeys, Session};
use omtalk_omemo::{Persist, PersistError, State};
use omtalk_xmpp::BareJid;
use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored value does not decode: {0}")]
    Decode(#[from] postcard::Error),
    #[error("stored JID is invalid: {0}")]
    Jid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("the database was written by a newer omtalk (schema {0})")]
    NewerSchema(i64),
}

/// Each entry upgrades the schema by one version; never edit one that has shipped.
const MIGRATIONS: &[&str] = &[
    "
    CREATE TABLE omemo_keys (
        account TEXT PRIMARY KEY,
        keys    BLOB NOT NULL
    );
    CREATE TABLE omemo_sessions (
        account TEXT NOT NULL,
        jid     TEXT NOT NULL,
        device  INTEGER NOT NULL,
        session BLOB NOT NULL,
        PRIMARY KEY (account, jid, device)
    );
    CREATE TABLE omemo_identities (
        account TEXT NOT NULL,
        jid     TEXT NOT NULL,
        device  INTEGER NOT NULL,
        key     BLOB NOT NULL,
        PRIMARY KEY (account, jid, device)
    );
",
    "
    CREATE TABLE archive_positions (
        account TEXT NOT NULL,
        archive TEXT NOT NULL,
        last_id TEXT NOT NULL,
        PRIMARY KEY (account, archive)
    );
",
    history::MIGRATION,
];

#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, Error> {
        create_private(path)?;
        let mut db = Connection::open(path)?;
        // WAL survives a crash mid-write and lets reads run beside the writer.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut db)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// The saved OMEMO state of `account`, or `None` if this device never had one.
    pub fn load_omemo(&self, account: &BareJid) -> Result<Option<State>, Error> {
        let db = self.db.lock().unwrap();
        let keys: Option<Vec<u8>> = db
            .query_row(
                "SELECT keys FROM omemo_keys WHERE account = ?1",
                [account.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(keys) = keys else { return Ok(None) };
        let mut state = State::new(postcard::from_bytes::<OwnKeys>(&keys)?);

        let mut sessions =
            db.prepare("SELECT jid, device, session FROM omemo_sessions WHERE account = ?1")?;
        let rows = sessions.query_map([account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        for row in rows {
            let (jid, device, session) = row?;
            state
                .sessions
                .insert((parse_jid(&jid)?, device), postcard::from_bytes(&session)?);
        }

        let mut identities =
            db.prepare("SELECT jid, device, key FROM omemo_identities WHERE account = ?1")?;
        let rows = identities.query_map([account.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        for row in rows {
            let (jid, device, key) = row?;
            let key: [u8; 32] = key
                .try_into()
                .map_err(|_| Error::Jid("identity key is not 32 bytes".into()))?;
            state.identities.insert((parse_jid(&jid)?, device), key);
        }
        let mut positions =
            db.prepare("SELECT archive, last_id FROM archive_positions WHERE account = ?1")?;
        let rows = positions.query_map([account.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (archive, id) = row?;
            state.archive_positions.insert(parse_jid(&archive)?, id);
        }
        Ok(Some(state))
    }

    /// What [`omtalk_omemo::Omemo::persisting`] writes `account`'s changes through.
    pub fn omemo(&self, account: &BareJid) -> Arc<dyn Persist> {
        Arc::new(AccountOmemo {
            store: self.clone(),
            account: account.clone(),
        })
    }
}

fn parse_jid(text: &str) -> Result<BareJid, Error> {
    text.parse()
        .map_err(|e: omtalk_xmpp::jid::Error| Error::Jid(e.to_string()))
}

fn migrate(db: &mut Connection) -> Result<(), Error> {
    let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > MIGRATIONS.len() as i64 {
        return Err(Error::NewerSchema(version));
    }
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = db.transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", index as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map(drop)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

struct AccountOmemo {
    store: Store,
    account: BareJid,
}

impl AccountOmemo {
    fn write(&self, sql: &str, params: impl rusqlite::Params) -> Result<(), PersistError> {
        let db = self.store.db.lock().unwrap();
        db.execute(sql, params)
            .map(drop)
            .map_err(|e| PersistError(e.to_string()))
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, PersistError> {
    postcard::to_stdvec(value).map_err(|e| PersistError(e.to_string()))
}

impl Persist for AccountOmemo {
    fn keys(&self, keys: &OwnKeys) -> Result<(), PersistError> {
        self.write(
            "INSERT OR REPLACE INTO omemo_keys (account, keys) VALUES (?1, ?2)",
            params![self.account.as_str(), encode(keys)?],
        )
    }

    fn session(&self, jid: &BareJid, device: DeviceId, session: &Session) -> Result<(), PersistError> {
        self.write(
            "INSERT OR REPLACE INTO omemo_sessions (account, jid, device, session) VALUES (?1, ?2, ?3, ?4)",
            params![self.account.as_str(), jid.as_str(), device, encode(session)?],
        )
    }

    fn archive_position(&self, archive: &BareJid, id: &str) -> Result<(), PersistError> {
        self.write(
            "INSERT OR REPLACE INTO archive_positions (account, archive, last_id) VALUES (?1, ?2, ?3)",
            params![self.account.as_str(), archive.as_str(), id],
        )
    }

    fn forget_archive_position(&self, archive: &BareJid) -> Result<(), PersistError> {
        self.write(
            "DELETE FROM archive_positions WHERE account = ?1 AND archive = ?2",
            params![self.account.as_str(), archive.as_str()],
        )
    }

    fn identity(&self, jid: &BareJid, device: DeviceId, key: &[u8; 32]) -> Result<(), PersistError> {
        self.write(
            "INSERT OR REPLACE INTO omemo_identities (account, jid, device, key) VALUES (?1, ?2, ?3, ?4)",
            params![self.account.as_str(), jid.as_str(), device, key.as_slice()],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    #[test]
    fn state_comes_back_as_it_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let alice = jid("alice@example.org");
        let bob = jid("bob@example.org");

        let keys = OwnKeys::generate(0).unwrap();
        let peer = OwnKeys::generate(0).unwrap();
        let mut session = Session::initiate(&keys, &peer.bundle()).unwrap();
        let first = session.encrypt(b"before restart");
        {
            let store = Store::open(&path).unwrap();
            let persist = store.omemo(&alice);
            persist.keys(&keys).unwrap();
            persist.session(&bob, peer.device_id, &session).unwrap();
            persist
                .identity(&bob, peer.device_id, &peer.bundle().identity_key)
                .unwrap();
            persist.archive_position(&alice, "A17").unwrap();
        }

        let store = Store::open(&path).unwrap();
        let mut state = store.load_omemo(&alice).unwrap().unwrap();
        assert_eq!(state.keys.device_id, keys.device_id);
        assert_eq!(
            state.keys.bundle(),
            keys.bundle(),
            "pre keys and signed pre key survive"
        );
        assert_eq!(
            state.identities[&(bob.clone(), peer.device_id)],
            peer.bundle().identity_key
        );
        assert_eq!(
            state.archive_positions[&alice], "A17",
            "catch-up resumes where it stopped"
        );

        // The restored session continues where the saved one stopped: the peer reads
        // the next message in the same session the first one started.
        let restored = state.sessions.get_mut(&(bob, peer.device_id)).unwrap();
        let second = restored.encrypt(b"after restart");
        let received = omemo::receive(&peer, None, &first.data, first.prekey).unwrap();
        let next = omemo::receive(&peer, Some(&received.session), &second.data, second.prekey).unwrap();
        assert_eq!(next.key_material, b"after restart");

        assert!(store.load_omemo(&jid("nobody@example.org")).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn database_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        Store::open(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn reopening_does_not_rerun_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        Store::open(&path).unwrap();
        Store::open(&path).unwrap();
    }
}
