//! One account on a phone that stays online and a laptop that is closed for a while,
//! against the test Prosody (`mise run prosody`). The server hands everything meanwhile
//! to the phone; the laptop must find it in the archive when it starts again: what the
//! peer wrote, and what the phone wrote back.

use std::time::Duration;

use omtalk_omemo::Direction;
use omtalk_testkit::{Device, converge, fresh_device_lists, sees, stop_all, strangers, text};
use omtalk_xmpp::{Affiliation, BareJid};

#[tokio::test(flavor = "multi_thread")]
async fn closed_laptop_reads_what_the_online_phone_got_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("laptop.db");
    strangers("u22", "u23").await;
    fresh_device_lists(&["u22", "u23"]).await;

    let (laptop, mut phone, mut bob) = tokio::join!(
        Device::start_stored("u22", &db),
        Device::start("u22"),
        Device::start("u23"),
    );
    converge(&[&laptop, &phone]).await;

    bob.omemo.send(&phone.jid, "пока все онлайн").await.unwrap();
    assert_eq!(text(&phone.next_text().await), "пока все онлайн");
    let mut laptop = laptop;
    assert_eq!(text(&laptop.next_text().await), "пока все онлайн");
    laptop.stop().await;

    // The laptop is closed. Bob writes; only the phone is there to get it, and answers.
    bob.omemo.send(&phone.jid, "ноутбук закрыт").await.unwrap();
    assert_eq!(text(&phone.next_text().await), "ноутбук закрыт");
    phone.omemo.send(&bob.jid, "отвечаю с телефона").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "отвечаю с телефона");

    // The laptop starts again on its database and catches up.
    let mut laptop = Device::start_stored("u22", &db).await;
    let missed = laptop.next_text().await;
    assert_eq!(
        (text(&missed), missed.direction, &missed.sender),
        ("ноутбук закрыт", Direction::Incoming, &bob.jid)
    );
    let answered = laptop.next_text().await;
    assert_eq!(
        (text(&answered), answered.direction, &answered.peer),
        ("отвечаю с телефона", Direction::Outgoing, &bob.jid)
    );
    // Nothing older comes again: the archive is read from where the laptop stopped.
    assert!(
        laptop.next_text_within(Duration::from_secs(2)).await.is_none(),
        "the message from before the laptop closed was replayed"
    );

    // Live messages go on as before.
    bob.omemo.send(&phone.jid, "снова все онлайн").await.unwrap();
    assert_eq!(text(&laptop.next_text().await), "снова все онлайн");
    assert_eq!(text(&phone.next_text().await), "снова все онлайн");

    stop_all([laptop, phone, bob]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closed_laptop_reads_what_the_room_said_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("laptop.db");
    fresh_device_lists(&["u24", "u25"]).await;
    let (laptop, phone, bob) = tokio::join!(
        Device::start_stored("u24", &db),
        Device::start("u24"),
        Device::start("u25"),
    );
    let (mut laptop, mut phone, mut bob) = (laptop, phone, bob);
    converge(&[&laptop, &phone]).await;

    // Bob's members-only room; Alice is a member and is in it on both devices.
    let room: BareJid = format!("missed-{}@muc.localhost", bob.omemo.device_id())
        .parse()
        .unwrap();
    bob.account.join(&room, "bob").await.unwrap();
    bob.account.make_private(&room).await.unwrap();
    bob.account
        .set_affiliation(&room, &phone.jid, Affiliation::Member)
        .await
        .unwrap();
    for alice in [&laptop, &phone] {
        alice.account.join(&room, "alice").await.unwrap();
    }
    for device in [&laptop, &phone, &bob] {
        sees(device, &room, &["alice", "bob"]).await;
    }
    // The first time in a room only marks where its archive stands.
    assert!(
        laptop
            .omemo
            .catch_up_room(&room)
            .await
            .unwrap()
            .messages
            .is_empty()
    );

    bob.omemo.send_group(&room, "все в комнате").await.unwrap();
    assert_eq!(text(&laptop.next_text().await), "все в комнате");
    assert_eq!(text(&phone.next_text().await), "все в комнате");
    laptop.stop().await;

    bob.omemo.send_group(&room, "ноутбук закрыт").await.unwrap();
    assert_eq!(text(&phone.next_text().await), "ноутбук закрыт");
    phone.omemo.send_group(&room, "отвечаю с телефона").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "отвечаю с телефона");

    // Back on its database, the laptop joins the room again and reads its archive. Bob
    // and the phone are named by the archive itself, whoever is in the room now.
    let laptop = Device::start_stored("u24", &db).await;
    laptop.account.join(&room, "alice").await.unwrap();
    sees(&laptop, &room, &["alice", "bob"]).await;
    let caught = laptop.omemo.catch_up_room(&room).await.unwrap();
    assert!(caught.failed.is_empty(), "{:?}", caught.failed);
    let got: Vec<_> = caught
        .messages
        .iter()
        .map(|m| (text(m).to_owned(), m.direction, m.sender.clone(), m.peer.clone()))
        .collect();
    assert_eq!(
        got,
        [
            (
                "ноутбук закрыт".to_owned(),
                Direction::Incoming,
                bob.jid.clone(),
                room.clone()
            ),
            (
                "отвечаю с телефона".to_owned(),
                Direction::Outgoing,
                phone.jid.clone(),
                room.clone()
            ),
        ],
        "both, in order, and nothing from before the laptop closed"
    );

    stop_all([laptop, phone, bob]).await;
}
