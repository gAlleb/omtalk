//! A client quits and starts again on the same database, against the test Prosody
//! (`mise run prosody`). It must come back as the same device, with its sessions: a new
//! device each start would pile up in everyone's device lists, and lost sessions would
//! leave the peer's next messages unreadable.

use omemo_xml::{DEVICE_LIST_NODE, device_list, xml};
use omtalk_testkit::{Device, converge, text};

async fn devices_on_server(device: &Device) -> Vec<u32> {
    let items = device.account.items(&device.jid, DEVICE_LIST_NODE).await.unwrap();
    let list = xml::DeviceList::try_from(items[0].payload.clone()).unwrap();
    device_list::from_xml(&list).into_iter().collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn restarted_client_is_the_same_device_and_keeps_its_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("omtalk.db");

    let (alice, mut bob) = tokio::join!(Device::start_stored("u20", &db), Device::start("u21"));
    converge(&[&alice]).await;
    let alice_device = alice.omemo.device_id();
    let listed_before = devices_on_server(&alice).await;

    alice.omemo.send(&bob.jid, "до перезапуска").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "до перезапуска");
    alice.stop().await;

    // While Alice is away, Bob writes; the server keeps it for her.
    bob.omemo
        .send(&"u20@localhost".parse().unwrap(), "пока тебя не было")
        .await
        .unwrap();

    let mut alice = Device::start_stored("u20", &db).await;
    assert_eq!(
        alice.omemo.device_id(),
        alice_device,
        "same database, same device"
    );
    assert_eq!(
        devices_on_server(&alice).await,
        listed_before,
        "no new device announced"
    );

    // The stored session decrypts what Bob sent meanwhile, with no new key exchange.
    let got = alice.next_text().await;
    assert_eq!(
        (text(&got), got.device),
        ("пока тебя не было", Some(bob.omemo.device_id()))
    );

    alice.omemo.send(&bob.jid, "я вернулась").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "я вернулась");
    bob.omemo.send(&alice.jid, "с возвращением").await.unwrap();
    assert_eq!(text(&alice.next_text().await), "с возвращением");

    // Closed properly: a connection that just vanishes stays parked on the server
    // (XEP-0198), and the next run's offline message for this account would go to it.
    alice.stop().await;
    bob.stop().await;
}
