//! The user's own folders of chats, inside each account: omtalk's alone, not the roster's
//! groups, so they hold rooms too.

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

/// Schema 5: a folder belongs to one account, as its chats do; the same name may be a
/// folder in two accounts. Schema 4's folders, which could mix accounts, are split by
/// account; empty ones go.
pub(crate) const MIGRATION_PER_ACCOUNT: &str = "
    CREATE TABLE account_folders (
        account  TEXT NOT NULL,
        name     TEXT NOT NULL,
        position INTEGER NOT NULL,
        PRIMARY KEY (account, name)
    );
    INSERT INTO account_folders (account, name, position)
        SELECT DISTINCT c.account, f.name, f.position
        FROM folders f JOIN folder_chats c ON c.folder = f.name;
    DROP TABLE folders;
";

/// A folder of chats inside one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub account: BareJid,
    pub name: String,
    /// In the order shown.
    pub chats: Vec<BareJid>,
}

impl Store {
    pub fn folders(&self) -> Result<Vec<Folder>, Error> {
        let db = self.db.lock().unwrap();
        let mut query =
            db.prepare("SELECT account, name FROM account_folders ORDER BY position, account, name")?;
        let heads: Vec<(String, String)> = query
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut members = db.prepare(
            "SELECT chat FROM folder_chats WHERE account = ?1 AND folder = ?2 ORDER BY position, chat",
        )?;
        let mut folders = Vec::new();
        for (account, name) in heads {
            let chats: Vec<String> = members
                .query_map(params![account, name], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            folders.push(Folder {
                account: parse_jid(&account)?,
                name,
                chats: chats.iter().map(|c| parse_jid(c)).collect::<Result<_, _>>()?,
            });
        }
        Ok(folders)
    }

    /// Replaces all folders with `folders`: they are few, and written whole they cannot
    /// half disagree with what is shown.
    pub fn save_folders(&self, folders: &[Folder]) -> Result<(), Error> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM folder_chats", [])?;
        tx.execute("DELETE FROM account_folders", [])?;
        for (position, folder) in folders.iter().enumerate() {
            tx.execute(
                "INSERT INTO account_folders (account, name, position) VALUES (?1, ?2, ?3)",
                params![folder.account.as_str(), folder.name, position as i64],
            )?;
            for (at, chat) in folder.chats.iter().enumerate() {
                tx.execute(
                    "INSERT OR REPLACE INTO folder_chats (folder, account, chat, position) VALUES (?1, ?2, ?3, ?4)",
                    params![folder.name, folder.account.as_str(), chat.as_str(), at as i64],
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
                account: work.clone(),
                name: "Работа".into(),
                chats: vec![jid("boss@work.org")],
            },
            Folder {
                account: home.clone(),
                name: "Работа".into(),
                chats: vec![jid("team@muc.home.org"), jid("ann@home.org")],
            },
            Folder {
                account: home.clone(),
                name: "Семья".into(),
                chats: vec![jid("mom@home.org")],
            },
        ];
        Store::open(&path).unwrap().save_folders(&folders).unwrap();
        assert_eq!(Store::open(&path).unwrap().folders().unwrap(), folders);

        let store = Store::open(&path).unwrap();
        store.save_folders(&folders[2..]).unwrap();
        assert_eq!(
            store.folders().unwrap(),
            &folders[2..],
            "saving replaces, removed ones go"
        );
    }

    #[test]
    fn folders_that_mixed_accounts_are_split_by_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        // A database as schema 4 left it.
        let db = rusqlite::Connection::open(&path).unwrap();
        for migration in &crate::MIGRATIONS[..4] {
            db.execute_batch(migration).unwrap();
        }
        db.execute_batch(
            "PRAGMA user_version = 4;
             INSERT INTO folders VALUES ('Свои', 0), ('Пустая', 1);
             INSERT INTO folder_chats VALUES ('Свои', 'me@home.org', 'ann@home.org', 0),
                                             ('Свои', 'me@work.org', 'boss@work.org', 1);",
        )
        .unwrap();
        drop(db);
        let mut folders = Store::open(&path).unwrap().folders().unwrap();
        folders.sort_by(|a, b| a.account.cmp(&b.account));
        assert_eq!(
            folders,
            [
                Folder {
                    account: jid("me@home.org"),
                    name: "Свои".into(),
                    chats: vec![jid("ann@home.org")]
                },
                Folder {
                    account: jid("me@work.org"),
                    name: "Свои".into(),
                    chats: vec![jid("boss@work.org")]
                },
            ]
        );
    }
}
