//! A reply travels beside an encrypted message, against the test Prosody (`mise run
//! prosody`): the quote inside the encrypted body, what it answers outside, as Gajim,
//! Conversations and Monal send it.

use omtalk_testkit::{Device, converge, fresh_device_lists, stop_all, text};
use omtalk_xmpp::replies::{self, ReplyTo};

#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_reply_names_what_it_answers() {
    fresh_device_lists(&["u28", "u29"]).await;
    let (alice, mut bob) = tokio::join!(Device::start("u28"), Device::start("u29"));
    converge(&[&alice]).await;

    alice
        .omemo
        .send_with_id(&bob.jid, "как дела?", "q-1")
        .await
        .unwrap();
    let asked = bob.next_text().await;
    assert_eq!(
        (text(&asked), asked.id.as_deref(), asked.reply_to.as_deref()),
        ("как дела?", Some("q-1"), None)
    );

    let (body, length) = replies::body("как дела?", "отлично");
    let to = ReplyTo {
        id: "q-1".into(),
        to: None,
    };
    bob.omemo
        .send_with(&alice.jid, &body, "a-1", replies::elements(&to, length))
        .await
        .unwrap();
    let mut alice = alice;
    let answer = alice.next_text().await;
    assert_eq!(text(&answer), "> как дела?\nотлично");
    assert_eq!(answer.reply_to.as_deref(), Some("q-1"));

    stop_all([alice, bob]).await;
}
