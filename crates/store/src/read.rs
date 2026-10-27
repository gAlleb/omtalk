//! The local read frontier also covers messages with no room/archive stanza-id.
//! Server synchronization is additional: an old or missing server mark cannot unread
//! what was already displayed here, even after a restart.

use rusqlite::{Connection, OptionalExtension, params};

use crate::{BareJid, Error, Store};

#[derive(Clone, Debug)]
pub struct ReadPoint {
    pub at: i64,
    pub id: Option<String>,
    pub archive_id: Option<String>,
    pub outgoing: bool,
}

fn key(account: &BareJid, chat: &BareJid) -> String {
    format!("read-local/{account}/{chat}")
}

fn local(db: &Connection, account: &BareJid, chat: &BareJid) -> Result<Option<(i64, i64)>, Error> {
    let value: Option<String> = db
        .query_row(
            "SELECT value FROM kept WHERE key = ?1",
            [key(account, chat)],
            |r| r.get(0),
        )
        .optional()?;
    Ok(value.and_then(|v| {
        let (at, row) = v.split_once('/')?;
        Some((at.parse().ok()?, row.parse().ok()?))
    }))
}

fn advance(db: &Connection, account: &BareJid, chat: &BareJid, cursor: (i64, i64)) -> Result<(), Error> {
    if local(db, account, chat)?.is_none_or(|old| cursor > old) {
        db.execute(
            "INSERT OR REPLACE INTO kept (key, value) VALUES (?1, ?2)",
            params![key(account, chat), format!("{}/{}", cursor.0, cursor.1)],
        )?;
    }
    Ok(())
}

impl Store {
    /// Remember precisely the displayed message, not newer messages already stored
    /// by a background task. Row order distinguishes messages in the same second.
    pub fn read_locally(&self, account: &BareJid, chat: &BareJid, point: &ReadPoint) -> Result<(), Error> {
        let db = self.db.lock().unwrap();
        let cursor: Option<(i64, i64)> = db
            .query_row(
                "SELECT at, rowid FROM message_history WHERE account = ?1 AND chat = ?2 AND outgoing = ?5 AND
             ((?3 IS NOT NULL AND id = ?3) OR (?4 IS NOT NULL AND archive_id = ?4) OR
              (?3 IS NULL AND ?4 IS NULL AND at = ?6)) ORDER BY at DESC, rowid DESC LIMIT 1",
                params![
                    account.as_str(),
                    chat.as_str(),
                    point.id,
                    point.archive_id,
                    point.outgoing,
                    point.at
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some(cursor) = cursor {
            advance(&db, account, chat, cursor)?;
        }
        Ok(())
    }

    /// A known remote frontier is read here too, and may never be moved back by
    /// a delayed publication from another client.
    pub fn read_remotely(&self, account: &BareJid, chat: &BareJid, id: &str) -> Result<(), Error> {
        if id.is_empty() {
            return Ok(());
        }
        let db = self.db.lock().unwrap();
        let cursor = db.query_row(
            "SELECT at, rowid FROM message_history WHERE account = ?1 AND chat = ?2 AND archive_id = ?3 ORDER BY at DESC, rowid DESC LIMIT 1",
            params![account.as_str(), chat.as_str(), id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        if let Some(cursor) = cursor {
            advance(&db, account, chat, cursor)?;
        }
        Ok(())
    }

    /// Unread after the later of the local read frontier and a known server marker.
    /// None means neither position is known here. No lexical ordering of opaque IDs.
    pub fn unread_after_mark(
        &self,
        account: &BareJid,
        chat: &BareJid,
        marker: Option<&str>,
    ) -> Result<Option<usize>, Error> {
        let db = self.db.lock().unwrap();
        let remote: Option<(i64, i64)> = match marker.filter(|id| !id.is_empty()) {
            Some(id) => db.query_row(
                "SELECT at, rowid FROM message_history WHERE account = ?1 AND chat = ?2 AND archive_id = ?3 ORDER BY at DESC, rowid DESC LIMIT 1",
                params![account.as_str(), chat.as_str(), id], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional()?,
            None => None,
        };
        let Some((at, row)) = local(&db, account, chat)?.max(remote) else {
            return Ok(None);
        };
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM message_history WHERE account = ?1 AND chat = ?2 AND outgoing = 0 AND (at, rowid) > (?3, ?4)",
            params![account.as_str(), chat.as_str(), at, row], |r| r.get(0),
        )?;
        Ok(Some(count as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredMessage;

    #[test]
    fn local_reads_cover_idless_messages_survive_restart_and_only_advance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let me: BareJid = "me@example.org".parse().unwrap();
        let other: BareJid = "other@example.org".parse().unwrap();
        let chat: BareJid = "#test%irc.example.org@irc.example.org".parse().unwrap();
        let elsewhere: BareJid = "#other%irc.example.org@irc.example.org".parse().unwrap();
        let message = |id: &str, archive: Option<&str>| StoredMessage {
            chat: chat.clone(),
            sender: "bob@example.org".parse().unwrap(),
            nick: Some("bob".into()),
            outgoing: false,
            encrypted: false,
            body: id.into(),
            at: 100,
            id: Some(id.into()),
            archive_id: archive.map(Into::into),
            delivery: None,
            shared_file: false,
            reply_to: None,
            untrusted: false,
            edited: false,
        };
        let point = |id: &str| ReadPoint {
            at: 100,
            id: Some(id.into()),
            archive_id: None,
            outgoing: false,
        };
        {
            let store = Store::open(&path).unwrap();
            for m in [
                message("old", Some("archive-old")),
                message("live-1", None),
                message("live-2", None),
            ] {
                store.save_message(&me, &m).unwrap();
            }
            // Do not mark newer data read just because a background task saved it.
            store.read_locally(&me, &chat, &point("live-1")).unwrap();
            assert_eq!(
                store.unread_after_mark(&me, &chat, Some("archive-old")).unwrap(),
                Some(1)
            );
            store.read_locally(&me, &chat, &point("live-2")).unwrap();
        }
        let store = Store::open(&path).unwrap();
        for marker in [None, Some("archive-old"), Some(""), Some("unknown")] {
            assert_eq!(store.unread_after_mark(&me, &chat, marker).unwrap(), Some(0));
        }
        store.read_locally(&me, &chat, &point("old")).unwrap();
        assert_eq!(store.unread_after_mark(&me, &chat, None).unwrap(), Some(0));
        assert_eq!(store.unread_after_mark(&other, &chat, None).unwrap(), None);
        assert_eq!(store.unread_after_mark(&me, &elsewhere, None).unwrap(), None);
        store
            .save_message(&me, &message("new", Some("archive-new")))
            .unwrap();
        assert_eq!(store.unread_after_mark(&me, &chat, None).unwrap(), Some(1));
        store.read_remotely(&me, &chat, "archive-new").unwrap();
        store.read_remotely(&me, &chat, "archive-old").unwrap();
        assert_eq!(
            store.unread_after_mark(&me, &chat, Some("archive-old")).unwrap(),
            Some(0)
        );
    }
}
