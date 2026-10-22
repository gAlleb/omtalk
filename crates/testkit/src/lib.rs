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
        let jid = test_jid(user);
        let (accounts, mut events, account) = connect(user).await;

        let omemo = Arc::new(Omemo::persisting(account.clone(), state, persist));
        let started = std::time::Instant::now();
        omemo.announce().await.unwrap();
        if started.elapsed() > Duration::from_secs(1) {
            // Shown only when a test fails; a slow server explains timeouts.
            eprintln!("{} announced in {:?}", jid, started.elapsed());
        }

        let (tx, inbox) = mpsc::unbounded_channel();
        // What the account got while this device was off, as a client shows it on start.
        deliver(&tx, omemo.catch_up().await);
        let (connection_tx, connection) = mpsc::unbounded_channel();
        let driver = omemo.clone();
        tokio::spawn(async move {
            let _accounts = accounts;
            while let Some(event) = events.recv().await {
                match &event.event {
                    Event::Online { resumed, .. } => {
                        if !resumed {
                            driver.announce().await.unwrap();
                            deliver(&tx, driver.catch_up().await);
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

    /// The next message with text within `limit`, or `None` if none came.
    pub async fn next_text_within(&mut self, limit: Duration) -> Option<Incoming> {
        timeout(limit, async {
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
        .ok()
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
    let settled = |seen: &[BTreeSet<DeviceId>]| seen.iter().all(|list| list.is_superset(&ids));
    let mut last_seen = Vec::new();
    let converged = timeout(WAIT, async {
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
            let done = settled(&seen);
            last_seen = seen;
            if done {
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
    .await;
    assert!(
        converged.is_ok(),
        "own device lists of {owner} did not converge on {ids:?}; last seen (each device, then the server): {last_seen:?}"
    );
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

fn deliver(
    inbox: &mpsc::UnboundedSender<Result<Incoming, String>>,
    caught: Result<omtalk_omemo::CatchUp, omtalk_omemo::Error>,
) {
    match caught {
        Ok(caught) => {
            for incoming in caught.messages {
                let _ = inbox.send(Ok(incoming));
            }
            for (id, error) in caught.failed {
                let _ = inbox.send(Err(format!("archived {id}: {error}")));
            }
        }
        Err(error) => drop(inbox.send(Err(format!("catch-up: {error}")))),
    }
}

/// Makes two test accounts strangers before their devices start. The server keeps
/// rosters across runs, and a contact is pushed the other's device list on login: a
/// list from an earlier run, which a later removal cannot take back.
pub async fn strangers(a: &str, b: &str) {
    let ((_a_accounts, _, a_account), (_b_accounts, _, b_account)) = tokio::join!(connect(a), connect(b));
    for (from, to) in [(&a_account, b), (&b_account, a)] {
        use xmpp_parsers::roster::{Item, Roster, Subscription};
        let remove = Roster {
            ver: None,
            items: vec![Item {
                jid: test_jid(to),
                name: None,
                subscription: Subscription::Remove,
                ask: Default::default(),
                approved: None,
                groups: Vec::new(),
            }],
        };
        // Not being in the roster is fine too: the server says item-not-found.
        let _ = from.set(None, remove).await;
    }
    a_account.stop().await;
    b_account.stop().await;
}

/// Empties the device lists of test accounts before their devices start, as for fresh
/// accounts: the server keeps lists across runs, and each run's devices stay in them.
pub async fn fresh_device_lists(users: &[&str]) {
    for user in users {
        let (_accounts, _, account) = connect(user).await;
        let empty: omtalk_xmpp::Element = omemo_xml::device_list::to_xml([]).into();
        account
            .publish(
                omemo_xml::DEVICE_LIST_NODE,
                Some("current"),
                empty,
                omtalk_xmpp::Access::Open,
            )
            .await
            .unwrap();
        account.stop().await;
    }
}

fn test_jid(user: &str) -> BareJid {
    format!("{user}@localhost").parse().unwrap()
}

/// A logged-in connection of a test account, without OMEMO.
async fn connect(
    user: &str,
) -> (
    Accounts,
    mpsc::UnboundedReceiver<omtalk_xmpp::AccountEvent>,
    Account,
) {
    let mut config = AccountConfig::new(test_jid(user), "test");
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
    (accounts, events, account)
}

/// Closes the devices' connections. Call it at the end of a test: a connection that
/// just vanishes stays parked on the server (XEP-0198) for minutes, and messages for
/// its account go to it in the next run.
pub async fn stop_all(devices: impl IntoIterator<Item = Device>) {
    for device in devices {
        device.stop().await;
    }
}
