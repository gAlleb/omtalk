//! One account on a phone that stays online and a laptop that is closed for a while,
//! against the test Prosody (`mise run prosody`). The server hands everything meanwhile
//! to the phone; the laptop must find it in the archive when it starts again: what the
//! peer wrote, and what the phone wrote back.

use std::time::Duration;

use omtalk_omemo::Direction;
use omtalk_testkit::{Device, converge, fresh_device_lists, stop_all, strangers, text};

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
