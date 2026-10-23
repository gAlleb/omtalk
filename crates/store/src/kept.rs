//! Small things omtalk keeps between runs, by name: one's own status, say.

use rusqlite::{OptionalExtension, params};

use crate::{Error, Store};

/// Schema 16; listed in `MIGRATIONS`.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE kept (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
";

impl Store {
    /// What is kept under `key`, if anything.
    pub fn kept(&self, key: &str) -> Result<Option<String>, Error> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM kept WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    /// Everything kept under a key that starts with `prefix`, the keys without it.
    pub fn kept_under(&self, prefix: &str) -> Result<Vec<(String, String)>, Error> {
        let db = self.db.lock().unwrap();
        let mut query = db.prepare("SELECT key, value FROM kept WHERE substr(key, 1, length(?1)) = ?1")?;
        let rows = query.query_map([prefix], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut found = Vec::new();
        for row in rows {
            let (key, value) = row?;
            found.push((key[prefix.len()..].to_owned(), value));
        }
        Ok(found)
    }

    /// Keeps `value` under `key`; none, and nothing is kept there.
    pub fn keep(&self, key: &str, value: Option<&str>) -> Result<(), Error> {
        let db = self.db.lock().unwrap();
        match value {
            Some(value) => db.execute(
                "INSERT OR REPLACE INTO kept (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?,
            None => db.execute("DELETE FROM kept WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_kept_across_a_restart_and_goes_when_told() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("omtalk.db");
        Store::open(&path)
            .unwrap()
            .keep("status", Some("away\nобед"))
            .unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(store.kept("status").unwrap().as_deref(), Some("away\nобед"));
        store.keep("status", None).unwrap();
        assert_eq!(store.kept("status").unwrap(), None);
        store.keep("omemo/a/b", Some("0")).unwrap();
        store.keep("omemo/a/c", Some("1")).unwrap();
        store.keep("other", Some("x")).unwrap();
        let mut under = store.kept_under("omemo/").unwrap();
        under.sort();
        assert_eq!(
            under,
            [
                ("a/b".to_owned(), "0".to_owned()),
                ("a/c".to_owned(), "1".to_owned())
            ]
        );
    }
}
