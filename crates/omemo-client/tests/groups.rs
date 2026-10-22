//! OMEMO in a group chat, against the test Prosody (`mise run prosody`).
//!
//! Three people, five devices, one members-only room. A member who is not in the room
//! right now must still get the keys: they are encrypted for members, not for occupants.

use std::time::Duration;

use omtalk_omemo::Direction;
use omtalk_testkit::{Device, WAIT, converge, fresh_device_lists, stop_all, text};
use omtalk_xmpp::{Affiliation, BareJid};
use tokio::time::timeout;

/// Waits until `device` sees every one of `nicks` in the room with a real JID.
async fn sees(device: &Device, room: &BareJid, nicks: &[&str]) {
    timeout(WAIT, async {
        loop {
            let known = device.omemo.occupants(room).await;
            if nicks.iter().all(|n| known.contains_key(*n)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{} does not see {nicks:?} in {room}", device.jid));
}

#[tokio::test(flavor = "multi_thread")]
async fn members_on_all_their_devices_read_group_messages() {
    fresh_device_lists(&["u10", "u11", "u12", "u13"]).await;
    let (mut alice_phone, mut alice_laptop, mut bob, mut carol_phone, mut carol_laptop, dave) = tokio::join!(
        Device::start("u10"),
        Device::start("u10"),
        Device::start("u11"),
        Device::start("u12"),
        Device::start("u12"),
        Device::start("u13"),
    );
    converge(&[&alice_phone, &alice_laptop]).await;
    converge(&[&carol_phone, &carol_laptop]).await;

    // Alice creates the room and makes it OMEMO-ready; Bob, Carol and Dave become members.
    let room: BareJid = format!("omemo-{}@muc.localhost", alice_phone.omemo.device_id())
        .parse()
        .unwrap();
    alice_phone.account.join(&room, "alice").await.unwrap();
    alice_phone.account.make_private(&room).await.unwrap();
    for member in [&bob, &carol_phone, &dave] {
        alice_phone
            .account
            .set_affiliation(&room, &member.jid, Affiliation::Member)
            .await
            .unwrap();
    }

    // Everybody but Dave joins; several devices of one account share its nick.
    alice_laptop.account.join(&room, "alice").await.unwrap();
    bob.account.join(&room, "bob").await.unwrap();
    carol_phone.account.join(&room, "carol").await.unwrap();
    carol_laptop.account.join(&room, "carol").await.unwrap();
    for device in [&alice_phone, &alice_laptop, &bob, &carol_phone, &carol_laptop] {
        sees(device, &room, &["alice", "bob", "carol"]).await;
    }

    let report = alice_phone.omemo.send_group(&room, "всем привет").await.unwrap();
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    let dave_device = (dave.jid.clone(), dave.omemo.device_id());
    assert!(
        report.devices.contains(&dave_device),
        "a member away from the room must get the keys too"
    );
    for device in [&mut alice_laptop, &mut bob, &mut carol_phone, &mut carol_laptop] {
        let got = device.next_text().await;
        assert_eq!(
            (text(&got), &got.peer, &got.sender),
            ("всем привет", &room, &alice_phone.jid)
        );
        // Alice's laptop sees her phone's message as her own.
        let expected = if device.jid == alice_phone.jid {
            Direction::Outgoing
        } else {
            Direction::Incoming
        };
        assert_eq!(got.direction, expected);
    }

    // Carol answers from her laptop: her phone and everybody else read it.
    carol_laptop
        .omemo
        .send_group(&room, "привет от Кэрол")
        .await
        .unwrap();
    for device in [&mut alice_phone, &mut alice_laptop, &mut bob, &mut carol_phone] {
        let got = device.next_text().await;
        assert_eq!((text(&got), &got.sender), ("привет от Кэрол", &carol_laptop.jid));
    }

    // A few rounds from everyone keep every pairwise session in step.
    let mut devices = [alice_phone, alice_laptop, bob, carol_phone, carol_laptop];
    for round in 0..3 {
        for sender in 0..devices.len() {
            let line = format!("раунд {round} от {sender}");
            devices[sender].omemo.send_group(&room, &line).await.unwrap();
            for (index, device) in devices.iter_mut().enumerate() {
                if index != sender {
                    assert_eq!(
                        text(&device.next_text().await),
                        line,
                        "device {index} in round {round}"
                    );
                }
            }
        }
    }
    stop_all(devices.into_iter().chain([dave])).await;
}
