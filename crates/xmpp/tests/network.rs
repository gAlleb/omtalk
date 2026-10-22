//! The network goes away under a client and comes back, against the test Prosody (`mise
//! run prosody`), through a proxy that can freeze: connections stay open and nothing
//! passes, as when a laptop's Wi-Fi drops. The client must say it is offline, connect
//! again once the network is back, and get what came meanwhile.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use omtalk_xmpp::{AccountConfig, AccountEvent, Accounts, Event, Server};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc::UnboundedReceiver;
use xmpp_parsers::message::{Lang, Message};

const SERVER: &str = "127.0.0.1:15222";

/// Forwards to the server while `open`; frozen, it moves no byte and lets no new
/// connection through, without closing anything.
async fn proxy(open: Arc<AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let (client, _) = listener.accept().await.unwrap();
            let open = open.clone();
            tokio::spawn(async move {
                while !open.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                let Ok(server) = TcpStream::connect(SERVER).await else {
                    return;
                };
                let (cr, cw) = client.into_split();
                let (sr, sw) = server.into_split();
                let pump = |open: Arc<AtomicBool>| {
                    move |mut from: tokio::net::tcp::OwnedReadHalf,
                          mut to: tokio::net::tcp::OwnedWriteHalf| async move {
                        let mut buf = [0u8; 8192];
                        loop {
                            let n = match from.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => n,
                            };
                            // Frozen: hold what came until the network is back.
                            while !open.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                            if to.write_all(&buf[..n]).await.is_err() {
                                return;
                            }
                        }
                    }
                };
                tokio::spawn(pump(open.clone())(cr, sw));
                tokio::spawn(pump(open)(sr, cw));
            });
        }
    });
    addr
}

fn config(user: &str, addr: &str) -> AccountConfig {
    let mut config = AccountConfig::new(format!("{user}@localhost").parse().unwrap(), "test");
    config.server = Server::Plaintext { addr: addr.into() };
    config.iq_timeout = Duration::from_secs(5);
    config.keepalive = Duration::from_secs(2);
    config.keepalive_answer = Duration::from_secs(2);
    config
}

async fn next(events: &mut UnboundedReceiver<AccountEvent>, within: Duration) -> Option<Event> {
    tokio::time::timeout(within, events.recv())
        .await
        .ok()
        .flatten()
        .map(|e| e.event)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frozen_network_is_noticed_and_left_behind() {
    let open = Arc::new(AtomicBool::new(true));
    let addr = proxy(open.clone()).await;
    let (mut laptop_accounts, mut laptop) = Accounts::new();
    let (mut peer_accounts, mut peer_events) = Accounts::new();
    let alice = laptop_accounts.add(config("u09", &addr)).await;
    let bob = peer_accounts.add(config("u30", SERVER)).await;
    for events in [&mut laptop, &mut peer_events] {
        while !matches!(
            next(events, Duration::from_secs(10)).await,
            Some(Event::Online { .. })
        ) {}
    }

    // The network goes. Within a few keepalives the laptop must say it is offline.
    open.store(false, Ordering::SeqCst);
    let went = Instant::now();
    let mut said_offline = false;
    while went.elapsed() < Duration::from_secs(12) {
        if let Some(Event::Disconnected { .. }) = next(&mut laptop, Duration::from_secs(1)).await {
            said_offline = true;
            break;
        }
    }
    let message = Message::chat(Some(alice.jid().clone().into()))
        .with_body(Lang::default(), "пока тебя не было".into());
    bob.send(message).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    // The network is back: online again, and the message is there.
    open.store(true, Ordering::SeqCst);
    let back = Instant::now();
    let (mut online, mut got) = (false, false);
    while back.elapsed() < Duration::from_secs(40) && !(online && got) {
        match next(&mut laptop, Duration::from_secs(1)).await {
            Some(Event::Online { .. } | Event::Reachable) => online = true,
            Some(Event::Message(m)) if m.bodies.values().any(|b| b == "пока тебя не было") => {
                got = true
            }
            _ => {}
        }
    }
    eprintln!(
        "said offline: {said_offline}, online again: {online} after {:?}, message: {got}",
        back.elapsed()
    );
    assert!(said_offline, "the laptop never said it was offline");
    assert!(online, "the laptop did not come back online");
    assert!(got, "the message sent meanwhile did not arrive");
}
