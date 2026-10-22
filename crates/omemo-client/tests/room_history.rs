//! The first entry into a room shows what was said there before, as Gajim does, against
//! the test Prosody (`mise run prosody`). A device that is new reads the plain text; what
//! was encrypted before it existed holds no key for it and is counted, not hidden.

use omtalk_testkit::{Device, converge, fresh_device_lists, sees, stop_all, text};
use omtalk_xmpp::{Affiliation, BareJid};
use xmpp_parsers::message::{Lang, Message};

#[tokio::test(flavor = "multi_thread")]
async fn a_new_device_entering_a_room_sees_its_past() {
    fresh_device_lists(&["u36", "u40"]).await;
    let (mut phone, bob) = tokio::join!(Device::start("u40"), Device::start("u36"));
    converge(&[&phone]).await;

    let room: BareJid = format!("past-{}@muc.localhost", bob.omemo.device_id())
        .parse()
        .unwrap();
    bob.account.join(&room, "bob").await.unwrap();
    bob.account.make_private(&room).await.unwrap();
    bob.account
        .set_affiliation(&room, &phone.jid, Affiliation::Member)
        .await
        .unwrap();
    phone.account.join(&room, "alice").await.unwrap();
    sees(&bob, &room, &["alice", "bob"]).await;

    // Said before the laptop exists: once encrypted (for the phone), once in plain text.
    bob.omemo.send_group(&room, "секрет до ноутбука").await.unwrap();
    assert_eq!(text(&phone.next_text().await), "секрет до ноутбука");
    let plain = Message::groupchat(Some(room.clone().into()))
        .with_body(Lang::default(), "открыто до ноутбука".into());
    bob.account.send(plain).await.unwrap();
    assert_eq!(text(&phone.next_text().await), "открыто до ноутбука");

    let laptop = Device::start("u40").await;
    laptop.account.join(&room, "alice-laptop").await.unwrap();
    sees(&laptop, &room, &["alice", "alice-laptop", "bob"]).await;
    let caught = laptop.omemo.catch_up_room(&room).await.unwrap();
    assert!(caught.failed.is_empty(), "{:?}", caught.failed);
    let got: Vec<_> = caught
        .messages
        .iter()
        .map(|m| (text(m).to_owned(), m.encrypted))
        .collect();
    assert_eq!(got, [("открыто до ноутбука".to_owned(), false)]);
    let at = caught.messages[0]
        .at
        .expect("an archived message has the server's time");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now - at).abs() < 120, "{at} is not about now ({now})");
    assert_eq!(caught.unreadable, 1, "the encrypted one is counted");

    // The room's past is read once; next time only what is new.
    let again = laptop.omemo.catch_up_room(&room).await.unwrap();
    assert!(again.messages.is_empty() && again.unreadable == 0, "{again:?}");

    stop_all([laptop, phone, bob]).await;
}
