use crate::{
    Config, Event, Snapshot,
    state::{Effects, HistoryRequest, State},
    wire,
};
use irc_proto::{Message, message::Tag};
use omtalk_core::{ChatMessage, ConnectionState, MessageId, MessageKind, MessageSource, NetworkId};
use std::{
    collections::VecDeque,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, oneshot, watch},
};

pub enum Command {
    Send {
        id: MessageId,
        target: String,
        kind: MessageKind,
        text: String,
    },
    Raw(String),
    History(HistoryRequest),
    Read {
        target: String,
        timestamp: Option<String>,
    },
    Discover,
}

struct Request {
    command: Command,
    result: oneshot::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Request>,
    stop: watch::Sender<bool>,
    snapshot: Arc<RwLock<Snapshot>>,
}

impl Handle {
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().unwrap().clone()
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
    pub async fn send(&self, command: Command) -> Result<(), String> {
        if !self.snapshot().connected {
            return Err("IRC: disconnected; message was not queued".into());
        }
        let (result, rx) = oneshot::channel();
        self.tx
            .try_send(Request { command, result })
            .map_err(|_| "IRC: sending queue is full or closed".to_owned())?;
        tokio::time::timeout(Duration::from_secs(60), rx)
            .await
            .map_err(|_| "IRC: sending timed out; delivery is uncertain, no automatic resend".to_owned())?
            .map_err(|_| "IRC: connection closed; delivery is uncertain, no automatic resend".to_owned())?
    }
}

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}

async fn connect(config: &Config) -> Result<Box<dyn Transport>, String> {
    let stream = tokio::net::TcpStream::connect((config.server.as_str(), config.port))
        .await
        .map_err(|e| format!("IRC: connection failed ({})", e.kind()))?;
    let _ = stream.set_nodelay(true);
    if !config.tls {
        return Ok(Box::new(stream));
    }
    use tokio_rustls::rustls::{
        self,
        pki_types::{CertificateDer, ServerName, pem::PemObject},
    };
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if let Some(path) = &config.ca_certificate {
        for cert in CertificateDer::pem_file_iter(path).map_err(|_| "IRC: cannot read CA certificate")? {
            roots
                .add(cert.map_err(|_| "IRC: invalid CA certificate")?)
                .map_err(|_| "IRC: invalid CA certificate")?;
        }
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| "IRC: TLS configuration failed")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = ServerName::try_from(config.server.clone()).map_err(|_| "IRC: invalid TLS server name")?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(tls))
        .connect(name, stream)
        .await
        .map_err(|_| "IRC: TLS handshake/certificate verification failed (no downgrade)")?;
    Ok(Box::new(stream))
}

pub(crate) fn spawn(config: Config, network: NetworkId, events: mpsc::UnboundedSender<Event>) -> Handle {
    let (tx, mut rx) = mpsc::channel::<Request>(64);
    let (stop, mut stopping) = watch::channel(false);
    let snapshot = Arc::new(RwLock::new(
        State::new(config.clone(), network.clone()).snapshot(),
    ));
    let handle = Handle {
        tx,
        stop,
        snapshot: snapshot.clone(),
    };
    tokio::spawn(async move {
        let mut attempt = 0u32;
        loop {
            if *stopping.borrow() {
                break;
            }
            let _ = events.send(Event::Connection {
                network: network.clone(),
                state: ConnectionState::Connecting,
            });
            let result = tokio::select! {
                _ = stopping.changed() => break,
                result = tokio::time::timeout(Duration::from_secs(20), connect(&config)) => result.unwrap_or_else(|_| Err("IRC: connection timed out".into())),
            };
            let mut state = State::new(config.clone(), network.clone());
            let error = match result {
                Ok(stream) => run(stream, &mut state, &mut rx, &mut stopping, &events, &snapshot)
                    .await
                    .err(),
                Err(e) => Some(e),
            };
            snapshot.write().unwrap().connected = false;
            while let Ok(request) = rx.try_recv() {
                let _ = request.result.send(Err("IRC: disconnected; not sent".into()));
            }
            if *stopping.borrow() {
                break;
            }
            let error = error.unwrap_or_else(|| "IRC: connection closed".into());
            if state.fatal.is_some() || error.contains("TLS") || error.contains("CA certificate") {
                let _ = events.send(Event::Connection {
                    network: network.clone(),
                    state: ConnectionState::Failed(error),
                });
                break;
            }
            let mut effects = Effects::default();
            if let Some((request, _)) = state.history_pending.take() {
                effects.events.push(Event::HistoryError {
                    network: network.clone(),
                    request,
                    reason: "IRC: history interrupted by disconnect".into(),
                });
            }
            for request in state.history_queue.drain(..) {
                effects.events.push(Event::HistoryError {
                    network: network.clone(),
                    request,
                    reason: "IRC: disconnected".into(),
                });
            }
            for event in effects.events {
                let _ = events.send(event);
            }
            let _ = events.send(Event::Connection {
                network: network.clone(),
                state: ConnectionState::Reconnecting(error),
            });
            if state.started.elapsed() > Duration::from_secs(60) {
                attempt = 0;
            }
            let delay = Duration::from_secs((1u64 << attempt.min(6)).min(60));
            attempt = attempt.saturating_add(1);
            tokio::select! { _ = stopping.changed() => break, _ = tokio::time::sleep(delay) => {} }
        }
        snapshot.write().unwrap().connected = false;
    });
    handle
}

struct Outbox {
    frames: VecDeque<Message>,
    result: oneshot::Sender<Result<(), String>>,
    local: Vec<ChatMessage>,
    id: Option<MessageId>,
}

struct PreparedCommand {
    frames: VecDeque<Message>,
    local: Vec<ChatMessage>,
    id: Option<MessageId>,
}

fn prepare(command: Command, state: &mut State, effects: &mut Effects) -> Result<PreparedCommand, String> {
    let mut frames = VecDeque::new();
    let mut local = Vec::new();
    let mut id = None;
    match command {
        Command::Send {
            id: message_id,
            target,
            kind,
            text,
        } => {
            let chunks = wire::split_message(&target, &text, kind, state.support.line_limit)?;
            let caps = state.snapshot();
            for (i, text) in chunks.into_iter().enumerate() {
                let chunk_id = if i == 0 {
                    message_id.clone()
                } else {
                    MessageId(format!("{}.{i}", message_id.0))
                };
                let body = if kind == MessageKind::Action {
                    format!("\x01ACTION {text}\x01")
                } else {
                    text.clone()
                };
                let mut frame = wire::command(
                    if kind == MessageKind::Notice {
                        "NOTICE"
                    } else {
                        "PRIVMSG"
                    },
                    &[&target, &body],
                );
                if caps.labels {
                    frame.tags = Some(vec![Tag("label".into(), Some(chunk_id.0.clone()))]);
                }
                wire::encode(&frame)?;
                frames.push_back(frame);
                if !caps.capabilities.echo {
                    local.push(ChatMessage {
                        conversation: state.conversation(&target),
                        id: chunk_id,
                        server_id: None,
                        server_time: None,
                        at: chrono::Utc::now(),
                        sender: state.nick.clone(),
                        outgoing: true,
                        kind,
                        text,
                        attachment: None,
                    });
                }
            }
            id = Some(message_id);
        }
        Command::Raw(line) => {
            if line.contains(['\r', '\n', '\0']) {
                return Err("IRC: /raw takes one command".into());
            }
            let frame = wire::parse(&line)?;
            if frame.prefix.is_some() {
                return Err("IRC: client commands cannot contain a sender prefix".into());
            }
            let (command, arguments) = wire::parts(&frame);
            if matches!(
                command.as_str(),
                "QUIT" | "CAP" | "AUTHENTICATE" | "PASS" | "USER"
            ) || (command == "BOUNCER"
                && arguments.first().is_some_and(|a| a.eq_ignore_ascii_case("BIND")))
            {
                return Err(
                    "IRC: registration commands are managed by omtalk; /quit exits the client".into(),
                );
            }
            wire::encode(&frame)?;
            frames.push_back(frame);
        }
        Command::History(request) => state.queue_history(request, effects),
        Command::Read { target, timestamp } => {
            if !state.snapshot().capabilities.read_sync {
                return Err("IRC: read synchronization is unavailable".into());
            }
            if !wire::atom(&target) {
                return Err("IRC: invalid read target".into());
            }
            if let Some(timestamp) = timestamp {
                let at = chrono::DateTime::parse_from_rfc3339(&timestamp)
                    .map_err(|_| "IRC: invalid read timestamp")?
                    .to_utc();
                let key = state.support.case_mapping.fold(&target);
                if state.read_frontiers.get(&key).is_some_and(|old| *old >= at) {
                    return Ok(PreparedCommand { frames, local, id });
                }
                frames.push_back(wire::command(
                    "MARKREAD",
                    &[&target, &format!("timestamp={timestamp}")],
                ));
            } else {
                frames.push_back(wire::command("MARKREAD", &[&target]));
            }
        }
        Command::Discover => {
            if !state.caps.contains("soju.im/bouncer-networks") {
                return Err("soju: discovery is unavailable".into());
            }
            frames.push_back(wire::command("BOUNCER", &["LISTNETWORKS"]));
        }
    }
    Ok(PreparedCommand { frames, local, id })
}

async fn run(
    mut stream: Box<dyn Transport>,
    state: &mut State,
    rx: &mut mpsc::Receiver<Request>,
    stop: &mut watch::Receiver<bool>,
    events: &mpsc::UnboundedSender<Event>,
    snapshot: &Arc<RwLock<Snapshot>>,
) -> Result<(), String> {
    for frame in state.start() {
        tokio::time::timeout(
            Duration::from_secs(15),
            stream.write_all(wire::encode(&frame)?.as_bytes()),
        )
        .await
        .map_err(|_| "IRC: registration write timed out")?
        .map_err(|_| "IRC: registration write failed")?;
    }
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut outbox: VecDeque<Outbox> = VecDeque::new();
    let mut tick = tokio::time::interval(Duration::from_millis(350));
    let mut last_read = std::time::Instant::now();
    let mut ping = false;
    loop {
        let mut effects = Effects::default();
        tokio::select! {
            _ = stop.changed() => {
                // QUIT closes this downstream connection only, never PART or soju network delete.
                let _ = tokio::time::timeout(Duration::from_secs(1), stream.write_all(b"QUIT :omtalk closing\r\n")).await;
                return Ok(());
            },
            read = stream.read(&mut chunk) => {
                let count = read.map_err(|_| "IRC: connection read failed")?;
                if count == 0 { return Err("IRC: connection closed".into()); }
                last_read = std::time::Instant::now(); ping = false;
                buffer.extend_from_slice(&chunk[..count]);
                while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                    if end > 8192 { return Err("IRC: oversized incoming frame".into()); }
                    let bytes: Vec<_> = buffer.drain(..=end).collect();
                    let text = String::from_utf8_lossy(&bytes);
                    let message = wire::parse(&text)?;
                    let next = state.receive(message);
                    effects.send.extend(next.send); effects.events.extend(next.events);
                }
                if buffer.len() > 8192 { return Err("IRC: oversized incoming frame".into()); }
            },
            request = rx.recv() => {
                let Some(request) = request else { return Ok(()); };
                if request.result.is_closed() { continue; }
                if !state.connected { let _ = request.result.send(Err("IRC: registration is not complete".into())); }
                else if outbox.len() >= 32 { let _ = request.result.send(Err("IRC: sending queue is full".into())); }
                else { match prepare(request.command, state, &mut effects) {
                    Ok(PreparedCommand {frames, local, id}) => outbox.push_back(Outbox { frames, local, id, result: request.result }),
                    Err(error) => { let _ = request.result.send(Err(error)); },
                } }
            },
            _ = tick.tick() => {
                if !state.connected && state.started.elapsed() > Duration::from_secs(30) {
                    state.fatal = Some("IRC: registration or network binding timed out".into());
                }
                if last_read.elapsed() > Duration::from_secs(90) { return Err("IRC: keepalive timed out".into()); }
                if last_read.elapsed() > Duration::from_secs(45) && !ping {
                    effects.send.push(wire::command("PING", &["omtalk"])); ping = true;
                }
                if state.history_pending.as_ref().is_some_and(|(_, started)| started.elapsed() > Duration::from_secs(30)) {
                    state.fail_history("IRC: history request timed out", &mut effects);
                }
                while outbox.front().is_some_and(|b| b.result.is_closed()) { outbox.pop_front(); }
                if let Some(batch) = outbox.front_mut() {
                    if let Some(frame) = batch.frames.pop_front() {
                        // An incoming read from another device may have advanced
                        // while this command waited in the paced outbox.
                        let read = read_frontier(&frame, state);
                        let obsolete = read.as_ref().is_some_and(|(key, at)| state.read_frontiers.get(key).is_some_and(|old| old >= at));
                        if !obsolete {
                            let line = wire::encode(&frame)?;
                            let write = tokio::time::timeout(Duration::from_secs(15), stream.write_all(line.as_bytes())).await;
                            if !matches!(write, Ok(Ok(()))) { return Err("IRC: write failed; delivery is uncertain, no automatic resend".into()); }
                            if let Some((key, at)) = read { state.read_frontiers.insert(key, at); }
                        }
                    }
                    if batch.frames.is_empty() {
                        let batch = outbox.pop_front().unwrap();
                        for message in batch.local {
                            let target = match &message.conversation { omtalk_core::ConversationId::Irc { target, .. } => target.clone(), _ => unreachable!() };
                            effects.events.push(Event::Message { message, target, source: MessageSource::Live, label: None });
                        }
                        if let Some(id) = batch.id { effects.events.push(Event::Sent { network: state.network.clone(), id, error: None }); }
                        let _ = batch.result.send(Ok(()));
                    }
                }
            },
        }
        for frame in effects.send {
            let line = wire::encode(&frame)?;
            tokio::time::timeout(Duration::from_secs(15), stream.write_all(line.as_bytes()))
                .await
                .map_err(|_| "IRC: control write timed out")?
                .map_err(|_| "IRC: control write failed")?;
        }
        *snapshot.write().unwrap() = state.snapshot();
        for event in effects.events {
            let _ = events.send(event);
        }
        if let Some(error) = &state.fatal {
            return Err(error.clone());
        }
        if state.disconnected {
            return Err("IRC: server closed the connection".into());
        }
    }
}

fn read_frontier(frame: &Message, state: &State) -> Option<(String, chrono::DateTime<chrono::Utc>)> {
    let (name, args) = wire::parts(frame);
    if name != "MARKREAD" {
        return None;
    }
    let at = chrono::DateTime::parse_from_rfc3339(args.get(1)?.strip_prefix("timestamp=")?)
        .ok()?
        .to_utc();
    Some((state.support.case_mapping.fold(args.first()?), at))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_reads_do_not_advance_the_sent_frontier_and_another_devices_newer_read_wins() {
        let config = crate::Config {
            id: "test".into(),
            name: "test".into(),
            mode: crate::Mode::Direct,
            server: "localhost".into(),
            port: 6667,
            tls: false,
            ca_certificate: None,
            username: "me".into(),
            nick: "me".into(),
            realname: "me".into(),
            sasl: None,
            server_password: None,
            selected_network: None,
        };
        let mut state = State::new(config.clone(), config.network());
        state
            .caps
            .extend(["draft/read-marker".into(), "server-time".into()]);
        let command = || Command::Read {
            target: "#Test".into(),
            timestamp: Some("2026-11-05T00:00:00.123Z".into()),
        };
        let prepared = prepare(command(), &mut state, &mut Effects::default()).unwrap();
        assert!(state.read_frontiers.is_empty());
        assert_eq!(prepared.frames.len(), 1);
        state.receive(wire::parse(":s MARKREAD #TEST timestamp=2026-11-05T00:00:01.123Z").unwrap());
        let (key, at) = read_frontier(&prepared.frames[0], &state).unwrap();
        assert!(state.read_frontiers[&key] > at);
        assert!(
            prepare(command(), &mut state, &mut Effects::default())
                .unwrap()
                .frames
                .is_empty()
        );
        assert!(
            prepare(
                Command::Raw("BOUNCER BIND other".into()),
                &mut state,
                &mut Effects::default()
            )
            .is_err()
        );
    }
}
