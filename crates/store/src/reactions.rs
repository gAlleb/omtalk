//! Reactions to messages (XEP-0444): per message, each one's reactions to it, as they
//! last sent them.

use omtalk_xmpp::BareJid;
use rusqlite::params;

use crate::{Error, Store};

/// Schema 15; listed in `MIGRATIONS`. `message` is the id the reactions name (the
/// room's id for a room message); `reactions` the texts, one per line.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE reactions (
        account   TEXT NOT NULL,
        chat      TEXT NOT NULL,
        message   TEXT NOT NULL,
        by        TEXT NOT NULL,
        reactions TEXT NOT NULL,
        PRIMARY KEY (account, chat, message, by)
    );
";

/// One sender's reactions to one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReactions {
    pub message: String,
    pub by: String,
    pub reactions: Vec<String>,
}

impl Store {
    /// `by`'s reactions to `message` in `chat`, all of them: none takes them away.
    pub fn set_reactions(
        &self,
        account: &BareJid,
        chat: &BareJid,
        message: &str,
        by: &str,
        reactions: &[String],
    ) -> Result<(), Error> {
        let db = self.db.lock().unwrap();
        if reactions.is_empty() {
            db.execute(
                "DELETE FROM reactions WHERE account = ?1 AND chat = ?2 AND message = ?3 AND by = ?4",
                params![account.as_str(), chat.as_str(), message, by],
            )?;
        } else {
            db.execute(
                "INSERT OR REPLACE INTO reactions (account, chat, message, by, reactions)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![account.as_str(), chat.as_str(), message, by, reactions.join("\n")],
            )?;
        }
        Ok(())
    }

    /// The reactions kept in `chat`.
    pub fn reactions(&self, account: &BareJid, chat: &BareJid) -> Result<Vec<StoredReactions>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare(
            "SELECT message, by, reactions FROM reactions WHERE account = ?1 AND chat = ?2 ORDER BY message, by",
        )?;
        let rows = query.query_map(params![account.as_str(), chat.as_str()], |r| {
            Ok(StoredReactions {
                message: r.get(0)?,
                by: r.get(1)?,
                reactions: r.get::<_, String>(2)?.lines().map(str::to_owned).collect(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    #[test]
    fn reactions_are_replaced_whole_and_taken_back_by_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("omtalk.db")).unwrap();
        let (me, bob) = (jid("me@example.org"), jid("bob@example.org"));
        store
            .set_reactions(&me, &bob, "m1", "bob@example.org", &["👍".into()])
            .unwrap();
        store
            .set_reactions(&me, &bob, "m1", "bob@example.org", &["❤".into(), "😂".into()])
            .unwrap();
        store
            .set_reactions(&me, &bob, "m1", "me@example.org", &["👍".into()])
            .unwrap();
        let kept = store.reactions(&me, &bob).unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].reactions, ["❤", "😂"], "the last ones only");
        store
            .set_reactions(&me, &bob, "m1", "me@example.org", &[])
            .unwrap();
        assert_eq!(store.reactions(&me, &bob).unwrap().len(), 1);
        assert!(
            store
                .reactions(&me, &jid("carol@example.org"))
                .unwrap()
                .is_empty()
        );
    }
}
