//! Encrypted text that names no key for this device, against the test Prosody (`mise run
//! prosody`): a sender's client that does not know or trust this device. It must show up
//! as such, not vanish: otherwise someone wrote and nothing is seen.

use omemo_xml::xml;
use omtalk_testkit::{Device, fresh_device_lists, stop_all, text};
use xmpp_parsers::message::{Message, MessageType};

#[tokio::test(flavor = "multi_thread")]
async fn text_for_other_devices_is_reported_not_dropped() {
    fresh_device_lists(&["u16", "u17"]).await;
    let (mut alice, bob) = tokio::join!(Device::start("u16"), Device::start("u17"));

    // As a client would send it that knows only some other device of Alice's.
    let encrypted = xml::Encrypted {
        header: xml::Header {
            sid: bob.omemo.device_id(),
            keys: vec![xml::Key {
                rid: alice.omemo.device_id().wrapping_add(1),
                prekey: false,
                data: vec![0; 40],
            }],
            iv: xml::IV { data: vec![0; 12] },
        },
        payload: Some(xml::Payload {
            data: vec![1, 2, 3, 4],
        }),
    };
    let message =
        Message::new_with_type(MessageType::Chat, Some(alice.jid.clone().into())).with_payload(encrypted);
    bob.account.send(message).await.unwrap();
    let got = alice.next_where(|m| m.not_for_this_device).await;
    assert_eq!((got.text, &got.sender, got.encrypted), (None, &bob.jid, true));

    // A key exchange answer for another device (no text) is not worth a word.
    let empty = xml::Encrypted {
        header: xml::Header {
            sid: bob.omemo.device_id(),
            keys: vec![xml::Key {
                rid: alice.omemo.device_id().wrapping_add(1),
                prekey: false,
                data: vec![0; 40],
            }],
            iv: xml::IV { data: vec![0; 12] },
        },
        payload: None,
    };
    let message =
        Message::new_with_type(MessageType::Chat, Some(alice.jid.clone().into())).with_payload(empty);
    bob.account.send(message).await.unwrap();
    bob.omemo.send(&alice.jid, "после пустого").await.unwrap();
    let next = alice
        .next_where(|m| m.not_for_this_device || m.text.is_some())
        .await;
    assert_eq!(text(&next), "после пустого", "the empty one said nothing");

    stop_all([alice, bob]).await;
}
