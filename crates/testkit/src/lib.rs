//! Test devices for the integration tests of this workspace: each one a client with
//! its own connection, keys and event loop, against the test Prosody
//! (`mise run prosody`).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use omemo::{DeviceId, OwnKeys};
use omtalk_omemo::{Ephemeral, Incoming, Omemo, Persist, State};
use omtalk_store::Store;
use omtalk_xmpp::{Account, AccountConfig, Accounts, BareJid, Event, Server};
use tokio::sync::mpsc;
use tokio::time::timeout;
use xmpp_parsers::presence::Presence;

pub const WAIT: Duration = Duration::from_secs(20);

pub struct Device {
    pub account: Account,
    pub omemo: Arc<Omemo>,
    pub jid: BareJid,
    inbox: mpsc::UnboundedReceiver<Result<Incoming, String>>,
    /// Connection events after the first login: `Online` and `Disconnected`.
    connection: mpsc::UnboundedReceiver<Event>,
}

impl Device {
    /// One client with fresh keys that keeps nothing.
    pub async fn start(user: &str) -> Self {
        let state = State::new(OwnKeys::generate(0).unwrap());
        Self::start_with(user, state, Arc::new(Ephemeral)).await
    }

    /// One client keeping its OMEMO state in the database at `path`: started again on
    /// the same file, it is the same device.
    pub async fn start_stored(user: &str, path: &Path) -> Self {
        let store = Store::open(path).unwrap();
        let jid: BareJid = format!("{user}@localhost").parse().unwrap();
        let state = store
            .load_omemo(&jid)
            .unwrap()
            .unwrap_or_else(|| State::new(OwnKeys::generate(0).unwrap()));
        Self::start_with(user, state, store.omemo(&jid)).await
    }

    /// One client: its own connection and keys, driven by its own event loop.
    pub async fn start_with(user: &str, state: State, persist: Arc<dyn Persist>) -> Self {
        let jid: BareJid = format!("{user}@localhost").parse().unwrap();
        let mut config = AccountConfig::new(jid.clone(), "test");
        config.server = Server::Plaintext {
            addr: "127.0.0.1:15222".into(),
        };
        config.iq_timeout = Duration::from_secs(5);
        // A dead connection is noticed within seconds here, not a minute.
        config.keepalive = Duration::from_secs(2);
        config.keepalive_answer = Duration::from_secs(2);

        let (mut accounts, mut events) = Accounts::new();
        let account = accounts.add(config).await;
        timeout(WAIT, async {
            while !matches!(events.recv().await.unwrap().event, Event::Online { .. }) {}
        })
        .await
        .expect("online; is the test server up (`mise run prosody`)?");

        let omemo = Arc::new(Omemo::persisting(account.clone(), state, persist));
        omemo.announce().await.unwrap();

        let (tx, inbox) = mpsc::unbounded_channel();
        let (connection_tx, connection) = mpsc::unbounded_channel();
        let driver = omemo.clone();
        tokio::spawn(async move {
            let _accounts = accounts;
            while let Some(event) = events.recv().await {
                match &event.event {
                    Event::Online { resumed, .. } => {
                        if !resumed {
                            driver.announce().await.unwrap();
                        }
                        let _ = connection_tx.send(event.event);
                        continue;
                    }
                    Event::Disconnected { reason } => {
                        // Shown only when a test fails, where a broken connection explains a lot.
                        eprintln!("{} disconnected: {reason}", driver.device_id());
                        let _ = connection_tx.send(event.event);
                        continue;
                    }
                    _ => {}
                }
                match driver.handle(&event.event).await {
                    Ok(Some(incoming)) => drop(tx.send(Ok(incoming))),
                    Ok(None) => {}
                    Err(error) => drop(tx.send(Err(error.to_string()))),
                }
            }
        });
        Self {
            account,
            omemo,
            jid,
            inbox,
            connection,
        }
    }

    /// Waits for the next login after a broken connection; `true` if the stream was
    /// resumed (XEP-0198), `false` if it is a new session.
    pub async fn reconnected(&mut self) -> bool {
        self.reconnected_within(WAIT).await
    }

    pub async fn reconnected_within(&mut self, limit: Duration) -> bool {
        timeout(limit, async {
            loop {
                if let Event::Online { resumed, .. } = self.connection.recv().await.unwrap() {
                    return resumed;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{} did not reconnect", self.jid))
    }

    /// Disconnects, like quitting the client.
    pub async fn stop(self) {
        self.account.stop().await;
    }

    pub async fn own_list(&self) -> BTreeSet<DeviceId> {
        self.omemo
            .with_state(|s| s.device_lists.get(&self.jid).cloned().unwrap_or_default())
            .await
    }

    /// The next message with text; empty handshake answers are skipped.
    pub async fn next_text(&mut self) -> Incoming {
        timeout(WAIT, async {
            loop {
                let incoming = self
                    .inbox
                    .recv()
                    .await
                    .unwrap()
                    .expect("decryption must not fail");
                if incoming.text.is_some() {
                    return incoming;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("device {} of {} got no message", self.omemo.device_id(), self.jid))
    }
}

/// Waits until the own device list on the server and in every device's cache names all
/// devices, and stays so for a moment. Devices that announce at the same time overwrite
/// each other's lists and react to stale notifications; this is where they must settle.
pub async fn converge(devices: &[&Device]) {
    let ids: BTreeSet<DeviceId> = devices.iter().map(|d| d.omemo.device_id()).collect();
    let owner = devices[0].jid.clone();
    let settled = |seen: Vec<BTreeSet<DeviceId>>| seen.iter().all(|list| list.is_superset(&ids));
    timeout(WAIT, async {
        let mut stable_since = None;
        loop {
            let mut seen = Vec::new();
            for device in devices {
                seen.push(device.own_list().await);
            }
            let on_server = devices[0]
                .account
                .items(&owner, omemo_xml::DEVICE_LIST_NODE)
                .await
                .ok()
                .and_then(|items| omemo_xml::xml::DeviceList::try_from(items.first()?.payload.clone()).ok())
                .map(|list| omemo_xml::device_list::from_xml(&list))
                .unwrap_or_default();
            seen.push(on_server);
            if settled(seen) {
                let since = *stable_since.get_or_insert_with(tokio::time::Instant::now);
                if since.elapsed() > Duration::from_millis(500) {
                    return;
                }
            } else {
                stable_since = None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("own device lists did not converge");
}

pub fn text(incoming: &Incoming) -> &str {
    incoming.text.as_deref().unwrap()
}

/// Mutual presence subscription, as between two roster contacts.
pub async fn befriend(a: &Device, b: &Device) {
    for (from, to) in [(a, b), (b, a)] {
        from.account
            .send(Presence::subscribed().with_to(to.jid.clone()))
            .await
            .unwrap();
        from.account
            .send(Presence::subscribe().with_to(to.jid.clone()))
            .await
            .unwrap();
    }
}

pub async fn unfriend(a: &Device, b: &Device) {
    use xmpp_parsers::roster::{Item, Roster, Subscription};
    for (from, to) in [(a, b), (b, a)] {
        let remove = Roster {
            ver: None,
            items: vec![Item {
                jid: to.jid.clone(),
                name: None,
                subscription: Subscription::Remove,
                ask: Default::default(),
                approved: None,
                groups: Vec::new(),
            }],
        };
        // Not being in the roster is fine too: the server says item-not-found.
        let _ = from.account.set(None, remove).await;
    }
}
