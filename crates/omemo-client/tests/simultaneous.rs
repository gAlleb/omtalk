//! Sessions that go wrong are put right. Two devices that start a session with each
//! other at once, each before the other's key exchange arrives, must still read each
//! other afterwards: each takes the other's key exchange and builds a session from it,
//! and without the one it had built itself kept as well, the two sides end up in
//! different sessions and every later message fails. And a device that lost its session
//! starts a new one, so that only the message that found none is lost.

use omemo::Session;
use omemo_xml::{bundle, bundle_node, xml};
use omtalk_testkit::{Device, fresh_device_lists, stop_all, text};

/// The session `from` would build with `to` from `to`'s bundle, kept as `from`'s own.
async fn start_session(from: &Device, to: &Device) {
    let device = to.omemo.device_id();
    let items = from.account.items(&to.jid, &bundle_node(device)).await.unwrap();
    let parsed = bundle::from_xml(&xml::Bundle::try_from(items[0].payload.clone()).unwrap()).unwrap();
    let jid = to.jid.clone();
    from.omemo
        .with_state(|s| {
            let session = Session::initiate(&s.keys, &parsed).unwrap();
            s.sessions.insert((jid, device), session);
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_that_start_at_once_read_each_other() {
    fresh_device_lists(&["u75", "u76"]).await;
    let (mut alice, mut bob) = tokio::join!(Device::start("u75"), Device::start("u76"));
    start_session(&alice, &bob).await;
    start_session(&bob, &alice).await;

    // Both first messages carry a key exchange of their own.
    alice.omemo.send(&bob.jid, "a1").await.unwrap();
    bob.omemo.send(&alice.jid, "b1").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "a1");
    assert_eq!(text(&alice.next_text().await), "b1");

    // And after them, both ways, more than once.
    for round in 2..5 {
        alice.omemo.send(&bob.jid, &format!("a{round}")).await.unwrap();
        assert_eq!(text(&bob.next_text().await), format!("a{round}"));
        bob.omemo.send(&alice.jid, &format!("b{round}")).await.unwrap();
        assert_eq!(text(&alice.next_text().await), format!("b{round}"));
    }
    stop_all([alice, bob]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_lost_its_session_starts_a_new_one() {
    fresh_device_lists(&["u77", "u78"]).await;
    let (mut alice, mut bob) = tokio::join!(Device::start("u77"), Device::start("u78"));
    alice.omemo.send(&bob.jid, "привет").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "привет");
    bob.omemo.send(&alice.jid, "и тебе").await.unwrap();
    assert_eq!(text(&alice.next_text().await), "и тебе");

    // Bob's side of it is gone (a database restored from an old copy, say).
    let (alice_jid, alice_device) = (alice.jid.clone(), alice.omemo.device_id());
    bob.omemo
        .with_state(|s| {
            s.sessions.remove(&(alice_jid.clone(), alice_device));
            s.previous_sessions.remove(&(alice_jid, alice_device));
        })
        .await;

    alice.omemo.send(&bob.jid, "потеряется").await.unwrap();
    assert!(bob.next_result().await.is_err(), "this one found no session");
    // Bob's new key exchange reaches Alice first; from then on they read each other.
    alice
        .next_where(|m| m.text.is_none() && m.device == Some(bob.omemo.device_id()))
        .await;
    alice.omemo.send(&bob.jid, "снова").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "снова");
    bob.omemo.send(&alice.jid, "слышу").await.unwrap();
    assert_eq!(text(&alice.next_text().await), "слышу");
    stop_all([alice, bob]).await;
}
