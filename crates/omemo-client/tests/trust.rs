//! Deciding about devices, as in Gajim, against the test Prosody (`mise run prosody`):
//! blind trust until one device is verified, a new device then waits for a decision, a
//! refused one gets nothing, and an old device of one's own leaves the list.

use omtalk_omemo::{Error, Trust};
use omtalk_testkit::{Device, befriend, converge, fresh_device_lists, stop_all, text};

#[tokio::test(flavor = "multi_thread")]
async fn devices_are_trusted_blindly_until_one_is_verified() {
    fresh_device_lists(&["u26", "u27"]).await;
    let (alice, mut phone) = tokio::join!(Device::start("u26"), Device::start("u27"));
    // Contacts, so that Alice hears of Bob's new devices as they come.
    befriend(&alice, &phone).await;

    // Blind trust: the first device of Bob's gets the message.
    let report = alice.omemo.send(&phone.jid, "вслепую").await.unwrap();
    assert!(report.untrusted.is_empty());
    assert_eq!(text(&phone.next_text().await), "вслепую");
    let devices = alice.omemo.devices(&phone.jid).await.unwrap();
    assert_eq!(
        devices.iter().map(|d| d.trust).collect::<Vec<_>>(),
        [Trust::Blind]
    );

    // Alice compares the phone's fingerprint and marks it verified. A new device of
    // Bob's now waits for her decision: it gets nothing, and the report says so.
    alice
        .omemo
        .set_trust(&phone.jid, phone.omemo.device_id(), Trust::Verified)
        .await
        .unwrap();
    let mut laptop = Device::start("u27").await;
    converge(&[&phone, &laptop]).await;
    // The server tells Alice of the longer list a moment later.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let report = alice.omemo.send(&phone.jid, "только проверенным").await.unwrap();
    assert_eq!(report.untrusted, [(phone.jid.clone(), laptop.omemo.device_id())]);
    assert_eq!(text(&phone.next_text().await), "только проверенным");
    let missed = laptop
        .next_where(|m| m.not_for_this_device || m.text.is_some())
        .await;
    assert!(missed.not_for_this_device, "the laptop was left out: {missed:?}");
    let trust =
        |devices: &[omtalk_omemo::DeviceInfo], id| devices.iter().find(|d| d.device == id).unwrap().trust;
    let devices = alice.omemo.devices(&phone.jid).await.unwrap();
    assert_eq!(trust(&devices, laptop.omemo.device_id()), Trust::Undecided);

    // Both refused: there is nobody to write to, and the error says why.
    alice
        .omemo
        .set_trust(&phone.jid, phone.omemo.device_id(), Trust::Distrusted)
        .await
        .unwrap();
    match alice.omemo.send(&phone.jid, "никому").await {
        Err(Error::NoTrustedDevices(jid)) => assert_eq!(jid, phone.jid),
        other => panic!("{other:?}"),
    }

    // An old device of one's own leaves the account's list.
    let old = laptop.omemo.device_id();
    laptop.stop().await;
    phone.omemo.remove_own_device(old).await.unwrap();
    assert!(!phone.own_list().await.contains(&old));
    assert!(matches!(
        phone.omemo.remove_own_device(phone.omemo.device_id()).await,
        Err(Error::ThisDevice)
    ));

    stop_all([alice, phone]).await;
}
