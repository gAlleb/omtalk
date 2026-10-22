//! Chat history and the rooms to enter on start, per account.

use omtalk_xmpp::BareJid;
use rusqlite::params;

use crate::{Error, Store, parse_jid};

/// Schema 3; listed in `MIGRATIONS`.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE messages (
        account   TEXT NOT NULL,
        chat      TEXT NOT NULL,
        sender    TEXT NOT NULL,
        nick      TEXT,
        outgoing  INTEGER NOT NULL,
        encrypted INTEGER NOT NULL,
        body      TEXT NOT NULL,
        at        INTEGER NOT NULL
    );
    CREATE INDEX messages_by_chat ON messages (account, chat, at);
    CREATE TABLE rooms (
        account TEXT NOT NULL,
        room    TEXT NOT NULL,
        nick    TEXT NOT NULL,
        PRIMARY KEY (account, room)
    );
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// The peer, or the room.
    pub chat: BareJid,
    pub sender: BareJid,
    pub nick: Option<String>,
    pub outgoing: bool,
    pub encrypted: bool,
    pub body: String,
    /// Unix time, seconds.
    pub at: i64,
}

/// A chat with history, newest activity first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub chat: BareJid,
    pub last_at: i64,
}

impl Store {
    pub fn save_message(&self, account: &BareJid, message: &StoredMessage) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "INSERT INTO messages (account, chat, sender, nick, outgoing, encrypted, body, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                account.as_str(),
                message.chat.as_str(),
                message.sender.as_str(),
                message.nick,
                message.outgoing,
                message.encrypted,
                message.body,
                message.at
            ],
        )?;
        Ok(())
    }

    /// The last `limit` messages of a chat, oldest first.
    pub fn recent_messages(
        &self,
        account: &BareJid,
        chat: &BareJid,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT sender, nick, outgoing, encrypted, body, at FROM messages
             WHERE account = ?1 AND chat = ?2 ORDER BY at DESC, rowid DESC LIMIT ?3",
        )?;
        let rows = query.query_map(params![account.as_str(), chat.as_str(), limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, bool>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (sender, nick, outgoing, encrypted, body, at) = row?;
            messages.push(StoredMessage {
                chat: chat.clone(),
                sender: parse_jid(&sender)?,
                nick,
                outgoing,
                encrypted,
                body,
                at,
            });
        }
        messages.reverse();
        Ok(messages)
    }

    pub fn chats(&self, account: &BareJid) -> Result<Vec<ChatSummary>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT chat, MAX(at) FROM messages WHERE account = ?1 GROUP BY chat ORDER BY MAX(at) DESC",
        )?;
        let rows = query.query_map([account.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut chats = Vec::new();
        for row in rows {
            let (chat, last_at) = row?;
            chats.push(ChatSummary {
                chat: parse_jid(&chat)?,
                last_at,
            });
        }
        Ok(chats)
    }

    /// Rooms entered, to enter again on start, with the nick used.
    pub fn rooms(&self, account: &BareJid) -> Result<Vec<(BareJid, String)>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT room, nick FROM rooms WHERE account = ?1 ORDER BY room")?;
        let rows = query.query_map([account.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut rooms = Vec::new();
        for row in rows {
            let (room, nick) = row?;
            rooms.push((parse_jid(&room)?, nick));
        }
        Ok(rooms)
    }

    pub fn remember_room(&self, account: &BareJid, room: &BareJid, nick: &str) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO rooms (account, room, nick) VALUES (?1, ?2, ?3)",
            params![account.as_str(), room.as_str(), nick],
        )?;
        Ok(())
    }

    pub fn forget_room(&self, account: &BareJid, room: &BareJid) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "DELETE FROM rooms WHERE account = ?1 AND room = ?2",
            params![account.as_str(), room.as_str()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    fn message(chat: &str, body: &str, at: i64) -> StoredMessage {
        StoredMessage {
            chat: jid(chat),
            sender: jid("bob@example.org"),
            nick: None,
            outgoing: false,
            encrypted: true,
            body: body.into(),
            at,
        }
    }

    #[test]
    fn history_comes_back_per_account_and_chat_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let me = jid("me@example.org");
        let other_account = jid("work@example.org");
        for (body, at) in [("один", 10), ("два", 20), ("три", 30)] {
            store
                .save_message(&me, &message("bob@example.org", body, at))
                .unwrap();
        }
        store
            .save_message(&me, &message("room@muc.example.org", "в комнате", 25))
            .unwrap();
        store
            .save_message(&other_account, &message("bob@example.org", "с работы", 40))
            .unwrap();

        let last_two: Vec<_> = store
            .recent_messages(&me, &jid("bob@example.org"), 2)
            .unwrap()
            .into_iter()
            .map(|m| m.body)
            .collect();
        assert_eq!(last_two, ["два", "три"], "the newest ones, shown oldest first");
        let chats: Vec<_> = store
            .chats(&me)
            .unwrap()
            .into_iter()
            .map(|c| c.chat.to_string())
            .collect();
        assert_eq!(
            chats,
            ["bob@example.org", "room@muc.example.org"],
            "by last activity; other accounts apart"
        );
    }

    #[test]
    fn rooms_are_remembered_and_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let me = jid("me@example.org");
        store.remember_room(&me, &jid("a@muc.example.org"), "me").unwrap();
        store.remember_room(&me, &jid("b@muc.example.org"), "me").unwrap();
        store
            .remember_room(&me, &jid("a@muc.example.org"), "other nick")
            .unwrap();
        store.forget_room(&me, &jid("b@muc.example.org")).unwrap();
        assert_eq!(
            store.rooms(&me).unwrap(),
            [(jid("a@muc.example.org"), "other nick".to_owned())]
        );
    }
}
