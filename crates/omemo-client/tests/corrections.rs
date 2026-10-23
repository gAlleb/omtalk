//! A correction (XEP-0308) goes beside the encrypted body, as replies do: the other side
//! learns which message it corrects, whatever the encryption.

use omtalk_testkit::{Device, fresh_device_lists, stop_all, text};

#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_correction_names_the_message_it_corrects() {
    fresh_device_lists(&["u79", "u80"]).await;
    let (alice, mut bob) = tokio::join!(Device::start("u79"), Device::start("u80"));
    alice
        .omemo
        .send_with(&bob.jid, "встреча в пятт", "c-1", Vec::new())
        .await
        .unwrap();
    let first = bob.next_text().await;
    assert_eq!(
        (text(&first), first.id.as_deref(), first.replaces.as_deref()),
        ("встреча в пятт", Some("c-1"), None)
    );

    let correction = omtalk_xmpp::corrections::element("c-1");
    alice
        .omemo
        .send_with(&bob.jid, "встреча в пять", "c-2", vec![correction])
        .await
        .unwrap();
    let fixed = bob.next_text().await;
    assert_eq!(
        (text(&fixed), fixed.replaces.as_deref()),
        ("встреча в пять", Some("c-1"))
    );
    stop_all([alice, bob]).await;
}
