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
    }
}
