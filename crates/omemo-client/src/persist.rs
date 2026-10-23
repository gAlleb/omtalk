use omemo::{DeviceId, OwnKeys, Session};
use omtalk_xmpp::BareJid;

/// Where [`crate::Omemo`] writes every change, as it happens. A session that moved on
/// in memory but not on disk breaks after a restart: the peer's next message no longer
/// decrypts.
pub trait Persist: Send + Sync {
    fn keys(&self, keys: &OwnKeys) -> Result<(), PersistError>;
    fn session(&self, jid: &BareJid, device: DeviceId, session: &Session) -> Result<(), PersistError>;
    fn identity(&self, jid: &BareJid, device: DeviceId, key: &[u8; 32]) -> Result<(), PersistError>;
    /// The last message id read from `archive`, so the next catch-up starts after it.
    fn archive_position(&self, archive: &BareJid, id: &str) -> Result<(), PersistError>;
    /// No position for `archive`: the next catch-up starts from its newest message.
    fn forget_archive_position(&self, archive: &BareJid) -> Result<(), PersistError>;
    /// The sessions with a device a newer one replaced, newest first (all of them: the
    /// list replaces what was kept).
    fn previous_sessions(
        &self,
        _jid: &BareJid,
        _device: DeviceId,
        _sessions: &[Session],
    ) -> Result<(), PersistError> {
        Ok(())
    }
    /// What the user decided about a device.
    fn trust(&self, _jid: &BareJid, _device: DeviceId, _trust: crate::Trust) -> Result<(), PersistError> {
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("saving OMEMO state failed: {0}")]
pub struct PersistError(pub String);

/// Keeps nothing; for tests and throwaway sessions.
pub struct Ephemeral;

impl Persist for Ephemeral {
    fn keys(&self, _: &OwnKeys) -> Result<(), PersistError> {
        Ok(())
    }
    fn session(&self, _: &BareJid, _: DeviceId, _: &Session) -> Result<(), PersistError> {
        Ok(())
    }
    fn identity(&self, _: &BareJid, _: DeviceId, _: &[u8; 32]) -> Result<(), PersistError> {
        Ok(())
    }
    fn archive_position(&self, _: &BareJid, _: &str) -> Result<(), PersistError> {
        Ok(())
    }
    fn forget_archive_position(&self, _: &BareJid) -> Result<(), PersistError> {
        Ok(())
    }
}
