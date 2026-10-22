//! The user's own folders of chats: omtalk's alone, not the roster's groups, so they can
//! hold chats of every account, rooms too.

use omtalk_xmpp::BareJid;
use rusqlite::params;

use crate::{Error, Store, parse_jid};

/// Schema 4; listed in `MIGRATIONS`.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE folders (
        name     TEXT PRIMARY KEY,
        position INTEGER NOT NULL
    );
    CREATE TABLE folder_chats (
        folder  TEXT NOT NULL,
        account TEXT NOT NULL,
        chat    TEXT NOT NULL,
        position INTEGER NOT NULL,
        -- A chat is in one folder at most.
        PRIMARY KEY (account, chat)
    );
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    /// (account, chat), in the order shown.
    pub chats: Vec<(BareJid, BareJid)>,
}

impl Store {
    pub fn folders(&self) -> Result<Vec<Folder>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT name FROM folders ORDER BY position, name")?;
        let names: Vec<String> = query.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut members =
            db.prepare("SELECT account, chat FROM folder_chats WHERE folder = ?1 ORDER BY position, chat")?;
        let mut folders = Vec::new();
        for name in names {
            let rows =
                members.query_map([&name], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut chats = Vec::new();
            for row in rows {
                let (account, chat) = row?;
                chats.push((parse_jid(&account)?, parse_jid(&chat)?));
            }
            folders.push(Folder { name, chats });
        }
        Ok(folders)
    }

    /// Replaces all folders with `folders`: they are few, and written whole they cannot
    /// half disagree with what is shown.
    pub fn save_folders(&self, folders: &[Folder]) -> Result<(), Error> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM folder_chats", [])?;
        tx.execute("DELETE FROM folders", [])?;
        for (position, folder) in folders.iter().enumerate() {
            tx.execute(
                "INSERT INTO folders (name, position) VALUES (?1, ?2)",
                params![folder.name, position as i64],
            )?;
            for (at, (account, chat)) in folder.chats.iter().enumerate() {
                tx.execute(
                    "INSERT OR REPLACE INTO folder_chats (folder, account, chat, position) VALUES (?1, ?2, ?3, ?4)",
                    params![folder.name, account.as_str(), chat.as_str(), at as i64],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(text: &str) -> BareJid {
        text.parse().unwrap()
    }

    #[test]
    fn folders_come_back_in_order_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        let (home, work) = (jid("me@home.org"), jid("me@work.org"));
        let folders = vec![
            Folder {
                name: "Работа".into(),
                chats: vec![
                    (work.clone(), jid("boss@work.org")),
                    (home.clone(), jid("team@muc.home.org")),
                ],
            },
            Folder {
                name: "Семья".into(),
                chats: vec![(home.clone(), jid("mom@home.org"))],
            },
            Folder {
                name: "Пустая".into(),
                chats: Vec::new(),
            },
        ];
        Store::open(&path).unwrap().save_folders(&folders).unwrap();
        assert_eq!(Store::open(&path).unwrap().folders().unwrap(), folders);

        let store = Store::open(&path).unwrap();
        store.save_folders(&folders[1..2]).unwrap();
        assert_eq!(
            store.folders().unwrap(),
            &folders[1..2],
            "saving replaces, removed ones go"
        );
    }
}
