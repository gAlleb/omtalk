mod history;
mod registration;
mod roster;

use crate::{Config, Support, wire};
pub use history::{HistoryKind, HistoryRequest};
use irc_proto::Message;
use omtalk_core::{
    Capabilities, ChatMessage, ConnectionState, ConversationId, ConversationKind, MessageId, MessageKind,
    MessageSource, NetworkId, display_text,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Participant {
    pub nick: String,
    pub prefixes: String,
    pub away: bool,
    pub user: Option<String>,
    pub host: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub id: NetworkId,
    /// Partial attributes; an absent value removes that attribute. Secrets are
    /// excluded before this reaches UI, persistence or Debug.
    pub attributes: BTreeMap<String, Option<String>>,
    pub removed: bool,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub network: NetworkId,
    pub nick: String,
    pub support: Support,
    pub capabilities: Capabilities,
    pub labels: bool,
    pub connected: bool,
    pub tls: bool,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connection {
        network: NetworkId,
        state: ConnectionState,
    },
    Snapshot(Snapshot),
    Network(NetworkInfo),
    NetworksListed {
        network: NetworkId,
        ids: BTreeSet<String>,
    },
    Message {
        message: ChatMessage,
        target: String,
        source: MessageSource,
        label: Option<String>,
    },
    Joined {
        network: NetworkId,
        channel: String,
    },
    Left {
        network: NetworkId,
        channel: String,
    },
    Participants {
        network: NetworkId,
        channel: String,
        participants: Vec<Participant>,
    },
    Presence {
        network: NetworkId,
        channel: String,
        text: String,
        at: chrono::DateTime<chrono::Utc>,
    },
    Topic {
        network: NetworkId,
        channel: String,
        text: String,
    },
    TopicAuthor {
        network: NetworkId,
        channel: String,
        nick: String,
        at: chrono::DateTime<chrono::Utc>,
    },
    Nick {
        network: NetworkId,
        old: String,
        new: String,
        own: bool,
    },
    Read {
        network: NetworkId,
        target: String,
        timestamp: Option<String>,
    },
    Pinned {
        network: NetworkId,
        target: String,
        pinned: bool,
    },
    HistoryDone {
        network: NetworkId,
        request: HistoryRequest,
        count: usize,
        exhausted: bool,
        last_time: Option<String>,
        last_id: Option<String>,
    },
    HistoryTarget {
        network: NetworkId,
        target: String,
        timestamp: String,
    },
    HistoryError {
        network: NetworkId,
        request: HistoryRequest,
        reason: String,
    },
    Diagnostic {
        network: NetworkId,
        target: Option<String>,
        text: String,
    },
    Sent {
        network: NetworkId,
        id: MessageId,
        error: Option<String>,
    },
}

#[derive(Default)]
pub(crate) struct Effects {
    pub send: Vec<Message>,
    pub events: Vec<Event>,
}

pub(crate) struct State {
    pub config: Config,
    pub network: NetworkId,
    pub nick: String,
    server_name: Option<String>,
    pub support: Support,
    pub caps: BTreeSet<String>,
    offered: BTreeMap<String, String>,
    requested: BTreeSet<String>,
    auth_started: bool,
    authenticated: bool,
    cap_ended: bool,
    pub registered: bool,
    pub connected: bool,
    pub fatal: Option<String>,
    pub disconnected: bool,
    pub read_frontiers: BTreeMap<String, chrono::DateTime<chrono::Utc>>,
    channels: BTreeMap<String, roster::Channel>,
    batches: BTreeMap<String, history::Batch>,
    pub history_queue: VecDeque<HistoryRequest>,
    pub history_pending: Option<(HistoryRequest, Instant)>,
    pub started: Instant,
}

impl State {
    pub fn new(config: Config, network: NetworkId) -> Self {
        Self {
            nick: config.nick.clone(),
            server_name: None,
            config,
            network,
            support: Support::default(),
            caps: BTreeSet::new(),
            offered: BTreeMap::new(),
            requested: BTreeSet::new(),
            auth_started: false,
            authenticated: false,
            cap_ended: false,
            registered: false,
            connected: false,
            fatal: None,
            disconnected: false,
            read_frontiers: BTreeMap::new(),
            channels: BTreeMap::new(),
            batches: BTreeMap::new(),
            history_queue: VecDeque::new(),
            history_pending: None,
            started: Instant::now(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut capabilities = self.support.capabilities(&self.caps, self.config.tls);
        capabilities.pin_sync = self.config.mode == crate::Mode::Soju
            && self.network.network != omtalk_core::Network::Bouncer
            && self.caps.contains("draft/metadata-2");
        Snapshot {
            network: self.network.clone(),
            nick: self.nick.clone(),
            support: self.support.clone(),
            capabilities,
            labels: self.caps.contains("labeled-response"),
            connected: self.connected,
            tls: self.config.tls,
        }
    }

    pub fn conversation(&self, target: &str) -> ConversationId {
        let kind = if self.support.is_channel(target) {
            ConversationKind::Channel
        } else if target.eq_ignore_ascii_case("BouncerServ") {
            ConversationKind::Control
        } else {
            ConversationKind::Direct
        };
        ConversationId::irc(self.network.clone(), kind, target, self.support.case_mapping)
    }

    fn diagnostic(&self, e: &mut Effects, target: Option<String>, text: impl Into<String>) {
        e.events.push(Event::Diagnostic {
            network: self.network.clone(),
            target,
            text: display_text(&text.into()),
        });
    }

    pub fn receive(&mut self, message: Message) -> Effects {
        let mut e = Effects::default();
        let (cmd, args) = wire::parts(&message);
        // A timed-out/aborted batch may still be arriving. Its remaining frames
        // must never become live traffic just because its batch was discarded.
        if wire::tag(&message, "batch").is_some_and(|id| !self.batches.contains_key(id)) {
            return e;
        }
        let replay = wire::tag(&message, "batch")
            .and_then(|id| self.batches.get(id))
            .is_some_and(|b| b.history);
        if !replay && self.registration(&message, &cmd, &args, &mut e) {
            return e;
        }
        if self.history_message(&message, &cmd, &args, &mut e) {
            return e;
        }
        let batch = wire::tag(&message, "batch").and_then(|id| self.batches.get(id));
        let source = batch.map_or(MessageSource::Live, |b| b.source);
        let historic = batch.is_some_and(|b| b.history);
        let label = wire::tag(&message, "label")
            .map(str::to_owned)
            .or_else(|| batch.and_then(|b| b.label.clone()));
        if historic && let Some(batch_id) = wire::tag(&message, "batch") {
            self.count_history(
                batch_id,
                wire::tag(&message, "time").map(str::to_owned),
                wire::tag(&message, "msgid").map(str::to_owned),
            );
        }
        if historic && !matches!(cmd.as_str(), "PRIVMSG" | "NOTICE") {
            return e;
        }
        // Historical presence/mode events must never change the live roster.
        if !historic && self.roster_message(&message, &cmd, &args, &mut e) {
            return e;
        }
        match (cmd.as_str(), args.as_slice()) {
            ("METADATA", [target, key, _, value, ..]) | ("761", [_, target, key, _, value, ..])
                if self.snapshot().capabilities.pin_sync && key == "soju.im/pinned" =>
            {
                if matches!(value.as_str(), "0" | "1") {
                    e.events.push(Event::Pinned {
                        network: self.network.clone(),
                        target: target.clone(),
                        pinned: value == "1",
                    });
                }
            }
            ("770" | "771" | "772", _) => {}
            ("PRIVMSG" | "NOTICE", [to, body, ..]) => {
                let to = self.support.message_target(to);
                if cmd == "NOTICE"
                    && !self.support.is_channel(to)
                    && (to == "*"
                        || message.prefix.is_none()
                        || matches!(message.prefix, Some(irc_proto::Prefix::ServerName(_)))
                        || message
                            .prefix
                            .as_ref()
                            .is_some_and(|p| self.server_name.as_deref() == Some(p.to_string().as_str())))
                {
                    self.diagnostic(&mut e, None, wire::plain(body));
                    return e;
                }
                let who = message
                    .source_nickname()
                    .map(str::to_owned)
                    .or_else(|| message.prefix.as_ref().map(ToString::to_string))
                    .unwrap_or_default();
                let outgoing = self.support.case_mapping.equivalent(&who, &self.nick);
                let target = if self.support.is_channel(to) || outgoing {
                    to.to_owned()
                } else {
                    who.clone()
                };
                let (kind, body) = if cmd == "NOTICE" {
                    (MessageKind::Notice, body.as_str())
                } else if let Some(action) = body
                    .strip_prefix("\x01ACTION ")
                    .and_then(|s| s.strip_suffix('\x01'))
                {
                    (MessageKind::Action, action)
                } else {
                    (MessageKind::Text, body.as_str())
                };
                let server_time = wire::tag(&message, "time")
                    .filter(|s| chrono::DateTime::parse_from_rfc3339(s).is_ok())
                    .map(str::to_owned);
                let at = server_time
                    .as_ref()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.to_utc())
                    .unwrap_or_else(chrono::Utc::now);
                let server_id = wire::tag(&message, "msgid").map(str::to_owned);
                let Ok(id) = MessageId::new() else {
                    self.fatal = Some("IRC: random ID generation failed".into());
                    return e;
                };
                e.events.push(Event::Message {
                    message: ChatMessage {
                        conversation: self.conversation(&target),
                        id,
                        server_id,
                        server_time,
                        at,
                        sender: display_text(&who),
                        outgoing,
                        kind,
                        text: wire::plain(body),
                        attachment: None,
                    },
                    target,
                    source,
                    label,
                });
            }
            ("BOUNCER", [sub, id, attrs, ..])
                if sub.eq_ignore_ascii_case("NETWORK")
                    && self.network.network == omtalk_core::Network::Bouncer
                    && self.caps.contains("soju.im/bouncer-networks") =>
            {
                if let Some(batch) = wire::tag(&message, "batch").and_then(|id| self.batches.get_mut(id))
                    && let Some(networks) = &mut batch.networks
                    && attrs != "*"
                {
                    networks.insert(id.clone());
                }
                let attributes = if attrs == "*" {
                    BTreeMap::new()
                } else {
                    // Tag values have their own escaping, also when carried in a parameter.
                    format!("@{attrs} X")
                        .parse::<Message>()
                        .ok()
                        .and_then(|m| m.tags)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|t| {
                            matches!(
                                t.0.as_str(),
                                "name" | "state" | "host" | "port" | "tls" | "nickname" | "error"
                            )
                        })
                        .map(|t| (t.0, t.1.map(|v| display_text(&v))))
                        .collect()
                };
                e.events.push(Event::Network(NetworkInfo {
                    id: NetworkId {
                        account: self.network.account.clone(),
                        network: omtalk_core::Network::Soju(id.clone()),
                    },
                    attributes,
                    removed: attrs == "*",
                }));
            }
            ("MARKREAD", [target, time, ..]) => {
                let timestamp = time
                    .strip_prefix("timestamp=")
                    .filter(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
                    .map(str::to_owned);
                if time == "*" || timestamp.is_some() {
                    if let Some(at) = timestamp
                        .as_ref()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    {
                        let key = self.support.case_mapping.fold(target);
                        let frontier = self.read_frontiers.entry(key).or_insert(at.to_utc());
                        *frontier = (*frontier).max(at.to_utc());
                    }
                    e.events.push(Event::Read {
                        network: self.network.clone(),
                        target: target.clone(),
                        timestamp,
                    });
                }
            }
            ("ERROR", _) => self.disconnected = true,
            ("FAIL", args)
                if args.first().is_some_and(|s| s == "BOUNCER") && args.iter().any(|s| s == "BIND") =>
            {
                self.fatal = Some("soju: network binding refused".into())
            }
            ("FAIL" | "WARN" | "NOTE", _) => self.diagnostic(&mut e, None, args.join(" ")),
            (numeric, _)
                if numeric.parse::<u16>().is_ok()
                    && !matches!(numeric, "900" | "903" | "904" | "905" | "906" | "907") =>
            {
                // Numerics can echo passwords/arguments. Never echo authentication payloads.
                let target = args.iter().skip(1).find(|s| self.support.is_channel(s)).cloned();
                self.diagnostic(
                    &mut e,
                    target,
                    format!(
                        "{numeric} {}",
                        args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ")
                    ),
                );
            }
            _ => {}
        }
        e
    }
}
