use std::collections::{BTreeSet, HashMap};

use omemo::{DeviceId, Session};
use omemo_xml::{DEVICE_LIST_NODE, bundle, bundle_node, device_list, message, xml};
use omtalk_xmpp::{Access, Account, Affiliation, BareJid, Element, Event, PepItem, muc};
use tokio::sync::Mutex;
use xmpp_parsers::carbons;
use xmpp_parsers::eme::ExplicitMessageEncryption;
use xmpp_parsers::message::{Lang, Message, MessageType};

use crate::envelope::{self, Direction};
use crate::{Error, State};

/// python-omemo answers with an empty message once it has received this many messages
/// in one chain without replying, so the sender's chain does not grow forever.
const STALE_CHAIN: u32 = 53;

/// The body other clients show when they cannot decrypt.
const FALLBACK_BODY: &str = "This message is OMEMO encrypted, and your client cannot decrypt it.";

pub struct Omemo {
    account: Account,
    device_id: DeviceId,
    state: Mutex<State>,
    /// Room occupants by nick, with their real JIDs: a group message names only the
    /// nick, but its keys belong to the real account.
    occupants: Mutex<HashMap<BareJid, HashMap<String, BareJid>>>,
}

/// A decrypted chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub peer: BareJid,
    pub sender: BareJid,
    pub device: DeviceId,
    pub direction: Direction,
    /// `None` for a message without content (a handshake answer or a heartbeat).
    pub text: Option<String>,
}

/// Which devices a message reached.
#[derive(Debug, Default)]
pub struct SendReport {
    pub devices: Vec<(BareJid, DeviceId)>,
    /// Devices without a usable session; they will not be able to read the message.
    pub failed: Vec<(BareJid, DeviceId, Error)>,
    /// Group members that publish no OMEMO devices at all.
    pub without_omemo: Vec<BareJid>,
}

impl Omemo {
    pub fn new(account: Account, state: State) -> Self {
        Self {
            account,
            device_id: state.keys.device_id,
            state: Mutex::new(state),
            occupants: Mutex::new(HashMap::new()),
        }
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Occupants of `room` this device knows, by nick, with their real accounts.
    pub async fn occupants(&self, room: &BareJid) -> HashMap<String, BareJid> {
        self.occupants.lock().await.get(room).cloned().unwrap_or_default()
    }

    pub async fn with_state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut *self.state.lock().await)
    }

    /// Call after every `Online { resumed: false }`: enables carbons, so messages of the
    /// own other devices reach this one, puts this device into the own device list and
    /// publishes its bundle.
    pub async fn announce(&self) -> Result<(), Error> {
        self.account.set(None, carbons::Enable).await?;
        let own = self.account.jid().clone();
        let current = match self.account.items(&own, DEVICE_LIST_NODE).await {
            Ok(items) => parse_device_list(&items),
            // A fresh account has no node yet.
            Err(omtalk_xmpp::Error::Stanza(_)) => BTreeSet::new(),
            Err(error) => return Err(error.into()),
        };
        let mut state = self.state.lock().await;
        self.publish_bundle(&state).await?;
        self.ensure_listed(&mut state, current).await
    }

    /// Handles an event of this account. Returns the decrypted message, if the event was one.
    pub async fn handle(&self, event: &Event) -> Result<Option<Incoming>, Error> {
        match event {
            Event::Pep {
                from, node, items, ..
            } if node == DEVICE_LIST_NODE => {
                let list = parse_device_list(items);
                let mut state = self.state.lock().await;
                if from == self.account.jid() {
                    // Another of our clients published a list without us; it happens
                    // when two of them publish at once. Put ourselves back.
                    self.ensure_listed(&mut state, list).await?;
                } else {
                    state.device_lists.insert(from.clone(), list);
                }
                Ok(None)
            }
            Event::Presence(presence) => {
                if let Some(occupant) = muc::occupant(presence) {
                    let mut rooms = self.occupants.lock().await;
                    let room = rooms.entry(occupant.room).or_default();
                    match occupant.real_jid {
                        Some(jid) if occupant.present => drop(room.insert(occupant.nick, jid)),
                        _ => drop(room.remove(&occupant.nick)),
                    }
                }
                Ok(None)
            }
            Event::Message(message) if message.type_ == MessageType::Groupchat => {
                let rooms = self.occupants.lock().await.clone();
                let resolve = |room: &BareJid, nick: &str| rooms.get(room)?.get(nick).cloned();
                match envelope::open_group(self.account.jid(), (**message).clone(), resolve)? {
                    // Our own message, reflected by the room: there is no key for us in it.
                    Some(envelope)
                        if envelope.sender == *self.account.jid()
                            && envelope.encrypted.header.sid == self.device_id =>
                    {
                        Ok(None)
                    }
                    Some(envelope) => match self.receive(envelope).await {
                        Err(Error::Omemo(omemo_xml::Error::NotForThisDevice))
                        | Err(Error::Omemo(omemo_xml::Error::Omemo(omemo::Error::DuplicateMessage))) => {
                            Ok(None)
                        }
                        other => other.map(Some),
                    },
                    None => Ok(None),
                }
            }
            Event::Message(message) => match envelope::open(self.account.jid(), (**message).clone()) {
                // Messages reach every device of an account, but some are addressed to
                // only one of them: the answer that completes another device's key
                // exchange, for instance. Not being a recipient is normal.
                //
                // Copies of one message are normal too: a message to the own account
                // arrives directly and as a carbon, and history catch-up repeats live
                // messages. The ratchet refuses the second copy; nothing is lost.
                Some(envelope) => match self.receive(envelope).await {
                    Err(Error::Omemo(omemo_xml::Error::NotForThisDevice))
                    | Err(Error::Omemo(omemo_xml::Error::Omemo(omemo::Error::DuplicateMessage))) => Ok(None),
                    other => other.map(Some),
                },
                None => Ok(None),
            },
            _ => Ok(None),
        }
    }

    async fn receive(&self, envelope: envelope::Envelope) -> Result<Incoming, Error> {
        let device = envelope.encrypted.header.sid;
        let mut state = self.state.lock().await;
        let key = (envelope.sender.clone(), device);
        let decrypted = message::decrypt(&state.keys, state.sessions.get(&key), &envelope.encrypted)?;

        let identity = *decrypted.received.session.their_identity();
        if !state.check_identity(&envelope.sender, device, &identity) {
            return Err(Error::IdentityChanged {
                jid: envelope.sender,
                device,
            });
        }

        // A device missing from the cached list means the cache is stale: drop it, so
        // the next message reads the list from the server again. Taking the sender into
        // the cache instead would make a list of just that one device.
        if state
            .device_lists
            .get(&envelope.sender)
            .is_some_and(|list| !list.contains(&device))
            && envelope.sender != *self.account.jid()
        {
            state.device_lists.remove(&envelope.sender);
        }
        let mut session = decrypted.received.session;
        let answer_needed =
            decrypted.received.used_pre_key.is_some() || session.receiving_length() > STALE_CHAIN;
        if let Some(pre_key) = decrypted.received.used_pre_key {
            state.keys.remove_pre_key(pre_key);
            state.keys.refill_pre_keys()?;
            self.publish_bundle(&state).await?;
        }
        if answer_needed {
            // Completes the key exchange, so the sender stops attaching it, or resets a
            // long one-sided chain.
            let empty = message::empty(state.keys.device_id, [(device, &mut session)])?;
            // To the sender's account, also for group messages: it concerns one session.
            self.account
                .send(stanza(MessageType::Chat, envelope.sender.clone(), empty, false))
                .await?;
        }
        state.sessions.insert(key, session);

        Ok(Incoming {
            peer: envelope.peer,
            sender: envelope.sender,
            device,
            direction: envelope.direction,
            text: decrypted
                .plaintext
                .map(|p| String::from_utf8_lossy(&p).into_owned()),
        })
    }

    /// Encrypts `text` for all devices of `to` and all other devices of the own account,
    /// and sends it.
    pub async fn send(&self, to: &BareJid, text: &str) -> Result<SendReport, Error> {
        let mut state = self.state.lock().await;
        let mut report = SendReport::default();
        let targets = self
            .targets(&mut state, std::slice::from_ref(to), &mut report)
            .await?;
        if !targets.iter().any(|(jid, _)| jid == to) {
            return Err(Error::NoDevices(to.clone()));
        }
        let encrypted = encrypt_for(&mut state, &targets, text)?;
        self.account
            .send(stanza(MessageType::Chat, to.clone(), encrypted, true))
            .await?;
        report.devices = targets;
        Ok(report)
    }

    /// Encrypts `text` for all devices of all members of `room` (owners, admins and
    /// members, present or not, and anyone else present), and sends it to the room.
    /// The room must be non-anonymous, or the members' accounts are unknown.
    pub async fn send_group(&self, room: &BareJid, text: &str) -> Result<SendReport, Error> {
        let mut members = BTreeSet::new();
        for affiliation in [Affiliation::Owner, Affiliation::Admin, Affiliation::Member] {
            members.extend(self.account.affiliated(room, affiliation).await?);
        }
        if let Some(present) = self.occupants.lock().await.get(room) {
            members.extend(present.values().cloned());
        }
        members.remove(self.account.jid());

        let mut state = self.state.lock().await;
        let mut report = SendReport::default();
        let members: Vec<_> = members.into_iter().collect();
        let targets = self.targets(&mut state, &members, &mut report).await?;
        if !targets.iter().any(|(jid, _)| jid != self.account.jid()) {
            return Err(Error::NoDevices(room.clone()));
        }
        let encrypted = encrypt_for(&mut state, &targets, text)?;
        self.account
            .send(stanza(MessageType::Groupchat, room.clone(), encrypted, true))
            .await?;
        report.devices = targets;
        Ok(report)
    }

    /// All devices of `peers` and the own other devices, with a session for each one
    /// reachable; the unreachable ones go into the report.
    async fn targets(
        &self,
        state: &mut State,
        peers: &[BareJid],
        report: &mut SendReport,
    ) -> Result<Vec<(BareJid, DeviceId)>, Error> {
        let own = self.account.jid().clone();
        let mut targets = Vec::new();
        for jid in peers.iter().chain([&own]) {
            if jid != &own && !state.device_lists.contains_key(jid) {
                match self.account.items(jid, DEVICE_LIST_NODE).await {
                    Ok(items) => drop(state.device_lists.insert(jid.clone(), parse_device_list(&items))),
                    // No node: this account does not use OMEMO.
                    Err(omtalk_xmpp::Error::Stanza(_)) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let devices = state.device_lists.get(jid).cloned().unwrap_or_default();
            if devices.is_empty() && jid != &own {
                report.without_omemo.push(jid.clone());
            }
            for device in devices {
                if jid == &own && device == state.keys.device_id {
                    continue;
                }
                if !state.sessions.contains_key(&(jid.clone(), device)) {
                    match self.start_session(state, jid, device).await {
                        Ok(session) => drop(state.sessions.insert((jid.clone(), device), session)),
                        Err(error) => {
                            report.failed.push((jid.clone(), device, error));
                            continue;
                        }
                    }
                }
                targets.push((jid.clone(), device));
            }
        }
        Ok(targets)
    }

    async fn start_session(&self, state: &State, jid: &BareJid, device: DeviceId) -> Result<Session, Error> {
        let items = self.account.items(jid, &bundle_node(device)).await?;
        let item = items.first().ok_or(Error::NoDevices(jid.clone()))?;
        let bundle = xml::Bundle::try_from(item.payload.clone())
            .map_err(|_| omemo_xml::Error::Malformed("bundle does not parse"))?;
        let bundle = bundle::from_xml(&bundle)?;
        if let Some(known) = state.identities.get(&(jid.clone(), device))
            && known != &bundle.identity_key
        {
            return Err(Error::IdentityChanged {
                jid: jid.clone(),
                device,
            });
        }
        Ok(Session::initiate(&state.keys, &bundle)?)
    }

    async fn publish_bundle(&self, state: &State) -> Result<(), Error> {
        let payload: Element = bundle::to_xml(&state.keys.bundle()).into();
        self.account
            .publish(
                &bundle_node(state.keys.device_id),
                Some("current"),
                payload,
                Access::Open,
            )
            .await?;
        Ok(())
    }

    /// Makes sure the own device list contains this device, publishing it if not.
    async fn ensure_listed(&self, state: &mut State, mut list: BTreeSet<DeviceId>) -> Result<(), Error> {
        let own = self.account.jid().clone();
        if list.insert(state.keys.device_id) {
            let payload: Element = device_list::to_xml(list.iter().copied()).into();
            self.account
                .publish(DEVICE_LIST_NODE, Some("current"), payload, Access::Open)
                .await?;
        }
        state.device_lists.insert(own, list);
        Ok(())
    }
}

fn parse_device_list(items: &[PepItem]) -> BTreeSet<DeviceId> {
    items
        .first()
        .and_then(|item| xml::DeviceList::try_from(item.payload.clone()).ok())
        .map(|list| device_list::from_xml(&list))
        .unwrap_or_default()
}

fn encrypt_for(
    state: &mut State,
    targets: &[(BareJid, DeviceId)],
    text: &str,
) -> Result<xml::Encrypted, Error> {
    let sender_device = state.keys.device_id;
    let recipients = state
        .sessions
        .iter_mut()
        .filter(|(key, _)| targets.contains(key))
        .map(|((_, device), session)| (*device, session));
    Ok(message::encrypt(sender_device, text.as_bytes(), recipients)?)
}

fn stanza(type_: MessageType, to: BareJid, encrypted: xml::Encrypted, with_body: bool) -> Message {
    let mut message = Message::new_with_type(type_, Some(to.into()))
        .with_payload(encrypted)
        .with_payload(ExplicitMessageEncryption {
            namespace: omemo::NAMESPACE.into(),
            name: Some("OMEMO".into()),
        });
    // Archive it (MAM) and keep it for offline delivery even without a body.
    message
        .payloads
        .push(Element::builder("store", "urn:xmpp:hints").build());
    if with_body {
        message = message.with_body(Lang::default(), FALLBACK_BODY.into());
    }
    message
}
