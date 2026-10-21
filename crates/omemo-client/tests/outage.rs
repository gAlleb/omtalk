//! The network goes away, then the server restarts, in the middle of a conversation,
//! against the test Prosody (`mise run prosody`). Each client must notice, come back on
//! its own, and lose nothing: what it is sent meanwhile, and what it writes meanwhile.
//!
//! Both outages stop the whole server, so they live in this test binary alone and run
//! one after the other: cargo runs test binaries in turn, never side by side.

use std::process::Command;
use std::time::Duration;

use omtalk_testkit::{Device, converge, text};

fn docker(args: &[&str]) {
    let status = Command::new("docker")
        .args(args)
        .status()
        .expect("docker is available");
    assert!(status.success(), "docker {args:?} failed");
}

/// Sends from a background task: while the connection is down the send waits in the
/// queue, and the test must be free to bring the server back meanwhile.
fn send_later(device: &Device, to: &Device, text: &str) -> tokio::task::JoinHandle<Result<(), String>> {
    let omemo = device.omemo.clone();
    let to = to.jid.clone();
    let text = text.to_owned();
    tokio::spawn(async move { omemo.send(&to, &text).await.map(drop).map_err(|e| e.to_string()) })
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_survives_network_loss_and_server_restart() {
    let (mut alice, mut bob) = tokio::join!(Device::start("u30"), Device::start("u31"));
    converge(&[&alice]).await;
    alice.omemo.send(&bob.jid, "до сбоев").await.unwrap();
    assert_eq!(text(&bob.next_text().await), "до сбоев");

    // The network vanishes: the server stops answering, nothing is closed. Only the
    // keepalive can tell; here it is 2 s of silence plus 2 s for an answer.
    docker(&["pause", "omtalk-test-prosody"]);
    let queued = send_later(&alice, &bob, "написано без сети");
    tokio::time::sleep(Duration::from_secs(8)).await;
    docker(&["unpause", "omtalk-test-prosody"]);
    alice.reconnected().await;
    bob.reconnected().await;
    queued.await.unwrap().unwrap();
    assert_eq!(text(&bob.next_text().await), "написано без сети");
    bob.omemo.send(&alice.jid, "сеть вернулась").await.unwrap();
    assert_eq!(text(&alice.next_text().await), "сеть вернулась");

    // The server restarts: it says goodbye and closes every connection, and is gone
    // until it is back. Re-registering the test accounts makes that a while.
    docker(&["restart", "omtalk-test-prosody"]);
    let queued = send_later(&alice, &bob, "написано пока сервер лежал");
    alice.reconnected_within(Duration::from_secs(90)).await;
    bob.reconnected_within(Duration::from_secs(90)).await;
    queued.await.unwrap().unwrap();
    assert_eq!(text(&bob.next_text().await), "написано пока сервер лежал");
    bob.omemo.send(&alice.jid, "сервер вернулся").await.unwrap();
    assert_eq!(text(&alice.next_text().await), "сервер вернулся");
}
