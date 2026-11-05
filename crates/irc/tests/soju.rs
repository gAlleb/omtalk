//! Run with tests/irc/run.py: a pinned real soju, private CA and disposable upstreams.
use omtalk_core::{MessageId, MessageKind, MessageSource, Network};
use omtalk_irc::{Client, Command, Config, Event, HistoryKind, HistoryRequest, Mode, Sasl};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};

async fn event(rx: &mut mpsc::UnboundedReceiver<Event>, matches: impl Fn(&Event) -> bool) -> Event {
    let mut observed = std::collections::VecDeque::new();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let e = rx.recv().await.expect("event stream");
            observed.push_back(format!("{e:?}"));
            if observed.len() > 15 {
                observed.pop_front();
            }
            if let Event::Connection {
                state: omtalk_core::ConnectionState::Failed(ref error),
                ..
            } = e
            {
                panic!("connection failed: {error}");
            }
            if matches(&e) {
                return e;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("expected IRC event within 30s; last events: {observed:#?}"))
}
fn config() -> Config {
    Config {
        id: "fixture".into(),
        name: "Fixture".into(),
        mode: Mode::Soju,
        server: "localhost".into(),
        port: std::env::var("OMTALK_TEST_SOJU_PORT").unwrap().parse().unwrap(),
        tls: true,
        ca_certificate: Some(std::env::var("OMTALK_TEST_SOJU_CA").unwrap().into()),
        username: "fixture".into(),
        nick: "do-not-overwrite-saved-nick".into(),
        realname: "omtalk fixture".into(),
        sasl: Some(Sasl {
            username: "fixture".into(),
            password: "fixture-password".into(),
        }),
        server_password: None,
        selected_network: None,
    }
}

#[tokio::test]
#[ignore = "requires tests/irc/run.py"]
async fn tls_and_sasl_fail_closed_and_direct_irc_works_without_history_or_echo() {
    for wrong_password in [false, true] {
        let mut c = config();
        if wrong_password {
            c.sasl.as_mut().unwrap().password = "incorrect-fixture-password".into();
        } else {
            c.ca_certificate = None;
        }
        let (client, mut rx) = Client::start(vec![c.clone()]).unwrap();
        let error = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match rx.recv().await.unwrap() {
                    Event::Connection {
                        state: omtalk_core::ConnectionState::Failed(error),
                        ..
                    } => break error,
                    Event::Connection {
                        state: omtalk_core::ConnectionState::Connected,
                        ..
                    } => panic!("invalid credentials/TLS accepted"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            error.contains(if wrong_password { "authentication" } else { "TLS" }),
            "{error}"
        );
        assert!(!client.session(&c.network()).unwrap().snapshot().connected);
        assert!(
            client.networks().is_empty(),
            "no silent selected-network fallback"
        );
        client.stop();
    }
    let mut c = config();
    c.mode = Mode::Direct;
    c.tls = false;
    c.sasl = None;
    c.ca_certificate = None;
    c.port = std::env::var("OMTALK_TEST_IRC_PORT").unwrap().parse().unwrap();
    c.nick = "direct-fixture".into();
    let (client, mut rx) = Client::start(vec![c.clone()]).unwrap();
    event(&mut rx, |e| matches!(e,Event::Snapshot(s)if s.connected)).await;
    let session = client.session(&c.network()).unwrap();
    assert!(!session.snapshot().capabilities.history && !session.snapshot().capabilities.echo);
    session.send(Command::Raw("JOIN #direct".into())).await.unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::Joined{channel,..}if channel=="#direct"),
    )
    .await;
    session
        .send(Command::Send {
            id: MessageId("direct-local".into()),
            target: "#direct".into(),
            kind: MessageKind::Text,
            text: "fallback text".into(),
        })
        .await
        .unwrap();
    assert!(
        matches!(event(&mut rx,|e|matches!(e,Event::Message{message,..}if message.text=="fallback text")).await,Event::Message{message,..}if message.outgoing && message.id.0=="direct-local")
    );
    session
        .send(Command::History(HistoryRequest {
            target: "#direct".into(),
            kind: HistoryKind::Latest,
            cursor: None,
        }))
        .await
        .unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::HistoryError{request,..}if request.target=="#direct"),
    )
    .await;
    client.stop();
}

// An independent downstream: no omtalk transport, state machine or parser.
type Tls = tokio_rustls::client::TlsStream<tokio::net::TcpStream>;
struct Peer {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<Tls>>>,
    write: tokio::io::WriteHalf<Tls>,
}
impl Peer {
    async fn send(&mut self, line: &str) {
        self.write
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }
    async fn until(&mut self, needle: &str) -> String {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .unwrap()
                    .expect("independent peer connected");
                if let Some(ping) = line.strip_prefix("PING ") {
                    self.send(&format!("PONG {ping}")).await;
                }
                if line.contains(needle) {
                    return line;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("independent peer waiting for {needle}"))
    }
    async fn connect(netid: &str) -> Self {
        use base64::Engine;
        use tokio_rustls::rustls::{
            self,
            pki_types::{CertificateDer, ServerName, pem::PemObject},
        };
        let c = config();
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_file_iter(c.ca_certificate.unwrap()).unwrap() {
            roots.add(cert.unwrap()).unwrap();
        }
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let stream = tokio::net::TcpStream::connect((
            "127.0.0.1",
            std::env::var("OMTALK_TEST_SOJU_DIRECT_PORT")
                .unwrap()
                .parse::<u16>()
                .unwrap(),
        ))
        .await
        .unwrap();
        let stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls))
            .connect(ServerName::try_from("localhost").unwrap(), stream)
            .await
            .unwrap();
        let (read, write) = tokio::io::split(stream);
        let mut peer = Self {
            lines: BufReader::new(read).lines(),
            write,
        };
        peer.send("CAP LS 302").await;
        peer.send("NICK independent-downstream").await;
        peer.send("USER fixture 0 * :independent peer").await;
        peer.until(" LS ").await;
        peer.send("CAP REQ :sasl soju.im/bouncer-networks batch message-tags server-time echo-message draft/read-marker draft/metadata-2").await;
        peer.until(" ACK ").await;
        peer.send("AUTHENTICATE PLAIN").await;
        peer.until("AUTHENTICATE +").await;
        peer.send(&format!(
            "AUTHENTICATE {}",
            base64::engine::general_purpose::STANDARD.encode(b"\0fixture\0fixture-password")
        ))
        .await;
        peer.until(" 903 ").await;
        peer.send(&format!("BOUNCER BIND {netid}")).await;
        peer.send("CAP END").await;
        peer.until(&format!("BOUNCER_NETID={netid}")).await;
        peer
    }
}

#[tokio::test]
#[ignore = "requires disposable fixture: python3 tests/irc/run.py"]
async fn real_soju_discovery_bind_echo_history_read_filehost_and_network_changes() {
    let (client, mut rx) = Client::start(vec![config()]).unwrap();
    let mut nets = Vec::new();
    while nets.len() < 2 {
        let e = event(
            &mut rx,
            |e| matches!(e,Event::Snapshot(s) if s.connected && matches!(s.network.network,Network::Soju(_))),
        )
        .await;
        if let Event::Snapshot(s) = e
            && !nets.iter().any(|n: &omtalk_irc::Snapshot| n.network == s.network)
        {
            nets.push(s);
        }
    }
    let alpha = nets
        .iter()
        .find(|s| s.nick == "saved-alpha")
        .expect("upstream nick preserved")
        .network
        .clone();
    let beta = nets
        .iter()
        .find(|s| s.nick == "saved-beta")
        .expect("second network isolated")
        .network
        .clone();
    let session = client.session(&alpha).unwrap();
    session.send(Command::Raw("JOIN #om".into())).await.unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::Joined{network,channel} if network==&alpha && channel=="#om"),
    )
    .await;
    // An independent client on the upstream sees the actual IRC text and ACTION.
    let stream = tokio::net::TcpStream::connect((
        "127.0.0.1",
        std::env::var("OMTALK_TEST_IRC_PORT")
            .unwrap()
            .parse::<u16>()
            .unwrap(),
    ))
    .await
    .unwrap();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    write
        .write_all(b"NICK independent\r\nUSER independent 0 * :peer\r\nJOIN #om\r\n")
        .await
        .unwrap();
    while !lines.next_line().await.unwrap().unwrap().contains(" 332 ") {}
    session
        .send(Command::Send {
            id: MessageId("literal".into()),
            target: "#om".into(),
            kind: MessageKind::Text,
            text: "/me literal".into(),
        })
        .await
        .unwrap();
    let wire = tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(wire.contains("PRIVMSG #om :/me literal"), "{wire}");
    session
        .send(Command::Send {
            id: MessageId("action".into()),
            target: "#om".into(),
            kind: MessageKind::Action,
            text: "waves".into(),
        })
        .await
        .unwrap();
    let wire = lines.next_line().await.unwrap().unwrap();
    assert!(wire.contains("\x01ACTION waves\x01"));
    event(&mut rx,|e|matches!(e,Event::Message{message,source:MessageSource::Live,..}if message.outgoing && message.kind==MessageKind::Action)).await;
    write
        .write_all(b"PRIVMSG #om :history from independent peer\r\n")
        .await
        .unwrap();
    let e = event(
        &mut rx,
        |e| matches!(e,Event::Message{message,..}if message.text=="history from independent peer"),
    )
    .await;
    let Event::Message { message, .. } = e else {
        unreachable!()
    };
    let time = message.server_time.clone().unwrap();
    assert!(message.server_id.is_some());
    session
        .send(Command::History(HistoryRequest {
            target: "#om".into(),
            kind: HistoryKind::Latest,
            cursor: None,
        }))
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Message{message,source:MessageSource::CatchUp,..}if message.text=="history from independent peer")).await;
    event(
        &mut rx,
        |e| matches!(e,Event::HistoryDone{request,..}if request.target=="#om"),
    )
    .await;
    session
        .send(Command::Read {
            target: "#om".into(),
            timestamp: Some(time.clone()),
        })
        .await
        .unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::Read{target,timestamp:Some(t),..}if target=="#om" && t==&time),
    )
    .await;
    let snapshot = session.snapshot();
    assert!(snapshot.capabilities.history && snapshot.capabilities.upload && snapshot.capabilities.read_sync);
    let Network::Soju(netid) = &alpha.network else {
        panic!()
    };
    let mut peer = Peer::connect(netid).await;
    assert!(session.snapshot().capabilities.pin_sync);
    peer.send("METADATA * SUB soju.im/pinned").await;
    peer.until(" 770 ").await;
    session
        .send(Command::Raw("METADATA #om SET soju.im/pinned 1".into()))
        .await
        .unwrap();
    peer.until("METADATA #om soju.im/pinned * 1").await;
    event(
        &mut rx,
        |e| matches!(e, Event::Pinned {network, target, pinned:true} if network==&alpha && target=="#om"),
    )
    .await;
    peer.send("METADATA #om SET soju.im/pinned 0").await;
    event(
        &mut rx,
        |e| matches!(e, Event::Pinned {network, target, pinned:false} if network==&alpha && target=="#om"),
    )
    .await;
    peer.send("PRIVMSG #om :from another downstream").await;
    let e=event(&mut rx,|e|matches!(e,Event::Message{message,source:MessageSource::Live,..}if message.text=="from another downstream")).await;
    let Event::Message { message, .. } = e else {
        panic!()
    };
    assert!(message.outgoing);
    assert_eq!(message.conversation.network(), Some(&alpha));
    let read_time = message.server_time.unwrap();
    session
        .send(Command::Read {
            target: "#om".into(),
            timestamp: Some(read_time.clone()),
        })
        .await
        .unwrap();
    peer.until(&format!("MARKREAD #om timestamp={read_time}")).await;
    // Drain the upstream observation of that same downstream send.
    assert!(
        lines
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .contains("from another downstream")
    );
    let ca = std::fs::read(config().ca_certificate.unwrap()).unwrap();
    let host = omtalk_media::filehost::FileHost::new(
        snapshot.support.filehost.as_ref().unwrap(),
        true,
        Some(("fixture".into(), "fixture-password".into())),
        Some(&ca),
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("voice.ogg");
    std::fs::write(&path, b"fixture audio body").unwrap();
    let uploaded = host.upload(&path, "audio/ogg", |_, _| {}).await.unwrap();
    assert!(uploaded.url.starts_with("https://localhost:"));
    let download = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&ca).unwrap())
        .build()
        .unwrap()
        .get(&uploaded.url)
        .send()
        .await
        .unwrap();
    assert!(download.status().is_success());
    assert_eq!(download.bytes().await.unwrap().as_ref(), b"fixture audio body");
    session
        .send(Command::Send {
            id: MessageId("file".into()),
            target: "#om".into(),
            kind: MessageKind::Text,
            text: uploaded.url.clone(),
        })
        .await
        .unwrap();
    let wire = lines.next_line().await.unwrap().unwrap();
    assert!(wire.contains(&uploaded.url), "{wire}");
    // Lose only omtalk's downstream sockets. The independent downstream and
    // upstream stay alive; a previously unknown private target arrives offline.
    let mut control = tokio::net::TcpStream::connect((
        "127.0.0.1",
        std::env::var("OMTALK_TEST_CONTROL_PORT")
            .unwrap()
            .parse::<u16>()
            .unwrap(),
    ))
    .await
    .unwrap();
    control.write_all(b"drop\n").await.unwrap();
    let mut response = String::new();
    BufReader::new(control).read_line(&mut response).await.unwrap();
    assert_eq!(response, "OK\n");
    write
        .write_all(b"PRIVMSG saved-alpha :private while offline\r\n")
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Connection{network,state:omtalk_core::ConnectionState::Reconnecting(_)}if network==&alpha)).await;
    event(
        &mut rx,
        |e| matches!(e,Event::Snapshot(s)if s.network==alpha && s.connected),
    )
    .await;
    session
        .send(Command::History(HistoryRequest {
            target: String::new(),
            kind: HistoryKind::Targets,
            cursor: None,
        }))
        .await
        .unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::HistoryTarget{network,target,..}if network==&alpha&&target=="independent"),
    )
    .await;
    session
        .send(Command::History(HistoryRequest {
            target: "independent".into(),
            kind: HistoryKind::Latest,
            cursor: None,
        }))
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Message{message,source:MessageSource::CatchUp,..}if message.text=="private while offline")).await;
    // Network updates/removal affect only their own downstream.
    let discovery = client.session(&config().network()).unwrap();
    discovery
        .send(Command::Send {
            id: MessageId("upstream-off".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: "network update alpha -enabled false".into(),
        })
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Network(n)if n.id==alpha && n.attributes.get("state")==Some(&Some("disconnected".into())))).await;
    assert!(
        session.snapshot().connected,
        "downstream remains connected while upstream is disabled"
    );
    discovery
        .send(Command::Send {
            id: MessageId("upstream-on".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: "network update alpha -enabled true".into(),
        })
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Network(n)if n.id==alpha && n.attributes.get("state")==Some(&Some("connected".into())))).await;
    discovery
        .send(Command::Send {
            id: MessageId("rename".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: "network update beta -name renamed".into(),
        })
        .await
        .unwrap();
    event(&mut rx,|e|matches!(e,Event::Network(n)if n.id==beta && n.attributes.get("name")==Some(&Some("renamed".into())))).await;
    discovery
        .send(Command::Send {
            id: MessageId("remove".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: "network delete renamed".into(),
        })
        .await
        .unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::Network(n)if n.id==beta && n.removed),
    )
    .await;
    assert!(client.session(&beta).is_none());
    assert!(client.session(&alpha).is_some());
    // Discover and bind a network added while the client is already running.
    discovery
        .send(Command::Send {
            id: MessageId("add-network".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: format!(
                "network create -name gamma -addr irc+insecure://127.0.0.1:{} -nick saved-gamma",
                std::env::var("OMTALK_TEST_IRC_PORT").unwrap()
            ),
        })
        .await
        .unwrap();
    let added = event(
        &mut rx,
        |e| matches!(e,Event::Network(n)if n.attributes.get("name")==Some(&Some("gamma".into()))),
    )
    .await;
    let Event::Network(added) = added else {
        unreachable!()
    };
    event(
        &mut rx,
        |e| matches!(e,Event::Snapshot(s)if s.network==added.id && s.connected),
    )
    .await;
    assert_eq!(client.session(&added.id).unwrap().snapshot().nick, "saved-gamma");
    discovery
        .send(Command::Send {
            id: MessageId("remove-added".into()),
            target: "BouncerServ".into(),
            kind: MessageKind::Text,
            text: "network delete gamma".into(),
        })
        .await
        .unwrap();
    event(
        &mut rx,
        |e| matches!(e,Event::Network(n)if n.id==added.id && n.removed),
    )
    .await;
    assert!(client.session(&added.id).is_none());
    client.stop();
}
