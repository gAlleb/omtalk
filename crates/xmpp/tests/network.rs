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

#[tokio::test(flavor = "multi_thread")]
async fn a_message_sent_into_a_frozen_network_counts_only_once_the_server_has_it() {
    let open = Arc::new(AtomicBool::new(true));
    let addr = proxy(open.clone()).await;
    let (mut laptop_accounts, mut laptop) = Accounts::new();
    let (mut peer_accounts, mut peer_events) = Accounts::new();
    let alice = laptop_accounts.add(config("u47", &addr)).await;
    let bob = peer_accounts.add(config("u48", SERVER)).await;
    for events in [&mut laptop, &mut peer_events] {
        while !matches!(
            next(events, Duration::from_secs(10)).await,
            Some(Event::Online { .. })
        ) {}
    }

    open.store(false, Ordering::SeqCst);
    let message =
        Message::chat(Some(bob.jid().clone().into())).with_body(Lang::default(), "сквозь заморозку".into());
    let sending = {
        let alice = alice.clone();
        tokio::spawn(async move { alice.send(message).await })
    };
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(
        !sending.is_finished(),
        "out into the socket is not received: no success yet"
    );

    // Back: the stream goes on, the server acknowledges, the message is there.
    open.store(true, Ordering::SeqCst);
    let sent = tokio::time::timeout(Duration::from_secs(40), sending)
        .await
        .expect("an answer once the network is back")
        .unwrap();
    assert!(sent.is_ok(), "{sent:?}");
    let arrived = Instant::now();
    let mut got = false;
    while arrived.elapsed() < Duration::from_secs(20) && !got {
        if let Some(Event::Message(m)) = next(&mut peer_events, Duration::from_secs(1)).await {
            got = m.bodies.values().any(|b| b == "сквозь заморозку");
        }
    }
    assert!(got, "it arrived");
}

/// The first connection checks the password; delay the real stream's handshake so
/// the restored chat's own-account IQ is allocated before binding completes.
#[tokio::test]
async fn own_iq_queued_before_login_matches_the_servers_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let forwarding = tokio::spawn(async move {
        let mut connection = 0;
        while let Ok((mut client, _)) = listener.accept().await {
            let delay = connection > 0;
            connection += 1;
            tokio::spawn(async move {
                let mut server = TcpStream::connect(SERVER).await.unwrap();
                if delay {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
            });
        }
    });
    let (mut accounts, mut events) = Accounts::new();
    let mut settings = config("u08", &addr);
    settings.iq_timeout = Duration::from_secs(3);
    let chat: omtalk_xmpp::BareJid = format!("#early-{}%irc.example.org@bridge.localhost", settings.resource)
        .parse()
        .unwrap();
    let account = accounts.add(settings).await;
    let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
    // Deliberately no wait for Online: this is the old TUI startup call, without
    // the new retry queue or an injected/mock timeout.
    let result = account.read_up_to(&chat, &chat, "early-room-id").await;
    let stored = account
        .read_marks()
        .await
        .unwrap()
        .contains(&(chat, "early-room-id".into()));
    account.stop().await;
    forwarding.abort();
    drain.abort();
    assert!(stored, "the real server must have stored the publication");
    assert!(
        result.is_ok(),
        "the server stored and acknowledged it, but the client returned {result:?}"
    );
}

/// Quitting must not depend on an IQ, a room join, or a message being acknowledged.
#[tokio::test(flavor = "multi_thread")]
async fn quitting_with_pending_work_on_a_frozen_connection_is_bounded() {
    let open = Arc::new(AtomicBool::new(true));
    let addr = proxy(open.clone()).await;
    let (mut accounts, mut events) = Accounts::new();
    let account = accounts.add(config("u79", &addr)).await;
    tokio::time::timeout(Duration::from_secs(15), async {
        while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
    })
    .await
    .expect("login before freezing the connection");
    open.store(false, Ordering::SeqCst);
    let work = {
        let account = account.clone();
        tokio::spawn(async move {
            let room = "quit-outage@muc.localhost".parse().unwrap();
            let message = Message::chat(Some("u80@localhost".parse().unwrap()))
                .with_body(Lang::default(), "pending during outage".into());
            let _ = tokio::join!(
                account.join(&room, "test"),
                account.send(message),
                account.get(None, xmpp_parsers::ping::Ping)
            );
        })
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        while !matches!(events.recv().await.unwrap().event, Event::Disconnected { .. }) {}
    })
    .await
    .expect("the frozen server must be reported offline");
    let start = Instant::now();
    tokio::time::timeout(Duration::from_secs(3), account.stop())
        .await
        .expect("quitting must not wait for the server");
    eprintln!("stopped frozen account in {:?}", start.elapsed());
    work.abort();
}
