//! Messages sent without encryption, against the test Prosody (`mise run prosody`). They
//! are shown, marked as such, also in chats that use OMEMO: hiding them would hide that
//! someone wrote, and showing them as encrypted would lie.

use omtalk_testkit::{Device, converge, fresh_device_lists, sees, stop_all, text};
use omtalk_xmpp::{Affiliation, BareJid};
use xmpp_parsers::message::{Lang, Message};

#[tokio::test(flavor = "multi_thread")]
async fn plain_text_is_shown_and_marked_in_chats_and_rooms() {
    fresh_device_lists(&["u14", "u15"]).await;
    let (alice, mut bob) = tokio::join!(Device::start("u14"), Device::start("u15"));
    converge(&[&alice]).await;

    // An encrypted chat first, then a plain message in the same chat.
    alice.omemo.send(&bob.jid, "зашифровано").await.unwrap();
    let got = bob.next_text().await;
    assert_eq!((text(&got), got.encrypted), ("зашифровано", true));
    let plain =
        Message::chat(Some(bob.jid.clone().into())).with_body(Lang::default(), "открытым текстом".into());
    alice.account.send(plain).await.unwrap();
    let got = bob.next_text().await;
    assert_eq!(
        (text(&got), got.encrypted, got.device),
        ("открытым текстом", false, None)
    );
    assert_eq!(got.sender, alice.jid);

    // The same in a room, with the sender named by the room.
    let room: BareJid = format!("plain-{}@muc.localhost", alice.omemo.device_id())
        .parse()
        .unwrap();
    alice.account.join(&room, "alice").await.unwrap();
    alice.account.make_private(&room).await.unwrap();
    alice
        .account
        .set_affiliation(&room, &bob.jid, Affiliation::Member)
        .await
        .unwrap();
    bob.account.join(&room, "bob").await.unwrap();
    sees(&bob, &room, &["alice", "bob"]).await;
    let plain =
        Message::groupchat(Some(room.clone().into())).with_body(Lang::default(), "в комнату открыто".into());
    alice.account.send(plain).await.unwrap();
    let got = bob.next_text().await;
    assert_eq!(
        (text(&got), got.encrypted, got.nick.as_deref()),
        ("в комнату открыто", false, Some("alice"))
    );
    assert_eq!((&got.sender, &got.peer), (&alice.jid, &room));

    stop_all([alice, bob]).await;
}
