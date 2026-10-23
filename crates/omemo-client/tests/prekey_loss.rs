//! If anything fails after a key exchange decrypted (here: saving the own keys once; in
//! life, more often the bundle publish or the empty answer on a dropped connection), the
//! session must not be lost while its pre key is already gone. Otherwise every later
//! message of that sender, which repeats the same key exchange until answered, fails
//! with "pre key N is not known". (From an outside review.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use omemo::{DeviceId, OwnKeys, Session};
use omtalk_omemo::{Ephemeral, Omemo, Persist, PersistError, State};
use omtalk_testkit::{Device, converge, fresh_device_lists, stop_all};
use omtalk_xmpp::{AccountConfig, Accounts, BareJid, Event, Server};

struct FlakyKeys(AtomicBool);

impl Persist for FlakyKeys {
    fn keys(&self, keys: &OwnKeys) -> Result<(), PersistError> {
        if self.0.swap(false, Ordering::SeqCst) {
            return Err(PersistError("disk full, once".into()));
        }
        Ephemeral.keys(keys)
    }
    fn session(&self, j: &BareJid, d: DeviceId, s: &Session) -> Result<(), PersistError> {
        Ephemeral.session(j, d, s)
    }
    fn identity(&self, j: &BareJid, d: DeviceId, k: &[u8; 32]) -> Result<(), PersistError> {
        Ephemeral.identity(j, d, k)
    }
    fn archive_position(&self, a: &BareJid, id: &str) -> Result<(), PersistError> {
        Ephemeral.archive_position(a, id)
    }
    fn forget_archive_position(&self, a: &BareJid) -> Result<(), PersistError> {
        Ephemeral.forget_archive_position(a)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn one_failure_after_a_key_exchange_does_not_break_the_sender_for_good() {
    fresh_device_lists(&["u64", "u65"]).await;
    let alice = Device::start("u64").await;
    converge(&[&alice]).await;

    // Bob, driven by hand to see every result, errors included.
    let mut config = AccountConfig::new("u65@localhost".parse().unwrap(), "test");
    config.server = Server::Plaintext {
        addr: "127.0.0.1:15222".into(),
    };
    config.iq_timeout = Duration::from_secs(5);
    let (mut accounts, mut events) = Accounts::new();
    let account = accounts.add(config).await;
    while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
    let flaky = Arc::new(FlakyKeys(AtomicBool::new(false)));
    let bob = Omemo::persisting(
        account.clone(),
        State::new(OwnKeys::generate(0).unwrap()),
        flaky.clone(),
    );
    bob.announce().await.unwrap();
    bob.catch_up().await.unwrap();

    let mut next = async || loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event")
            .unwrap();
        match bob.handle(&event.event).await {
            Ok(Some(m)) if m.text.is_some() => return Ok(m.text.unwrap()),
            Ok(_) => continue,
            Err(e) => return Err(e.to_string()),
        }
    };

    flaky.0.store(true, Ordering::SeqCst);
    let bob_jid: BareJid = "u65@localhost".parse().unwrap();
    alice.omemo.send(&bob_jid, "first").await.unwrap();
    let first = next().await;
    assert!(first.is_err(), "the injected failure shows: {first:?}");

    // Everything works again; alice has had no answer and repeats the key exchange.
    alice.omemo.send(&bob_jid, "second").await.unwrap();
    let second = next().await;
    account.stop().await;
    stop_all([alice]).await;
    assert_eq!(second, Ok("second".to_owned()));
}
