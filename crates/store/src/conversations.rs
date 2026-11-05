//! Typed history facade. The original XMPP tables and their rowids are untouched.
use crate::{Error, Store};
use omtalk_core::{AccountId, ChatMessage, ConversationId, MessageId, NetworkId};
use rusqlite::{OptionalExtension, params};

pub(crate) const MIGRATION: &str = "
CREATE TABLE irc_conversations (
    conversation TEXT PRIMARY KEY, account TEXT NOT NULL, network TEXT NOT NULL,
    state TEXT NOT NULL, read_time INTEGER, cursor_time TEXT, cursor_id TEXT, cursor_nanos INTEGER
);
CREATE INDEX irc_conversations_account ON irc_conversations(account);
CREATE TABLE irc_messages (
    local_id TEXT PRIMARY KEY, conversation TEXT NOT NULL, network TEXT NOT NULL,
    server_id TEXT, at INTEGER NOT NULL, outgoing INTEGER NOT NULL,
    historical INTEGER NOT NULL DEFAULT 0, body TEXT NOT NULL, message TEXT NOT NULL
);
CREATE UNIQUE INDEX irc_messages_server_id ON irc_messages(network, server_id)
    WHERE server_id IS NOT NULL;
CREATE INDEX irc_messages_history ON irc_messages(conversation, at, local_id);
";

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConversationState {
    /// Current wire spelling, separate from the stable local conversation identity.
    pub target: String,
    pub hidden: bool,
    pub muted: bool,
    pub folder: Option<String>,
    pub folder_position: i64,
    pub position: i64,
    pub draft: String,
    pub joined: bool,
    /// Direct IRC only: reconnect membership explicitly chosen by the user.
    /// soju remains authoritative for its upstream channels.
    pub desired_join: bool,
    /// A history page has been fetched, not merely a live message received.
    pub history_loaded: bool,
    pub notify: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryPage {
    pub messages: Vec<ChatMessage>,
    pub exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    Inserted,
    Merged(MessageId),
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|e| Error::Conversation(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, Error> {
    serde_json::from_str(text).map_err(|e| Error::Conversation(e.to_string()))
}
fn network(conversation: &ConversationId) -> Result<&NetworkId, Error> {
    match conversation {
        ConversationId::Irc { network, .. } => Ok(network),
        _ => Err(Error::Conversation("expected IRC conversation".into())),
    }
}
fn nanos(at: chrono::DateTime<chrono::Utc>) -> Result<i64, Error> {
    at.timestamp_nanos_opt()
        .ok_or_else(|| Error::Conversation("timestamp is out of range".into()))
}

/// Common store dispatch preserves the complete protocol payload, including OMEMO,
/// trust, replies, edits, receipts and archive IDs on the XMPP side.
#[derive(Debug, Clone)]
pub enum HistoryMessage {
    Xmpp {
        account: AccountId,
        message: crate::StoredMessage,
    },
    Irc(ChatMessage),
}
#[derive(Debug, Clone)]
pub enum HistoryCursor {
    XmppOffset(usize),
    IrcMessage(String),
}

impl Store {
    pub fn save_history(&self, message: &HistoryMessage, older: bool) -> Result<SaveOutcome, Error> {
        match message {
            HistoryMessage::Xmpp { account, message } => {
                if account.protocol != omtalk_core::Protocol::Xmpp {
                    return Err(Error::Conversation("XMPP account required".into()));
                }
                self.save_message(&crate::parse_jid(&account.id)?, message)?;
                Ok(SaveOutcome::Inserted)
            }
            HistoryMessage::Irc(message) => self.save_chat_message(message, older),
        }
    }
    pub fn history(
        &self,
        conversation: &ConversationId,
        cursor: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<Vec<HistoryMessage>, Error> {
        match (conversation, cursor) {
            (ConversationId::Xmpp { account, jid }, None | Some(HistoryCursor::XmppOffset(_))) => {
                let skip = match cursor {
                    Some(HistoryCursor::XmppOffset(n)) => *n,
                    _ => 0,
                };
                Ok(self
                    .older_messages(
                        &crate::parse_jid(&account.id)?,
                        &crate::parse_jid(jid)?,
                        skip,
                        limit,
                    )?
                    .into_iter()
                    .map(|message| HistoryMessage::Xmpp {
                        account: account.clone(),
                        message,
                    })
                    .collect())
            }
            (ConversationId::Irc { .. }, None | Some(HistoryCursor::IrcMessage(_))) => {
                let before = match cursor {
                    Some(HistoryCursor::IrcMessage(id)) => Some(id.as_str()),
                    _ => None,
                };
                Ok(self
                    .history_page(conversation, before, limit)?
                    .messages
                    .into_iter()
                    .map(HistoryMessage::Irc)
                    .collect())
            }
            _ => Err(Error::Conversation(
                "history cursor belongs to a different protocol".into(),
            )),
        }
    }

    pub fn save_conversation(
        &self,
        conversation: &ConversationId,
        state: &ConversationState,
    ) -> Result<(), Error> {
        let network = network(conversation)?;
        self.db.lock().unwrap().execute(
            "INSERT INTO irc_conversations(conversation,account,network,state) VALUES(?1,?2,?3,?4)
             ON CONFLICT(conversation) DO UPDATE SET state=excluded.state",
            params![
                json(conversation)?,
                json(conversation.account())?,
                json(network)?,
                json(state)?
            ],
        )?;
        Ok(())
    }

    pub fn conversations(
        &self,
        account: &AccountId,
    ) -> Result<Vec<(ConversationId, ConversationState)>, Error> {
        let db = self.db.lock().unwrap();
        let mut q =
            db.prepare("SELECT conversation,state FROM irc_conversations WHERE account=?1 ORDER BY rowid")?;
        q.query_map([json(account)?], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, state) = row?;
            Ok((decode(&id)?, decode(&state)?))
        })
        .collect()
    }

    /// Server IDs are scoped to a network. Equal bodies/timestamps are never enough
    /// to merge: two consecutive identical messages must remain two messages.
    pub fn save_chat_message(&self, message: &ChatMessage, older: bool) -> Result<SaveOutcome, Error> {
        let network = network(&message.conversation)?;
        {
            {
                let mut db = self.db.lock().unwrap();
                let tx = db.transaction()?;
                let existing: Option<(String, String)> = tx.query_row(
                    "SELECT local_id,message FROM irc_messages WHERE local_id=?1 OR (network=?2 AND server_id=?3) LIMIT 1",
                    params![message.id.0, json(network)?, message.server_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                let outcome = if let Some((id, old)) = existing {
                    let mut fresh = message.clone();
                    let old: ChatMessage = decode(&old)?;
                    fresh.id = MessageId(id.clone());
                    fresh.conversation = old.conversation;
                    fresh.server_id = fresh.server_id.or(old.server_id);
                    if fresh.server_time.is_none() && old.server_time.is_some() {
                        fresh.at = old.at;
                    }
                    fresh.server_time = fresh.server_time.or(old.server_time);
                    // A later nick change can make replay look incoming. A proven
                    // own copy with this same server ID remains ours.
                    fresh.outgoing |= old.outgoing;
                    if fresh.attachment.is_none() {
                        fresh.attachment = old.attachment;
                    }
                    tx.execute("UPDATE irc_messages SET server_id=COALESCE(?2,server_id),at=?3,message=?4,outgoing=?5,body=?6 WHERE local_id=?1",
                        params![id, fresh.server_id, nanos(fresh.at)?, json(&fresh)?, fresh.outgoing, fresh.text])?;
                    SaveOutcome::Merged(MessageId(id))
                } else {
                    tx.execute("INSERT INTO irc_messages(local_id,conversation,network,server_id,at,outgoing,historical,body,message)
                        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![message.id.0, json(&message.conversation)?, json(network)?,
                        message.server_id, nanos(message.at)?, message.outgoing, older, message.text, json(message)?])?;
                    SaveOutcome::Inserted
                };
                if !older && let Some(time) = &message.server_time {
                    tx.execute("UPDATE irc_conversations SET cursor_time=?2,cursor_id=?3,cursor_nanos=?4 WHERE conversation=?1
                        AND (cursor_nanos IS NULL OR cursor_nanos<=?4)",
                        params![json(&message.conversation)?, time, message.server_id, nanos(message.at)?])?;
                }
                tx.commit()?;
                Ok(outcome)
            }
        }
    }

    /// Stable local paging uses the oldest message ID, including all timestamp ties.
    pub fn history_page(
        &self,
        conversation: &ConversationId,
        before: Option<&str>,
        limit: usize,
    ) -> Result<HistoryPage, Error> {
        network(conversation)?;
        let db = self.db.lock().unwrap();
        let mut q = db.prepare("SELECT message FROM irc_messages WHERE conversation=?1 AND
            (?2 IS NULL OR (at,rowid)<(SELECT at,rowid FROM irc_messages WHERE local_id=?2 AND conversation=?1))
            ORDER BY at DESC,rowid DESC LIMIT ?3")?;
        let mut messages: Vec<ChatMessage> = q
            .query_map(params![json(conversation)?, before, limit as i64], |r| {
                r.get::<_, String>(0)
            })?
            .map(|r| decode(&r?))
            .collect::<Result<_, _>>()?;
        messages.reverse();
        Ok(HistoryPage {
            exhausted: messages.len() < limit,
            messages,
        })
    }

    pub fn search_conversation(
        &self,
        conversation: &ConversationId,
        text: &str,
        limit: usize,
    ) -> Result<Vec<ChatMessage>, Error> {
        network(conversation)?;
        let db = self.db.lock().unwrap();
        let mut q =
            db.prepare("SELECT message FROM irc_messages WHERE conversation=?1 ORDER BY at DESC,rowid DESC")?;
        let words: Vec<_> = text.split_whitespace().map(str::to_lowercase).collect();
        let mut found = Vec::new();
        for row in q.query_map([json(conversation)?], |r| r.get::<_, String>(0))? {
            let message: ChatMessage = decode(&row?)?;
            let body = message.text.to_lowercase();
            if words.iter().all(|word| body.contains(word)) {
                found.push(message);
            }
            if found.len() >= limit {
                break;
            }
        }
        Ok(found)
    }

    pub fn first_conversation_message(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<ChatMessage>, Error> {
        network(conversation)?;
        let value: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT message FROM irc_messages WHERE conversation=?1 ORDER BY at,rowid LIMIT 1",
                [json(conversation)?],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| decode(&s)).transpose()
    }

    pub fn chat_message(&self, id: &MessageId) -> Result<Option<ChatMessage>, Error> {
        let value: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT message FROM irc_messages WHERE local_id=?1",
                [&id.0],
                |r| r.get(0),
            )
            .optional()?;
        value.map(|s| decode(&s)).transpose()
    }

    pub fn conversation_from_time(
        &self,
        conversation: &ConversationId,
        seconds: i64,
        limit: usize,
    ) -> Result<Vec<ChatMessage>, Error> {
        network(conversation)?;
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT message FROM irc_messages WHERE conversation=?1 AND at>=?2 ORDER BY at,rowid LIMIT ?3",
        )?;
        query
            .query_map(
                params![
                    json(conversation)?,
                    seconds.saturating_mul(1_000_000_000),
                    limit.min(i64::MAX as usize) as i64
                ],
                |r| r.get::<_, String>(0),
            )?
            .map(|s| decode(&s?))
            .collect()
    }

    pub fn conversation_read_time(&self, conversation: &ConversationId) -> Result<Option<String>, Error> {
        let db = self.db.lock().unwrap();
        let time: Option<i64> = db
            .query_row(
                "SELECT read_time FROM irc_conversations WHERE conversation=?1",
                [json(conversation)?],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(time.map(|t| {
            let at = chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(t);
            let precision = if at.timestamp_subsec_nanos().is_multiple_of(1_000_000) {
                chrono::SecondsFormat::Millis
            } else {
                chrono::SecondsFormat::Nanos
            };
            at.to_rfc3339_opts(precision, true)
        }))
    }

    pub fn conversation_cursor(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<(String, Option<String>)>, Error> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT cursor_time,cursor_id FROM irc_conversations WHERE conversation=?1 AND cursor_time IS NOT NULL",
            [json(conversation)?], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(Into::into)
    }

    pub fn read_conversation(&self, conversation: &ConversationId, timestamp: &str) -> Result<(), Error> {
        network(conversation)?;
        let at = chrono::DateTime::parse_from_rfc3339(timestamp)
            .map_err(|e| Error::Conversation(e.to_string()))?
            .to_utc();
        self.db.lock().unwrap().execute(
            "UPDATE irc_conversations SET read_time=MAX(COALESCE(read_time,?2),?2) WHERE conversation=?1",
            params![json(conversation)?, nanos(at)?],
        )?;
        Ok(())
    }

    pub fn unread_conversation(&self, conversation: &ConversationId) -> Result<usize, Error> {
        let db = self.db.lock().unwrap();
        Ok(db.query_row("SELECT COUNT(*) FROM irc_messages WHERE conversation=?1 AND outgoing=0 AND historical=0
            AND at>COALESCE((SELECT read_time FROM irc_conversations WHERE conversation=?1),-9223372036854775808)",
            [json(conversation)?], |r| r.get::<_, i64>(0))? as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omtalk_core::{CaseMapping, ConversationKind, MessageKind, Network};

    #[test]
    fn migration_18_preserves_xmpp_rowids_read_frontier_and_omemo_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        for migration in &crate::MIGRATIONS[..17] {
            db.execute_batch(migration).unwrap();
        }
        db.pragma_update(None, "user_version", 17).unwrap();
        db.execute("INSERT INTO messages(rowid,account,chat,sender,outgoing,encrypted,body,at,id) VALUES(400,'a@b','c@d','c@d',0,1,'legacy',1,'old')",[]).unwrap();
        db.execute("INSERT INTO omemo_keys VALUES('a@b',X'01020304')", [])
            .unwrap();
        db.execute("INSERT INTO omemo_sessions VALUES('a@b','c@d',1,X'0001FF')", [])
            .unwrap();
        db.execute("INSERT INTO omemo_trust VALUES('a@b','c@d',1,2)", [])
            .unwrap();
        db.execute("INSERT INTO kept VALUES('read/a@b/c@d','400')", [])
            .unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        let db = store.db.lock().unwrap();
        assert_eq!(
            db.query_row("SELECT rowid,encrypted,body FROM messages", [], |r| Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, String>(2)?
            )))
            .unwrap(),
            (400, true, "legacy".into())
        );
        assert_eq!(
            db.query_row("SELECT hex(keys) FROM omemo_keys", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "01020304"
        );
        assert_eq!(
            db.query_row("SELECT hex(session) FROM omemo_sessions", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "0001FF"
        );
        assert_eq!(
            db.query_row("SELECT value FROM kept WHERE key='read/a@b/c@d'", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "400"
        );
        assert_eq!(
            db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            18
        );
    }

    #[test]
    fn network_scoped_ids_fractional_cursor_and_equal_timestamp_pages_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let store = Store::open(&path).unwrap();
        let mut ids = Vec::new();
        for (account, net) in [("one", "1"), ("one", "2"), ("two", "1")] {
            let id = ConversationId::irc(
                NetworkId {
                    account: AccountId::irc(account),
                    network: Network::Soju(net.into()),
                },
                ConversationKind::Channel,
                "#Same",
                CaseMapping::Rfc1459,
            );
            store
                .save_conversation(
                    &id,
                    &ConversationState {
                        target: "#Same".into(),
                        draft: "draft".into(),
                        folder: Some("folder".into()),
                        muted: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            for (n, time) in [
                (0, "2026-11-05T12:00:00.000000001Z"),
                (1, "2026-11-05T12:00:00.000000002Z"),
                (2, "2026-11-05T12:00:00.000000002Z"),
            ] {
                let m = ChatMessage {
                    conversation: id.clone(),
                    id: MessageId(format!("{account}-{net}-{n}")),
                    server_id: Some(format!("same-{n}")),
                    server_time: Some(time.into()),
                    at: time.parse().unwrap(),
                    sender: "same".into(),
                    outgoing: false,
                    kind: MessageKind::Text,
                    text: "одинаково".into(),
                    attachment: None,
                };
                assert_eq!(store.save_chat_message(&m, false).unwrap(), SaveOutcome::Inserted);
            }
            ids.push(id);
        }
        drop(store);
        let store = Store::open(&path).unwrap();
        for id in ids {
            let last = store.history_page(&id, None, 1).unwrap().messages.pop().unwrap();
            let previous = store
                .history_page(&id, Some(&last.id.0), 1)
                .unwrap()
                .messages
                .pop()
                .unwrap();
            assert_eq!(last.at, previous.at);
            assert_ne!(last.id, previous.id);
            assert_eq!(
                store.conversation_cursor(&id).unwrap().unwrap().0,
                "2026-11-05T12:00:00.000000002Z"
            );
            store
                .read_conversation(&id, "2026-11-05T12:00:00.000000001Z")
                .unwrap();
            assert_eq!(store.unread_conversation(&id).unwrap(), 2);
            assert_eq!(store.search_conversation(&id, "ОДИНАКОВО", 10).unwrap().len(), 3);
            assert!(
                store
                    .conversations(id.account())
                    .unwrap()
                    .iter()
                    .any(|(key, s)| key == &id && s.muted && s.draft == "draft")
            );
        }
    }

    #[test]
    fn replay_deduplicates_only_by_id_and_read_mark_never_moves_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db")).unwrap();
        let id = ConversationId::irc(
            NetworkId {
                account: AccountId::irc("home"),
                network: Network::Soju("1".into()),
            },
            ConversationKind::Channel,
            "#om",
            CaseMapping::Rfc1459,
        );
        store
            .save_conversation(
                &id,
                &ConversationState {
                    target: "#om".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut m = ChatMessage {
            conversation: id.clone(),
            id: MessageId("one".into()),
            server_id: Some("Aa".into()),
            server_time: Some("2026-11-05T10:00:00.123456789Z".into()),
            at: "2026-11-05T10:00:00.123456789Z".parse().unwrap(),
            sender: "friend".into(),
            outgoing: false,
            kind: MessageKind::Text,
            text: "same".into(),
            attachment: None,
        };
        assert_eq!(store.save_chat_message(&m, false).unwrap(), SaveOutcome::Inserted);
        m.id = MessageId("replay".into());
        assert_eq!(
            store.save_chat_message(&m, false).unwrap(),
            SaveOutcome::Merged(MessageId("one".into()))
        );
        m.server_id = Some("aa".into());
        m.id = MessageId("two".into());
        store.save_chat_message(&m, false).unwrap();
        assert_eq!(store.unread_conversation(&id).unwrap(), 2);
        store
            .read_conversation(&id, m.server_time.as_ref().unwrap())
            .unwrap();
        store.read_conversation(&id, "2020-01-01T00:00:00Z").unwrap();
        assert_eq!(store.unread_conversation(&id).unwrap(), 0);
        let rows = store.history_page(&id, None, 50).unwrap();
        assert_eq!(rows.messages.len(), 2);
        assert_eq!(rows.messages[0].server_time, m.server_time);
        assert!(
            store
                .load_omemo(&crate::parse_jid("home").unwrap())
                .unwrap()
                .is_none()
        );
    }
}
