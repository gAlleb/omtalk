//! Several devices per account, against the test Prosody (`mise run prosody`).
//!
//! The case that matters: one person with three clients on one account (a phone and two
//! laptops) talking to someone with two. Every device must read every message, the own
//! sent ones included.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use omemo::{DeviceId, OwnKeys};
use omtalk_omemo::{Direction, Incoming, Omemo, State};
use omtalk_xmpp::{Account, AccountConfig, Accounts, BareJid, Event, Server};
use tokio::sync::mpsc;
use tokio::time::timeout;
use xmpp_parsers::presence::Presence;

const WAIT: Duration = Duration::from_secs(20);

struct Device {
    account: Account,
    omemo: Arc<Omemo>,
    jid: BareJid,
    inbox: mpsc::UnboundedReceiver<Result<Incoming, String>>,
}

impl Device {
    /// One client: its own connection and keys, driven by its own event loop.
    async fn start(user: &str) -> Self {
        let jid: BareJid = format!("{user}@localhost").parse().unwrap();
        let mut config = AccountConfig::new(jid.clone(), "test");
        config.server = Server::Plaintext {
            addr: "127.0.0.1:15222".into(),
        };
        config.iq_timeout = Duration::from_secs(5);

        let (mut accounts, mut events) = Accounts::new();
        let account = accounts.add(config).await;
        timeout(WAIT, async {
            while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
        })
        .await
        .expect("online; is the test server up (`mise run prosody`)?");

        let omemo = Arc::new(Omemo::new(
            account.clone(),
            State::new(OwnKeys::generate(0).unwrap()),
        ));
        omemo.announce().await.unwrap();

        let (tx, inbox) = mpsc::unbounded_channel();
        let driver = omemo.clone();
        tokio::spawn(async move {
            let _accounts = accounts;
            while let Some(event) = events.recv().await {
                match &event.event {
                    Event::Online { resumed: false, .. } => driver.announce().await.unwrap(),
                    // Shown only when a test fails, where a broken connection explains a lot.
                    Event::Disconnected { reason } => {
                        eprintln!("{} disconnected: {reason}", driver.device_id())
                    }
                    _ => {}
                }
                match driver.handle(&event.event).await {
                    Ok(Some(incoming)) => drop(tx.send(Ok(incoming))),
                    Ok(None) => {}
                    Err(error) => drop(tx.send(Err(error.to_string()))),
                }
            }
        });
        Self {
            account,
            omemo,
            jid,
            inbox,
        }
    }

    async fn own_list(&self) -> BTreeSet<DeviceId> {
        self.omemo
            .with_state(|s| s.device_lists.get(&self.jid).cloned().unwrap_or_default())
            .await
    }

    /// The next message with text; empty handshake answers are skipped.
    async fn next_text(&mut self) -> Incoming {
        timeout(WAIT, async {
            loop {
                let incoming = self
                    .inbox
                    .recv()
                    .await
                    .unwrap()
                    .expect("decryption must not fail");
                if incoming.text.is_some() {
                    return incoming;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("device {} of {} got no message", self.omemo.device_id(), self.jid))
    }
}

/// Waits until the own device list on the server and in every device's cache names all
/// devices, and stays so for a moment. Devices that announce at the same time overwrite
/// each other's lists and react to stale notifications; this is where they must settle.
async fn converge(devices: &[&Device]) {
    let ids: BTreeSet<DeviceId> = devices.iter().map(|d| d.omemo.device_id()).collect();
    let owner = devices[0].jid.clone();
    let settled = |seen: Vec<BTreeSet<DeviceId>>| seen.iter().all(|list| list.is_superset(&ids));
    timeout(WAIT, async {
        let mut stable_since = None;
        loop {
            let mut seen = Vec::new();
            for device in devices {
                seen.push(device.own_list().await);
            }
            let on_server = devices[0]
                .account
                .items(&owner, omemo_xml::DEVICE_LIST_NODE)
                .await
                .ok()
                .and_then(|items| omemo_xml::xml::DeviceList::try_from(items.first()?.payload.clone()).ok())
                .map(|list| omemo_xml::device_list::from_xml(&list))
                .unwrap_or_default();
            seen.push(on_server);
            if settled(seen) {
                let since = *stable_since.get_or_insert_with(tokio::time::Instant::now);
                if since.elapsed() > Duration::from_millis(500) {
                    return;
                }
            } else {
                stable_since = None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("own device lists did not converge");
}

fn text(incoming: &Incoming) -> &str {
    incoming.text.as_deref().unwrap()
}

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

/// Mutual presence subscription, as between two roster contacts.
async fn befriend(a: &Device, b: &Device) {
    for (from, to) in [(a, b), (b, a)] {
        from.account
            .send(Presence::subscribed().with_to(to.jid.clone()))
            .await
            .unwrap();
        from.account
            .send(Presence::subscribe().with_to(to.jid.clone()))
            .await
            .unwrap();
    }
}

async fn unfriend(a: &Device, b: &Device) {
    use xmpp_parsers::roster::{Item, Roster, Subscription};
    for (from, to) in [(a, b), (b, a)] {
        let remove = Roster {
            ver: None,
            items: vec![Item {
                jid: to.jid.clone(),
                name: None,
                subscription: Subscription::Remove,
                ask: Default::default(),
                approved: None,
                groups: Vec::new(),
            }],
        };
        // Not being in the roster is fine too: the server says item-not-found.
        let _ = from.account.set(None, remove).await;
    }
}
