//! Several devices per account, against the test Prosody (`mise run prosody`).
//!
//! The case that matters: one person with three clients on one account (a phone and two
//! laptops) talking to someone with two. Every device must read every message, the own
//! sent ones included.

mod common;

use std::time::Duration;

use common::{Device, WAIT, befriend, converge, text, unfriend};
use omtalk_omemo::Direction;
use tokio::time::timeout;

#[tokio::test(flavor = "multi_thread")]
async fn three_own_devices_and_two_peer_devices_all_read_everything() {
    // Started concurrently, as clients coming online together would be.
    let (mut phone, mut laptop, mut desk, mut bob1, mut bob2) = tokio::join!(
        Device::start("u01"),
        Device::start("u01"),
        Device::start("u01"),
        Device::start("u02"),
        Device::start("u02"),
    );
    converge(&[&phone, &laptop, &desk]).await;
    converge(&[&bob1, &bob2]).await;

    // The phone writes: both of Bob's devices read it, the two laptops see it as sent.
    let report = phone.omemo.send(&bob1.jid, "с телефона").await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    // Lists may also name dead devices of earlier runs, as real lists do.
    for device in [&bob1, &bob2, &laptop, &desk] {
        let target = (device.jid.clone(), device.omemo.device_id());
        assert!(report.devices.contains(&target), "{target:?} is not a recipient");
    }
    for bob in [&mut bob1, &mut bob2] {
        let got = bob.next_text().await;
        assert_eq!(
            (text(&got), got.direction, &got.sender),
            ("с телефона", Direction::Incoming, &phone.jid)
        );
    }
    for own in [&mut laptop, &mut desk] {
        let got = own.next_text().await;
        assert_eq!(
            (text(&got), got.direction, &got.peer),
            ("с телефона", Direction::Outgoing, &bob1.jid)
        );
    }

    // Bob answers from his second device: all three of ours read it, his first sees it as sent.
    bob2.omemo.send(&phone.jid, "ответ").await.unwrap();
    for own in [&mut phone, &mut laptop, &mut desk] {
        let got = own.next_text().await;
        assert_eq!(
            (text(&got), got.direction, got.device),
            ("ответ", Direction::Incoming, bob2.omemo.device_id())
        );
    }
    assert_eq!(text(&bob1.next_text().await), "ответ");

    // A laptop writes next, over sessions that are passive on some sides now.
    laptop.omemo.send(&bob1.jid, "с ноутбука").await.unwrap();
    for device in [&mut bob1, &mut bob2, &mut phone, &mut desk] {
        assert_eq!(text(&device.next_text().await), "с ноутбука");
    }

    // A long exchange in both directions keeps every ratchet in step.
    for round in 0..5 {
        let from_us = format!("мы {round}");
        desk.omemo.send(&bob1.jid, &from_us).await.unwrap();
        for device in [&mut bob1, &mut bob2, &mut phone, &mut laptop] {
            assert_eq!(text(&device.next_text().await), from_us);
        }
        let from_bob = format!("боб {round}");
        bob1.omemo.send(&desk.jid, &from_bob).await.unwrap();
        for device in [&mut phone, &mut laptop, &mut desk, &mut bob2] {
            assert_eq!(text(&device.next_text().await), from_bob);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_added_later_is_included_in_the_next_message() {
    let (alice, mut bob) = tokio::join!(Device::start("u03"), Device::start("u04"));
    // The server keeps rosters until it restarts; start as strangers.
    unfriend(&alice, &bob).await;
    converge(&[&alice]).await;
    alice.omemo.send(&bob.jid, "before").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "before");

    // Bob opens a second client. As contacts, Alice is pushed his new device list.
    befriend(&alice, &bob).await;
    let mut bob_new = Device::start("u04").await;
    converge(&[&bob, &bob_new]).await;
    let new_id = bob_new.omemo.device_id();
    timeout(WAIT, async {
        while !alice
            .omemo
            .with_state(|s| s.device_lists.get(&bob.jid).is_some_and(|l| l.contains(&new_id)))
            .await
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Alice was not told about Bob's new device");

    let report = alice.omemo.send(&bob.jid, "after").await.unwrap();
    assert!(report.devices.contains(&(bob.jid.clone(), new_id)));
    assert_eq!(text(&bob.next_text().await), "after");
    assert_eq!(text(&bob_new.next_text().await), "after");
}
