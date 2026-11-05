use crate::{Config, Event, Handle, NetworkInfo, session};
use omtalk_core::{ConnectionState, Network, NetworkId};
use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};
use tokio::sync::mpsc;

/// One discovery connection plus a separately authenticated/bound session per netid.
#[derive(Clone)]
pub struct Client {
    sessions: Arc<RwLock<BTreeMap<NetworkId, Handle>>>,
    configs: Arc<BTreeMap<String, Config>>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Client {
    pub fn start(configs: Vec<Config>) -> Result<(Self, mpsc::UnboundedReceiver<Event>), String> {
        let mut by_id = BTreeMap::new();
        for config in configs {
            config.validate()?;
            if by_id.insert(config.id.clone(), config).is_some() {
                return Err("IRC: duplicate profile id".into());
            }
        }
        let (wire_tx, mut wire_rx) = mpsc::unbounded_channel();
        let (tx, rx) = mpsc::unbounded_channel();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let client = Self {
            sessions: Arc::default(),
            configs: Arc::new(by_id),
            stop,
        };
        if client.configs.is_empty() {
            return Ok((client, rx));
        }
        for config in client.configs.values() {
            let network = config.network();
            client.sessions.write().unwrap().insert(
                network.clone(),
                session::spawn(config.clone(), network, wire_tx.clone()),
            );
        }
        let manager = client.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! { _ = stopping.changed() => break, event = wire_rx.recv() => match event { Some(event) => event, None => break } };
                if let Event::NetworksListed { network, ids } = &event {
                    // A network removed while discovery was offline sends no live
                    // deletion to this client. Reconcile the complete new list.
                    if network.network == Network::Bouncer {
                        let mut sessions = manager.sessions.write().unwrap();
                        let removed: Vec<_> = sessions
                            .keys()
                            .filter(|key| {
                                key.account == network.account
                                    && matches!(&key.network, Network::Soju(id) if !ids.contains(id))
                            })
                            .cloned()
                            .collect();
                        for id in removed {
                            if let Some(handle) = sessions.remove(&id) {
                                handle.stop();
                            }
                            let _ = tx.send(Event::Network(NetworkInfo {
                                id: id.clone(),
                                attributes: BTreeMap::new(),
                                removed: true,
                            }));
                            let _ = tx.send(Event::Connection {
                                network: id,
                                state: ConnectionState::Removed,
                            });
                        }
                    }
                }
                let source = match &event {
                    Event::Snapshot(s) => Some(&s.network),
                    Event::Connection { network, .. }
                    | Event::Joined { network, .. }
                    | Event::Left { network, .. }
                    | Event::Participants { network, .. }
                    | Event::Presence { network, .. }
                    | Event::Topic { network, .. }
                    | Event::TopicAuthor { network, .. }
                    | Event::Nick { network, .. }
                    | Event::Read { network, .. }
                    | Event::Pinned { network, .. }
                    | Event::HistoryDone { network, .. }
                    | Event::HistoryTarget { network, .. }
                    | Event::HistoryError { network, .. }
                    | Event::Diagnostic { network, .. }
                    | Event::Sent { network, .. } => Some(network),
                    Event::Message { message, .. } => message.conversation.network(),
                    _ => None,
                };
                if source.is_some_and(|n| !manager.sessions.read().unwrap().contains_key(n)) {
                    continue;
                }
                if let Event::Network(info) = &event {
                    let mut sessions = manager.sessions.write().unwrap();
                    if info.removed {
                        if let Some(handle) = sessions.remove(&info.id) {
                            handle.stop();
                        }
                        let _ = tx.send(Event::Connection {
                            network: info.id.clone(),
                            state: ConnectionState::Removed,
                        });
                    } else if let std::collections::btree_map::Entry::Vacant(entry) =
                        sessions.entry(info.id.clone())
                        && let Some(config) = manager.configs.get(&info.id.account.id)
                    {
                        entry.insert(session::spawn(config.clone(), info.id.clone(), wire_tx.clone()));
                    }
                }
                if tx.send(event).is_err() {
                    break;
                }
            }
            for handle in manager.sessions.read().unwrap().values() {
                handle.stop();
            }
        });
        Ok((client, rx))
    }

    pub fn session(&self, network: &NetworkId) -> Option<Handle> {
        self.sessions.read().unwrap().get(network).cloned()
    }
    pub fn networks(&self) -> Vec<NetworkId> {
        self.sessions
            .read()
            .unwrap()
            .keys()
            .filter(|id| id.network != Network::Bouncer)
            .cloned()
            .collect()
    }
    pub fn config(&self, id: &str) -> Option<&Config> {
        self.configs.get(id)
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}
