//! Chats closed by the user: gone from the list, history kept. A new message in one
//! (saved through [`Store::save_message`]) opens it again.

use omtalk_xmpp::BareJid;
use rusqlite::params;

use crate::{Error, Store, parse_jid};

/// Schema 6; listed in `MIGRATIONS`.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE closed_chats (
        account TEXT NOT NULL,
        chat    TEXT NOT NULL,
        PRIMARY KEY (account, chat)
    );
";

impl Store {
    pub fn close_chat(&self, account: &BareJid, chat: &BareJid) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "INSERT OR IGNORE INTO closed_chats (account, chat) VALUES (?1, ?2)",
            params![account.as_str(), chat.as_str()],
        )?;
        Ok(())
    }

    pub fn reopen_chat(&self, account: &BareJid, chat: &BareJid) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "DELETE FROM closed_chats WHERE account = ?1 AND chat = ?2",
            params![account.as_str(), chat.as_str()],
        )?;
        Ok(())
    }

    pub fn closed_chats(&self, account: &BareJid) -> Result<Vec<BareJid>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT chat FROM closed_chats WHERE account = ?1")?;
        let rows: Vec<String> = query
            .query_map([account.as_str()], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        rows.iter().map(|c| parse_jid(c)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StoredMessage;

    #[test]
    fn a_closed_chat_stays_closed_until_someone_writes_in_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let (me, bob): (BareJid, BareJid) = (
            "me@example.org".parse().unwrap(),
            "bob@example.org".parse().unwrap(),
        );
        store.close_chat(&me, &bob).unwrap();
        store.close_chat(&me, &bob).unwrap();
        assert_eq!(store.closed_chats(&me).unwrap(), std::slice::from_ref(&bob));
        assert!(
            store
                .closed_chats(&"other@example.org".parse().unwrap())
                .unwrap()
                .is_empty()
        );
        let message = StoredMessage {
            chat: bob.clone(),
            sender: bob.clone(),
            nick: None,
            outgoing: false,
            encrypted: true,
            body: "снова я".into(),
            at: 1,
            id: None,
            delivery: None,
            shared_file: false,
            reply_to: None,
            archive_id: None,
            untrusted: false,
            edited: false,
        };
        store.save_message(&me, &message).unwrap();
        assert!(
            store.closed_chats(&me).unwrap().is_empty(),
            "a new message opens it"
        );
    }
}
