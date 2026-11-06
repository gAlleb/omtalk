//! Per-participant read checkpoints, kept without changing the database schema.
//! An earlier checkpoint retains its time when a later one arrives. An unknown
//! target is pending until its message is loaded; it does not read arbitrary text.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use omtalk_store::Store;
use omtalk_xmpp::BareJid;
use serde::{Deserialize, Serialize};

use crate::app::Line;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reader {
    pub name: String,
    pub at: Option<i64>,
    pub received: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub reader: String,
    pub message: String,
    pub info: Reader,
}

#[derive(Clone, Debug, Default)]
pub struct Readers {
    loaded: bool,
    marks: BTreeMap<(String, String), Checkpoint>,
    positions: BTreeMap<String, (i64, i64)>,
    own: BTreeSet<String>,
}

fn prefix(account: &BareJid, room: &BareJid) -> String {
    format!(
        "room-readers/{}/",
        URL_SAFE_NO_PAD.encode(format!("{account}\0{room}"))
    )
}

/// The author's occupant ID is metadata about a message, not another reader.
/// Keep it separately so anonymous history remains attributable after a restart.
pub fn author_key(account: &BareJid, room: &BareJid, message: &str) -> String {
    format!(
        "room-message-author/{}/{}",
        URL_SAFE_NO_PAD.encode(format!("{account}\0{room}")),
        URL_SAFE_NO_PAD.encode(message)
    )
}

pub fn author_ids(message: &omtalk_omemo::Incoming) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if message.nick.is_some() {
        if message.sender != message.peer {
            ids.insert(format!("jid:{}", message.sender));
        }
        if let Some(id) = &message.occupant_id {
            ids.insert(format!("occupant:{id}"));
        }
    }
    ids
}

pub fn checkpoint(
    chat: &crate::app::Chat,
    mark: &omtalk_xmpp::room_reads::RoomRead,
    archived: bool,
    occupant_ids: bool,
    received: i64,
) -> Option<Checkpoint> {
    if !chat.is_room() || chat.jid != mark.room {
        return None;
    }
    // An archived nickname may have been reused by somebody else since then.
    let real = if archived {
        mark.real_jid.as_ref()
    } else {
        chat.occupants.get(&mark.nick).and_then(Option::as_ref)
    };
    let is_own = mark
        .occupant_id
        .as_ref()
        .is_some_and(|id| chat.readers.own.contains(id))
        || real == Some(&chat.account)
        || (!archived && matches!(&chat.kind, crate::app::ChatKind::Room { nick } if nick == &mark.nick));
    let reader = if is_own {
        format!("jid:{}", chat.account)
    } else {
        match real {
            Some(jid) => format!("jid:{jid}"),
            None if occupant_ids => format!("occupant:{}", mark.occupant_id.as_ref()?),
            None => return None,
        }
    };
    Some(Checkpoint {
        reader,
        message: mark.message.clone(),
        info: Reader {
            name: mark.nick.clone(),
            at: mark.at.filter(|at| *at <= received),
            received,
        },
    })
}

impl Readers {
    pub fn load(&mut self, store: &Store, account: &BareJid, room: &BareJid) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let prefix = prefix(account, room);
        for (key, value) in store.kept_under(&prefix).unwrap_or_default() {
            if key.starts_with("self/") {
                self.own.insert(value);
                continue;
            }
            if let Ok(mark) = toml::from_str::<Checkpoint>(&value) {
                self.marks
                    .insert((mark.reader.clone(), mark.message.clone()), mark);
            }
        }
    }

    /// Self presence identifies us even in anonymous rooms and in archived markers
    /// under an old nickname. Keep every ID the room has used for this account.
    pub fn remember_self(&mut self, store: &Store, account: &BareJid, room: &BareJid, id: String) -> bool {
        self.load(store, account, room);
        if !self.own.insert(id.clone()) {
            return false;
        }
        let key = format!("{}self/{}", prefix(account, room), URL_SAFE_NO_PAD.encode(&id));
        let _ = store.keep(&key, Some(&id));
        true
    }

    pub fn record(&mut self, store: &Store, account: &BareJid, room: &BareJid, mut mark: Checkpoint) {
        self.load(store, account, room);
        let id = (mark.reader.clone(), mark.message.clone());
        if let Some(old) = self.marks.get(&id) {
            mark.info.received = mark.info.received.min(old.info.received);
            mark.info.at = old.info.at.into_iter().chain(mark.info.at).min();
            if old.info.at == mark.info.at && old.info.received == mark.info.received {
                return;
            }
        }
        let key = format!(
            "{}{}",
            prefix(account, room),
            URL_SAFE_NO_PAD.encode(format!("{}\0{}", mark.reader, mark.message))
        );
        if let Ok(value) = toml::to_string(&mark) {
            let _ = store.keep(&key, Some(&value));
        }
        self.marks.insert(id, mark);
    }

    /// Whether any message's reader count changed.
    pub fn apply(&mut self, store: &Store, account: &BareJid, room: &BareJid, lines: &mut [Line]) -> bool {
        self.load(store, account, room);
        if self.marks.is_empty() {
            return false;
        }
        let missing: BTreeSet<_> = self
            .marks
            .values()
            .map(|m| &m.message)
            .chain(lines.iter().filter_map(|l| l.archive_id.as_ref()))
            .filter(|id| !self.positions.contains_key(*id))
            .cloned()
            .collect();
        if !missing.is_empty() {
            self.positions.extend(
                store
                    .room_message_positions(account, room, &missing.into_iter().collect::<Vec<_>>())
                    .unwrap_or_default(),
            );
        }
        let mut readers: BTreeMap<&str, BTreeMap<(i64, i64), &Reader>> = BTreeMap::new();
        let own_key = format!("jid:{account}");
        for mark in self.marks.values() {
            let reader = if mark
                .reader
                .strip_prefix("occupant:")
                .is_some_and(|id| self.own.contains(id))
            {
                own_key.as_str()
            } else {
                mark.reader.as_str()
            };
            if let Some(position) = self.positions.get(&mark.message) {
                readers
                    .entry(reader)
                    .or_default()
                    .entry(*position)
                    .and_modify(|info| {
                        if mark.info.at.unwrap_or(mark.info.received) < info.at.unwrap_or(info.received) {
                            *info = &mark.info;
                        }
                    })
                    .or_insert(&mark.info);
            }
        }
        let mut changed = false;
        for line in lines {
            let old_count = line.readers.len();
            line.readers.clear();
            if line.notice {
                changed |= old_count != 0;
                continue;
            }
            let Some(position) = line.archive_id.as_ref().and_then(|id| self.positions.get(id)) else {
                changed |= old_count != 0;
                continue;
            };
            let own_author = line.outgoing
                || line.author_ids.iter().any(|id| {
                    id == &own_key
                        || id
                            .strip_prefix("occupant:")
                            .is_some_and(|id| self.own.contains(id))
                });
            for (reader, points) in &readers {
                // The author is not a recipient. Self still counts when reading
                // somebody else's message, but never when reading one's own.
                if (own_author && *reader == own_key) || line.author_ids.contains(*reader) {
                    continue;
                }
                if let Some((_, info)) = points.range(*position..).next() {
                    line.readers.insert((*reader).to_owned(), (*info).clone());
                }
            }
            changed |= old_count != line.readers.len();
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Chat, ChatKind};
    use omtalk_store::StoredMessage;
    use omtalk_xmpp::room_reads::RoomRead;

    fn jid(s: &str) -> BareJid {
        s.parse().unwrap()
    }

    fn room() -> Chat {
        Chat::new(
            jid("me@example.org"),
            jid("room@muc.example.org"),
            ChatKind::Room { nick: "me".into() },
        )
    }

    fn save(store: &Store, chat: &Chat, id: &str, at: i64) -> Line {
        store
            .save_message(
                &chat.account,
                &StoredMessage {
                    chat: chat.jid.clone(),
                    sender: jid("writer@example.org"),
                    nick: Some("writer".into()),
                    outgoing: false,
                    encrypted: false,
                    body: "hello".into(),
                    at,
                    id: Some(format!("client-{id}")),
                    delivery: None,
                    shared_file: false,
                    reply_to: None,
                    archive_id: Some(id.into()),
                    untrusted: false,
                    edited: false,
                },
            )
            .unwrap();
        let mut line = Line::message(at, "writer".into(), "hello".into(), false, false);
        line.archive_id = Some(id.into());
        line
    }

    fn point(reader: &str, message: &str, at: Option<i64>, received: i64) -> Checkpoint {
        Checkpoint {
            reader: reader.into(),
            message: message.into(),
            info: Reader {
                name: reader.into(),
                at,
                received,
            },
        }
    }

    #[test]
    fn checkpoints_survive_restart_and_keep_each_readers_earlier_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        let store = Store::open(&path).unwrap();
        let chat = room();
        let mut lines = vec![
            save(&store, &chat, "z-first", 100),
            save(&store, &chat, "a-second", 100),
            save(&store, &chat, "m-third", 101),
        ];
        let mut readers = Readers::default();
        for mark in [
            point("anya", "z-first", Some(200), 200),
            point("anya", "m-third", Some(220), 220),
            point("bob", "a-second", None, 230),
        ] {
            readers.record(&store, &chat.account, &chat.jid, mark);
        }
        readers.apply(&store, &chat.account, &chat.jid, &mut lines);
        assert_eq!(
            lines.iter().map(|l| l.readers.len()).collect::<Vec<_>>(),
            [2, 2, 1]
        );
        assert_eq!(lines[0].readers["anya"].at, Some(200));
        assert_eq!(lines[1].readers["anya"].at, Some(220));
        assert_eq!(lines[1].readers["bob"].at, None);
        let expected: Vec<_> = lines.iter().map(|l| l.readers.clone()).collect();
        drop(readers);
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut readers = Readers::default();
        // Replayed and out-of-order markers neither move a reader backwards nor
        // invent a new read time each time the archive is opened.
        readers.record(
            &store,
            &chat.account,
            &chat.jid,
            point("anya", "z-first", Some(200), 900),
        );
        readers.record(
            &store,
            &chat.account,
            &chat.jid,
            point("bob", "a-second", None, 900),
        );
        readers.apply(&store, &chat.account, &chat.jid, &mut lines);
        assert_eq!(
            lines.iter().map(|l| l.readers.clone()).collect::<Vec<_>>(),
            expected
        );
        let mut other_account = Readers::default();
        let mut fresh = vec![save(&store, &chat, "m-third", 101)];
        other_account.apply(&store, &jid("other@example.org"), &chat.jid, &mut fresh);
        assert!(fresh[0].readers.is_empty());
    }

    #[test]
    fn unknown_targets_wait_for_history_and_later_reads_do_not_cover_newer_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let chat = room();
        let mut lines = vec![
            save(&store, &chat, "first", 100),
            save(&store, &chat, "newer", 110),
        ];
        let mut readers = Readers::default();
        readers.record(
            &store,
            &chat.account,
            &chat.jid,
            point("anya", "missing", Some(200), 200),
        );
        readers.apply(&store, &chat.account, &chat.jid, &mut lines);
        assert!(lines.iter().all(|l| l.readers.is_empty()));
        lines.insert(1, save(&store, &chat, "missing", 105));
        readers.apply(&store, &chat.account, &chat.jid, &mut lines);
        assert_eq!(
            lines.iter().map(|l| l.readers.len()).collect::<Vec<_>>(),
            [1, 1, 0]
        );
        lines.insert(0, save(&store, &chat, "older-page", 90));
        readers.apply(&store, &chat.account, &chat.jid, &mut lines);
        assert_eq!(lines[0].readers["anya"].at, Some(200));
    }

    #[test]
    fn identity_uses_presence_or_archived_ids_never_a_reused_nickname() {
        let mut chat = room();
        chat.occupants
            .insert("Anya".into(), Some(jid("anya@example.org")));
        let mut mark = RoomRead {
            room: chat.jid.clone(),
            nick: "Anya".into(),
            message: "id".into(),
            occupant_id: None,
            real_jid: Some(jid("forged@example.org")),
            at: None,
        };
        let first = checkpoint(&chat, &mark, false, false, 300).unwrap();
        assert_eq!(
            first.reader, "jid:anya@example.org",
            "live payload cannot override presence"
        );
        chat.occupants
            .insert("NewName".into(), Some(jid("anya@example.org")));
        mark.nick = "NewName".into();
        assert_eq!(
            checkpoint(&chat, &mark, false, false, 300).unwrap().reader,
            first.reader
        );
        chat.occupants
            .insert("Anya".into(), Some(jid("someone-else@example.org")));
        mark.nick = "Anya".into();
        mark.real_jid = Some(jid("anya@example.org"));
        assert_eq!(
            checkpoint(&chat, &mark, true, false, 300).unwrap().reader,
            first.reader
        );
        mark.real_jid = None;
        assert!(checkpoint(&chat, &mark, true, false, 300).is_none());
        mark.occupant_id = Some("room-stable-anya".into());
        assert!(checkpoint(&chat, &mark, true, false, 300).is_none());
        assert_eq!(
            checkpoint(&chat, &mark, true, true, 300).unwrap().reader,
            "occupant:room-stable-anya"
        );
        mark.real_jid = Some(chat.account.clone());
        assert_eq!(
            checkpoint(&chat, &mark, true, true, 300).unwrap().reader,
            "jid:me@example.org"
        );
        mark.nick = "me".into();
        assert_eq!(
            checkpoint(&chat, &mark, false, true, 300).unwrap().reader,
            "jid:me@example.org"
        );
        mark.room = jid("different@muc.example.org");
        assert!(checkpoint(&chat, &mark, true, true, 300).is_none());
    }

    #[test]
    fn own_reading_counts_once_on_incoming_messages_across_nick_changes_and_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("history.db")).unwrap();
        let mut chat = room();
        let line = save(&store, &chat, "target", 100);
        chat.lines.push(line);
        let mut own_line = save(&store, &chat, "own", 101);
        own_line.outgoing = true;
        own_line.delivery = Some(crate::app::Delivery::Sent);
        chat.lines.push(own_line);
        chat.readers.record(
            &store,
            &chat.account,
            &chat.jid,
            point("occupant:self-id", "own", Some(200), 200),
        );
        chat.readers
            .remember_self(&store, &chat.account, &chat.jid, "self-id".into());
        chat.readers = Readers::default();
        chat.readers
            .apply(&store, &chat.account, &chat.jid, &mut chat.lines);
        assert_eq!(chat.lines[0].readers.len(), 1);
        assert_eq!(chat.lines[0].readers["jid:me@example.org"].at, Some(200));
        assert!(
            chat.lines[1].readers.is_empty(),
            "self does not read own messages"
        );
        let own = RoomRead {
            room: chat.jid.clone(),
            nick: "old-own-nick".into(),
            message: "own".into(),
            occupant_id: Some("self-id".into()),
            real_jid: None,
            at: Some(200),
        };
        let echoed = checkpoint(&chat, &own, true, true, 300).unwrap();
        assert_eq!(echoed.reader, "jid:me@example.org");
        chat.readers.record(&store, &chat.account, &chat.jid, echoed);
        chat.readers.record(
            &store,
            &chat.account,
            &chat.jid,
            point("jid:me@example.org", "own", Some(250), 250),
        );
        chat.readers = Readers::default();
        chat.readers
            .apply(&store, &chat.account, &chat.jid, &mut chat.lines);
        assert_eq!(
            chat.lines[0].readers.len(),
            1,
            "local and echoed reads are one person"
        );
        assert_eq!(chat.lines[0].readers["jid:me@example.org"].at, Some(200));
        assert!(chat.lines[1].readers.is_empty());
        assert_eq!(chat.lines[1].delivery, Some(crate::app::Delivery::Sent));
        // Anonymous MAM may omit our real JID/direction, but the stored occupant
        // identity still identifies the author when self-presence is available.
        chat.lines[1].outgoing = false;
        chat.lines[1].author_ids.insert("occupant:self-id".into());
        chat.readers
            .apply(&store, &chat.account, &chat.jid, &mut chat.lines);
        assert!(chat.lines[1].readers.is_empty());
    }

    #[test]
    fn a_large_archive_keeps_distinct_readers_and_resolves_positions_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("large.db")).unwrap();
        let chat = room();
        let mut readers = Readers::default();
        let mut lines = Vec::new();
        for i in 0..1000 {
            let id = format!("opaque-{}", 1000 - i);
            let line = save(&store, &chat, &id, i / 2);
            if i >= 700 {
                lines.push(line);
            }
            for reader in ["anya", "bob", "carol"] {
                readers.record(
                    &store,
                    &chat.account,
                    &chat.jid,
                    point(reader, &id, Some(2000 + i), 3000),
                );
            }
        }
        let start = std::time::Instant::now();
        assert!(readers.apply(&store, &chat.account, &chat.jid, &mut lines));
        let cold = start.elapsed();
        assert_eq!(readers.positions.len(), 1000);
        assert!(lines.iter().all(|l| l.readers.len() == 3));
        assert_eq!(lines[0].readers["anya"].at, Some(2700));
        let start = std::time::Instant::now();
        assert!(!readers.apply(&store, &chat.account, &chat.jid, &mut lines));
        eprintln!(
            "3,000 checkpoints, 300 visible messages: cold {cold:?}, cached {:?}",
            start.elapsed()
        );
    }
}
