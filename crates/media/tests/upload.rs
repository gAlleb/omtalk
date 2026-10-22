//! Sending a file through the test Prosody's upload service (`mise run prosody`): found,
//! a slot handed out, the file sealed, put with its progress, fetched back and opened.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use omtalk_xmpp::{AccountConfig, AccountEvent, Accounts, Event, Server};

#[tokio::test(flavor = "multi_thread")]
async fn a_sealed_file_goes_up_and_comes_back_whole() {
    let mut config = AccountConfig::new("u08@localhost".parse().unwrap(), "test");
    config.server = Server::Plaintext {
        addr: "127.0.0.1:15222".into(),
    };
    config.iq_timeout = Duration::from_secs(5);
    let (mut accounts, mut events) = Accounts::new();
    let account = accounts.add(config).await;
    while let Some(AccountEvent { event, .. }) = events.recv().await {
        if matches!(event, Event::Online { .. }) {
            break;
        }
    }

    let service = account
        .upload_service()
        .await
        .unwrap()
        .expect("the test server has one");
    assert_eq!(service.jid.as_str(), "upload.localhost");
    assert_eq!(service.max_size, Some(10 * 1024 * 1024));

    let picture: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let sealed = omtalk_media::encrypt(&picture);
    let slot = account
        .upload_slot(
            &service.jid,
            "кот.png",
            sealed.data.len() as u64,
            Some("image/png"),
        )
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    omtalk_media::upload(
        &slot.put,
        &slot.headers,
        sealed.data.clone(),
        Some("image/png"),
        move |sent, total| log.lock().unwrap().push((sent, total)),
    )
    .await
    .unwrap();
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() > 2, "progress in steps: {seen:?}");
    assert_eq!(
        seen.last(),
        Some(&(sealed.data.len() as u64, sealed.data.len() as u64))
    );

    // What the server keeps is sealed; the link's key opens it.
    let stored = reqwest::get(&slot.get).await.unwrap().bytes().await.unwrap();
    assert_ne!(&stored[..], &picture[..]);
    assert_eq!(
        omtalk_media::decrypt(&stored, &sealed.iv, &sealed.key).unwrap(),
        picture
    );
    let link = omtalk_media::aesgcm_link(&slot.get, &sealed.iv, &sealed.key);
    assert!(link.starts_with("aesgcm://127.0.0.1:15280/"), "{link}");
    let media = omtalk_media::Media::from_text(&link).expect("our own link reads back");
    assert!(media.encrypted() && media.is_image(), "{media:?}");
}
