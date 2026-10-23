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

/// Schema 9: a message's id (receipts, markers and replies name it) and, for one's own,
/// how far it got: [`Delivery`] codes, the reason with a failure.
pub(crate) const MIGRATION_DELIVERY: &str = "
    ALTER TABLE messages ADD COLUMN id TEXT;
    ALTER TABLE messages ADD COLUMN delivery INTEGER;
    ALTER TABLE messages ADD COLUMN failure TEXT;
    CREATE INDEX messages_by_id ON messages (account, id);
";

/// Schema 10: the body is a file its sender shared (XEP-0066), not a link written.
pub(crate) const MIGRATION_SHARED: &str = "
    ALTER TABLE messages ADD COLUMN shared INTEGER NOT NULL DEFAULT 0;
";

/// Schema 11: what the message answers, the archive's id for it (a reply in a room
/// names that), and whether it came from a device not trusted: kept, so that after a
/// restart a reply still shows what it answers and a ⚠ stays.
pub(crate) const MIGRATION_REPLIES: &str = "
    ALTER TABLE messages ADD COLUMN reply_to TEXT;
    ALTER TABLE messages ADD COLUMN archive_id TEXT;
    ALTER TABLE messages ADD COLUMN untrusted INTEGER NOT NULL DEFAULT 0;
";

/// Schema 12: chats known without a message kept, as one a new device finds in the
/// account's archive with nothing it can read: still listed after a restart.
pub(crate) const MIGRATION_KNOWN: &str = "
    CREATE TABLE known_chats (
        account TEXT NOT NULL,
        chat    TEXT NOT NULL,
        PRIMARY KEY (account, chat)
    );
";

/// How far one's own message got, as kept: each step only moves forward, and a failure
/// is final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Delivered,
    Read,
    Failed(String),
}

impl Delivery {
    fn code(&self) -> i64 {
        match self {
            Delivery::Sent => 1,
            Delivery::Delivered => 2,
            Delivery::Read => 3,
            Delivery::Failed(_) => 9,
        }
    }

    fn from_code(code: Option<i64>, failure: Option<String>) -> Option<Self> {
        match code? {
            1 => Some(Delivery::Sent),
            2 => Some(Delivery::Delivered),
            3 => Some(Delivery::Read),
            9 => Some(Delivery::Failed(failure.unwrap_or_default())),
            _ => None,
        }
    }
}

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
    /// The sender's id for it, if it named one.
    pub id: Option<String>,
    /// For one's own, sent from here: how far it got.
    pub delivery: Option<Delivery>,
    /// The body is a file shared, not a link written (XEP-0066).
    pub shared_file: bool,
    /// The id of the message this one answers (XEP-0461).
    pub reply_to: Option<String>,
    /// The archive's id for it (the room's, in a room).
    pub archive_id: Option<String>,
    /// Written on a device refused or not decided on.
    pub untrusted: bool,
}

/// A chat with history, newest activity first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub chat: BareJid,
    pub last_at: i64,
}

impl Store {
    /// Saves a message; a chat closed before opens again with it.
    pub fn save_message(&self, account: &BareJid, message: &StoredMessage) -> Result<(), Error> {
        self.reopen_chat(account, &message.chat)?;
        // A message that has an id is kept once: the archive read again, a carbon and
        // its original, bring it more than once.
        self.db.lock().unwrap().execute(
            "INSERT INTO messages (account, chat, sender, nick, outgoing, encrypted, body, at, id, delivery, failure, shared, reply_to, archive_id, untrusted)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15
             WHERE ?9 IS NULL OR NOT EXISTS (
                 SELECT 1 FROM messages WHERE account = ?1 AND chat = ?2 AND sender = ?3 AND id = ?9
             )",
            params![
                account.as_str(),
                message.chat.as_str(),
                message.sender.as_str(),
                message.nick,
                message.outgoing,
                message.encrypted,
                message.body,
                message.at,
                message.id,
                message.delivery.as_ref().map(Delivery::code),
                match &message.delivery {
                    Some(Delivery::Failed(why)) => Some(why.as_str()),
                    _ => None,
                },
                message.shared_file,
                message.reply_to,
                message.archive_id,
                message.untrusted,
            ],
        )?;
        Ok(())
    }

    /// [`Store::set_delivery`] for the message `id` in `chat` only.
    pub fn set_delivery_in(
        &self,
        account: &BareJid,
        chat: &BareJid,
        id: &str,
        delivery: &Delivery,
    ) -> Result<(), Error> {
        let failure = match delivery {
            Delivery::Failed(why) => Some(why.as_str()),
            _ => None,
        };
        self.db.lock().unwrap().execute(
            "UPDATE messages SET delivery = ?4, failure = ?5
             WHERE account = ?1 AND chat = ?2 AND id = ?3 AND outgoing = 1 AND (delivery IS NULL OR delivery < ?4)",
            params![account.as_str(), chat.as_str(), id, delivery.code(), failure],
        )?;
        Ok(())
    }

    /// One's own message `id` got further; an older step (a receipt after the read
    /// marker) changes nothing, and a failure stays.
    pub fn set_delivery(&self, account: &BareJid, id: &str, delivery: &Delivery) -> Result<(), Error> {
        let failure = match delivery {
            Delivery::Failed(why) => Some(why.as_str()),
            _ => None,
        };
        self.db.lock().unwrap().execute(
            "UPDATE messages SET delivery = ?3, failure = ?4
             WHERE account = ?1 AND id = ?2 AND outgoing = 1 AND (delivery IS NULL OR delivery < ?3)",
            params![account.as_str(), id, delivery.code(), failure],
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
            "SELECT sender, nick, outgoing, encrypted, body, at, id, delivery, failure, shared, reply_to, archive_id, untrusted FROM messages
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
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<i64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, bool>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<String>>(11)?,
                r.get::<_, bool>(12)?,
            ))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (
                sender,
                nick,
                outgoing,
                encrypted,
                body,
                at,
                id,
                delivery,
                failure,
                shared_file,
                reply_to,
                archive_id,
                untrusted,
            ) = row?;
            messages.push(StoredMessage {
                chat: chat.clone(),
                sender: parse_jid(&sender)?,
                nick,
                outgoing,
                encrypted,
                body,
                at,
                id,
                delivery: Delivery::from_code(delivery, failure),
                shared_file,
                reply_to,
                archive_id,
                untrusted,
            });
        }
        messages.reverse();
        Ok(messages)
    }

    /// Keeps `chat` in the list though no message of it is kept. A chat closed stays
    /// closed.
    pub fn keep_chat(&self, account: &BareJid, chat: &BareJid) -> Result<(), Error> {
        self.db.lock().unwrap().execute(
            "INSERT OR IGNORE INTO known_chats (account, chat) VALUES (?1, ?2)",
            params![account.as_str(), chat.as_str()],
        )?;
        Ok(())
    }

    /// The chats with history, and those kept without ([`Store::keep_chat`]) after them.
    pub fn chats(&self, account: &BareJid) -> Result<Vec<ChatSummary>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT chat, MAX(at) FROM (
                 SELECT chat, at FROM messages WHERE account = ?1
                 UNION ALL SELECT chat, 0 FROM known_chats WHERE account = ?1
             ) GROUP BY chat ORDER BY MAX(at) DESC",
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
            id: None,
            delivery: None,
            shared_file: false,
            reply_to: None,
            archive_id: None,
            untrusted: false,
        }
    }

    #[test]
    fn a_message_with_an_id_is_kept_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let me = jid("me@example.org");
        let named = StoredMessage {
            id: Some("x1".into()),
            ..message("bob@example.org", "привет", 1)
        };
        store.save_message(&me, &named).unwrap();
        store.save_message(&me, &named).unwrap();
        let unnamed = message("bob@example.org", "без id", 2);
        store.save_message(&me, &unnamed).unwrap();
        store.save_message(&me, &unnamed).unwrap();
        let bodies: Vec<String> = store
            .recent_messages(&me, &jid("bob@example.org"), 10)
            .unwrap()
            .into_iter()
            .map(|m| m.body)
            .collect();
        assert_eq!(
            bodies,
            ["привет", "без id", "без id"],
            "one with an id once; without, as they come"
        );
    }

    #[test]
    fn a_reply_its_archive_id_and_an_untrusted_device_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let me = jid("me@example.org");
        let reply = StoredMessage {
            reply_to: Some("orig-1".into()),
            archive_id: Some("arch-7".into()),
            untrusted: true,
            ..message("bob@example.org", "да", 1)
        };
        Store::open(&path).unwrap().save_message(&me, &reply).unwrap();
        let read = Store::open(&path)
            .unwrap()
            .recent_messages(&me, &jid("bob@example.org"), 10)
            .unwrap();
        assert_eq!(read, [reply]);
    }

    #[test]
    fn own_messages_keep_how_far_they_got() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let me = jid("me@example.org");
        let own = StoredMessage {
            sender: me.clone(),
            outgoing: true,
            id: Some("m1".into()),
            delivery: Some(Delivery::Sent),
            ..message("bob@example.org", "привет", 1)
        };
        store.save_message(&me, &own).unwrap();
        store
            .save_message(
                &me,
                &StoredMessage {
                    id: Some("m2".into()),
                    ..own.clone()
                },
            )
            .unwrap();
        let state = |id: &str| {
            store
                .recent_messages(&me, &jid("bob@example.org"), 10)
                .unwrap()
                .into_iter()
                .find(|m| m.id.as_deref() == Some(id))
                .unwrap()
                .delivery
        };
        store.set_delivery(&me, "m1", &Delivery::Read).unwrap();
        store.set_delivery(&me, "m1", &Delivery::Delivered).unwrap();
        assert_eq!(
            state("m1"),
            Some(Delivery::Read),
            "a late receipt does not undo the read marker"
        );
        store
            .set_delivery(&me, "m2", &Delivery::Failed("отказано".into()))
            .unwrap();
        store.set_delivery(&me, "m2", &Delivery::Read).unwrap();
        assert_eq!(
            state("m2"),
            Some(Delivery::Failed("отказано".into())),
            "a failure stays"
        );
    }

    #[test]
    fn a_chat_kept_without_messages_is_listed_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let me = jid("me@example.org");
        {
            let store = Store::open(&path).unwrap();
            store.keep_chat(&me, &jid("alice@example.org")).unwrap();
            store.keep_chat(&me, &jid("bob@example.org")).unwrap();
            store.keep_chat(&me, &jid("bob@example.org")).unwrap();
            store
                .save_message(&me, &message("bob@example.org", "привет", 5))
                .unwrap();
        }
        let chats: Vec<BareJid> = Store::open(&path)
            .unwrap()
            .chats(&me)
            .unwrap()
            .into_iter()
            .map(|c| c.chat)
            .collect();
        assert_eq!(
            chats,
            [jid("bob@example.org"), jid("alice@example.org")],
            "once each, those with history first"
        );
        assert!(
            Store::open(&path)
                .unwrap()
                .chats(&jid("other@example.org"))
                .unwrap()
                .is_empty()
        );
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
