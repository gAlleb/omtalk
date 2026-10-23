//! A long absence from a room is read page by page, each page handed over as soon as it
//! is read: what was read stays read if a later step fails, since the archive position
//! has moved past it. Before, a failure on the way threw away all that was read.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use omemo::{DeviceId, OwnKeys, Session};
use omtalk_omemo::{Ephemeral, Persist, PersistError, State};
use omtalk_testkit::{Device, stop_all};
use omtalk_xmpp::{Affiliation, BareJid};
use xmpp_parsers::message::{Lang, Message};

/// Keeps nothing, and fails to keep an archive position once, at the `fail_at`-th.
struct FailsOnce {
    calls: AtomicUsize,
    fail_at: AtomicUsize,
}

impl Persist for FailsOnce {
    fn keys(&self, keys: &OwnKeys) -> Result<(), PersistError> {
        Ephemeral.keys(keys)
    }
    fn session(&self, j: &BareJid, d: DeviceId, s: &Session) -> Result<(), PersistError> {
        Ephemeral.session(j, d, s)
    }
    fn identity(&self, j: &BareJid, d: DeviceId, k: &[u8; 32]) -> Result<(), PersistError> {
        Ephemeral.identity(j, d, k)
    }
    fn archive_position(&self, a: &BareJid, id: &str) -> Result<(), PersistError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call == self.fail_at.load(Ordering::SeqCst) {
            return Err(PersistError("disk full, once".into()));
        }
        Ephemeral.archive_position(a, id)
    }
    fn forget_archive_position(&self, a: &BareJid) -> Result<(), PersistError> {
        Ephemeral.forget_archive_position(a)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn what_was_read_of_a_room_stays_read_when_a_later_page_fails() {
    let persist = Arc::new(FailsOnce {
        calls: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(usize::MAX),
    });
    let (writer, reader) = tokio::join!(
        Device::start("u55"),
        Device::start_with("u56", State::new(OwnKeys::generate(0).unwrap()), persist.clone())
    );
    let room: BareJid = format!("pages-{}@muc.localhost", writer.omemo.device_id())
        .parse()
        .unwrap();
    writer.account.join(&room, "writer").await.unwrap();
    writer.account.make_private(&room).await.unwrap();
    writer
        .account
        .set_affiliation(&room, &reader.jid, Affiliation::Member)
        .await
        .unwrap();
    // Once in, so the reader has a position in the room's archive; then away.
    reader.account.join(&room, "reader").await.unwrap();
    reader.omemo.catch_up_room(&room).await.unwrap();
    reader.account.leave(&room, "reader").await.unwrap();
    // Gone from the room before anything is said there.
    tokio::time::sleep(Duration::from_secs(1)).await;

    for i in 0..120 {
        let message =
            Message::groupchat(Some(room.clone().into())).with_body(Lang::default(), format!("m{i:03}"));
        writer.account.send(message).await.unwrap();
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Back; keeping the position fails on the 60th message of the absence.
    let calls = persist.calls.load(Ordering::SeqCst);
    persist.fail_at.store(calls + 60, Ordering::SeqCst);
    reader.account.join(&room, "reader").await.unwrap();
    let mut seen = Vec::new();
    let first = reader
        .omemo
        .catch_up_room_each(&room, |m| seen.extend(m.text))
        .await;
    assert!(
        first.is_err(),
        "the failure shows: {} read, {} positions kept",
        seen.len(),
        persist.calls.load(Ordering::SeqCst) - calls
    );
    assert!(
        seen.len() >= 59,
        "what was read before it is there: {}",
        seen.len()
    );

    // Read again: on from where it was kept, nothing missing.
    let again = reader
        .omemo
        .catch_up_room_each(&room, |m| seen.extend(m.text))
        .await;
    assert!(again.is_ok(), "{again:?}");
    let mut wanted: Vec<String> = (0..120).map(|i| format!("m{i:03}")).collect();
    seen.retain(|t| t.starts_with('m') && t.len() == 4);
    seen.dedup();
    wanted.retain(|w| !seen.contains(w));
    assert!(wanted.is_empty(), "lost: {wanted:?}");
    stop_all([writer, reader]).await;
}
