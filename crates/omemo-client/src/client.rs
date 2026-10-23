use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use omemo::{DeviceId, Session};
use omemo_xml::{DEVICE_LIST_NODE, bundle, bundle_node, device_list, message, xml};
use omtalk_xmpp::{Access, Account, Affiliation, BareJid, Element, Event, PepItem, muc};
use tokio::sync::Mutex;
use xmpp_parsers::carbons;
use xmpp_parsers::eme::ExplicitMessageEncryption;
use xmpp_parsers::message::{Lang, Message, MessageType};

use crate::envelope::{self, Content, Direction};
use crate::state::{Identity, Trust};
use crate::{Ephemeral, Error, Persist, State};

/// python-omemo answers with an empty message once it has received this many messages
/// in one chain without replying, so the sender's chain does not grow forever.
const STALE_CHAIN: u32 = 53;

/// Archived messages read per request while catching up.
const ARCHIVE_PAGE: usize = 50;
/// How much of a room's past the first entry shows, as Gajim does.
const ROOM_HISTORY: usize = 50;

/// The body other clients show when they cannot decrypt.
const FALLBACK_BODY: &str = "This message is OMEMO encrypted, and your client cannot decrypt it.";

pub struct Omemo {
    account: Account,
    device_id: DeviceId,
    state: Mutex<State>,
    persist: Arc<dyn Persist>,
    /// Whether this session has read the archive yet. Until it has, live messages must
    /// not move the archive position: they arrive first after a reconnect, and moving
    /// past them would skip what was missed before.
    caught_up: Mutex<HashSet<BareJid>>,
    /// Room occupants by nick, with their real JIDs: a group message names only the
    /// nick, but its keys belong to the real account.
    occupants: Mutex<HashMap<BareJid, HashMap<String, BareJid>>>,
}

/// A chat message to show: decrypted, or sent without encryption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    pub peer: BareJid,
    pub sender: BareJid,
    /// The sender's nick, in a group chat.
    pub nick: Option<String>,
    /// The sending device; `None` for plain text, which names none.
    pub device: Option<DeviceId>,
    pub direction: Direction,
    /// `false` for plain text: show it, marked as unencrypted.
    pub encrypted: bool,
    /// `None` for a message without content (a handshake answer or a heartbeat).
    pub text: Option<String>,
    /// When the server got it (Unix seconds), for a message read from an archive;
    /// `None` for one that arrives live, which is now.
    pub at: Option<i64>,
    /// Encrypted text with no key for this device (`text` is `None`): the sender's
    /// client does not know this device, or does not trust it. Worth saying, since
    /// otherwise someone wrote and nothing shows.
    pub not_for_this_device: bool,
    /// The sender's id for the message, to which receipts and read markers refer.
    pub id: Option<String>,
    /// The sender would like a read marker (XEP-0333).
    pub markable: bool,
    /// The archive's id for it (XEP-0359): the own account's for chats, the room's for
    /// rooms. One's other clients learn what was read by it (XEP-0490).
    pub archive_id: Option<String>,
    /// Written on a device the user refused or has not decided on: worth a mark.
    pub untrusted: bool,
    /// The id of the message this one answers (XEP-0461), if it is a reply.
    pub reply_to: Option<String>,
    /// The text is a link to a file the sender shared (XEP-0066 out of band data names
    /// it), not a link written: a page is shown as a link, never fetched.
    pub shared_file: bool,
}

/// One published device of an account, as [`Omemo::devices`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device: DeviceId,
    /// From its bundle now; `None` if the bundle is missing or broken (a dead device).
    pub identity_key: Option<[u8; 32]>,
    /// The key this device recorded for it before, if it ever talked to it.
    pub known_identity_key: Option<[u8; 32]>,
    pub has_session: bool,
    /// The device this `Omemo` runs on.
    pub this_device: bool,
    pub trust: Trust,
}

/// An identity key as Gajim and Conversations show it: 64 hex digits in groups of 8.
pub fn fingerprint(identity_key: &[u8; 32]) -> String {
    let hex: String = identity_key.iter().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes()
        .chunks(8)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Who an OMEMO message is from and about, once its envelope is open.
struct Received {
    peer: BareJid,
    sender: BareJid,
    nick: Option<String>,
    direction: Direction,
    meta: envelope::Meta,
}

/// What [`Omemo::catch_up`] found in the archive, oldest first.
#[derive(Debug, Default)]
pub struct CatchUp {
    pub messages: Vec<Incoming>,
    /// Archived messages addressed to this device that did not decrypt, by archive id.
    pub failed: Vec<(String, Error)>,
    /// Where this device stopped is no longer in the archive (expired, or the server
    /// lost it): messages from then until now could not be read.
    pub position_lost: bool,
    /// Encrypted messages with no key for this device: written before it existed, or by
    /// a client that did not know it yet. Nobody can read them here.
    pub unreadable: usize,
}

/// Which devices a message reached.
#[derive(Debug, Default)]
pub struct SendReport {
    pub devices: Vec<(BareJid, DeviceId)>,
    /// Devices without a usable session; they will not be able to read the message.
    pub failed: Vec<(BareJid, DeviceId, Error)>,
    /// Group members that publish no OMEMO devices at all.
    pub without_omemo: Vec<BareJid>,
    /// Devices left out because they are refused or not decided on.
    pub untrusted: Vec<(BareJid, DeviceId)>,
}

impl Omemo {
    /// OMEMO for `account`; nothing is saved. See [`Omemo::persisting`].
    pub fn new(account: Account, state: State) -> Self {
        Self::persisting(account, state, Arc::new(Ephemeral))
    }

    /// OMEMO for `account`, writing every change of `state` to `persist`.
    pub fn persisting(account: Account, state: State, persist: Arc<dyn Persist>) -> Self {
        Self {
            account,
            device_id: state.keys.device_id,
            state: Mutex::new(state),
            persist,
            caught_up: Mutex::new(HashSet::new()),
            occupants: Mutex::new(HashMap::new()),
        }
    }

    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// This device's identity key; see [`crate::fingerprint`].
    pub async fn identity_key(&self) -> [u8; 32] {
        *self.state.lock().await.keys.identity.public()
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
        // A new session: rooms have to be joined and every archive read again.
        self.caught_up.lock().await.clear();
        self.account.set(None, carbons::Enable).await?;
        let own = self.account.jid().clone();
        // Only a node that is not there (a fresh account) is an empty list. Any other
        // refusal, or an answer that is not a list, is no list to publish over: that
        // would drop the account's other devices (Gajim, Monal…) from OMEMO.
        let current = match self.account.items(&own, DEVICE_LIST_NODE).await {
            Ok(items) => own_device_list(&items)?,
            Err(omtalk_xmpp::Error::Stanza(e))
                if e.defined_condition == omtalk_xmpp::stanza_error::DefinedCondition::ItemNotFound =>
            {
                BTreeSet::new()
            }
            Err(error) => return Err(error.into()),
        };
        let mut state = self.state.lock().await;
        // On disk before on the server: a published pre key must never be one we lost.
        self.persist.keys(&state.keys)?;
        self.publish_bundle(&state).await?;
        self.ensure_listed(&mut state, current).await
    }

    /// Handles an event of this account. Returns the decrypted message, if the event was one.
    pub async fn handle(&self, event: &Event) -> Result<Option<Incoming>, Error> {
        match event {
            Event::Pep {
                from, node, items, ..
            } if node == DEVICE_LIST_NODE => {
                let mut state = self.state.lock().await;
                if from == self.account.jid() {
                    // One's own list is published back: read strictly, or not at all.
                    let list = own_device_list(items).unwrap_or_default();
                    // Another of our clients published a list without us; it happens
                    // when two of them publish at once. The notification may be stale by
                    // now, and publishing it back would drop whoever came since: read the
                    // list again, and put ourselves back only if we are still missing.
                    let list = if list.contains(&self.device_id) {
                        list
                    } else {
                        own_device_list(&self.account.items(from, DEVICE_LIST_NODE).await?)?
                    };
                    self.ensure_listed(&mut state, list).await?;
                } else {
                    state.device_lists.insert(from.clone(), parse_device_list(items));
                }
                Ok(None)
            }
            Event::Presence(presence) => {
                // Only a room one is in says who is in it: anyone can send a presence
                // that looks like a room's, naming whoever they like.
                if let Some(occupant) = muc::occupant(presence).filter(|o| self.account.in_room(&o.room)) {
                    let mut rooms = self.occupants.lock().await;
                    let room = rooms.entry(occupant.room).or_default();
                    match occupant.real_jid {
                        Some(jid) if occupant.present => drop(room.insert(occupant.nick, jid)),
                        _ => drop(room.remove(&occupant.nick)),
                    }
                }
                Ok(None)
            }
            // A groupchat message from a room one is not in is no room's: nobody's to show.
            Event::Message(message)
                if message.type_ == MessageType::Groupchat
                    && !message
                        .from
                        .as_ref()
                        .is_some_and(|from| self.account.in_room(&from.to_bare())) =>
            {
                Ok(None)
            }
            Event::Message(message) if message.type_ == MessageType::Groupchat => {
                let rooms = self.occupants.lock().await.clone();
                let resolve = |room: &BareJid, nick: &str| rooms.get(room)?.get(nick).cloned();
                let archived = message.from.as_ref().map(|f| f.to_bare()).and_then(|room| {
                    let id = envelope::archive_id(&room, message)?;
                    Some((room, id))
                });
                if let Some((room, id)) = &archived {
                    self.live_archive_position(room, id).await?;
                }
                let incoming = match envelope::open_group(self.account.jid(), (**message).clone(), resolve)? {
                    Some(envelope) => self.open_envelope(envelope).await?,
                    None => None,
                };
                Ok(incoming.map(|i| Incoming {
                    archive_id: archived.map(|(_, id)| id),
                    ..i
                }))
            }
            Event::Message(message) => {
                let incoming = match envelope::open(self.account.jid(), (**message).clone()) {
                    Some(envelope) => self.open_envelope(envelope).await?,
                    None => None,
                };
                let own = self.account.jid().clone();
                let archive_id = envelope::archive_id(&own, message);
                if let Some(id) = &archive_id {
                    self.live_archive_position(&own, id).await?;
                }
                Ok(incoming.map(|i| Incoming { archive_id, ..i }))
            }
            _ => Ok(None),
        }
    }

    /// What to show for a message: plain text as it is, OMEMO decrypted. `None` for
    /// what is not meant to be shown here (see [`ignorable`]).
    async fn open_envelope(&self, envelope: envelope::Envelope) -> Result<Option<Incoming>, Error> {
        let encrypted = match envelope.content {
            Content::Plain(text) => {
                return Ok(Some(Incoming {
                    peer: envelope.peer,
                    sender: envelope.sender,
                    nick: envelope.nick,
                    device: None,
                    direction: envelope.direction,
                    encrypted: false,
                    text: Some(text),
                    at: None,
                    not_for_this_device: false,
                    id: envelope.meta.id,
                    markable: envelope.meta.markable,
                    archive_id: None,
                    untrusted: false,
                    reply_to: envelope.meta.reply_to,
                    shared_file: envelope.meta.shared_file,
                }));
            }
            Content::Omemo(_) if self.has_no_key(&envelope) => {
                return Ok(Some(Incoming {
                    peer: envelope.peer,
                    sender: envelope.sender,
                    nick: envelope.nick,
                    device: None,
                    direction: envelope.direction,
                    encrypted: true,
                    text: None,
                    at: None,
                    not_for_this_device: true,
                    id: envelope.meta.id,
                    markable: envelope.meta.markable,
                    archive_id: None,
                    untrusted: false,
                    reply_to: envelope.meta.reply_to,
                    shared_file: false,
                }));
            }
            Content::Omemo(encrypted) => encrypted,
        };
        // This device's own message, reflected by a room or kept in an archive: it holds
        // no key for the device that wrote it.
        if envelope.sender == *self.account.jid() && encrypted.header.sid == self.device_id {
            return Ok(None);
        }
        let parts = Received {
            peer: envelope.peer,
            sender: envelope.sender,
            nick: envelope.nick,
            direction: envelope.direction,
            meta: envelope.meta,
        };
        ignorable(self.receive(parts, encrypted).await)
    }

    async fn receive(&self, envelope: Received, encrypted: xml::Encrypted) -> Result<Incoming, Error> {
        let device = encrypted.header.sid;
        let mut state = self.state.lock().await;
        let key = (envelope.sender.clone(), device);
        let decrypted = message::decrypt(&state.keys, state.sessions.get(&key), &encrypted)?;

        let identity = *decrypted.received.session.their_identity();
        // A device seen for the first time must be one its account lists: else anyone
        // who passes for the account (a made-up room, a forged archive) could slip in
        // a key of their own as one of its devices.
        if !state.identities.contains_key(&(envelope.sender.clone(), device))
            && !self.listed(&mut state, &envelope.sender, device).await?
        {
            return Err(Error::UnlistedDevice {
                jid: envelope.sender.clone(),
                device,
            });
        }
        self.record_identity(&mut state, &envelope.sender, device, &identity)?;

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
        // The answer that completes the key exchange (so the sender stops attaching it),
        // or resets a long one-sided chain: made here, as it moves the session on.
        let answer = if answer_needed {
            Some(message::empty(state.keys.device_id, [(device, &mut session)])?)
        } else {
            None
        };
        // The session is kept before the pre key is spent and before anything goes over
        // the network: lost now, a repeat of the key exchange would find neither, and the
        // first message would be gone. A repeat decrypts in the session kept.
        self.persist.session(&key.0, key.1, &session)?;
        state.sessions.insert(key, session);
        if let Some(pre_key) = decrypted.received.used_pre_key {
            state.keys.remove_pre_key(pre_key);
            state.keys.refill_pre_keys()?;
            self.persist.keys(&state.keys)?;
        }
        // On the network, last, and not at the message's cost: a bundle not republished
        // or an answer not sent now goes with the next login or the next message.
        if decrypted.received.used_pre_key.is_some() {
            let _ = self.publish_bundle(&state).await;
        }
        let untrusted = !state.trust_of(&envelope.sender, device).receives();
        drop(state);
        if let Some(empty) = answer {
            // To the sender's account, also for group messages: it concerns one session.
            let _ = self
                .account
                .send(stanza(MessageType::Chat, envelope.sender.clone(), empty, None))
                .await;
        }

        Ok(Incoming {
            peer: envelope.peer,
            sender: envelope.sender,
            nick: envelope.nick,
            device: Some(device),
            direction: envelope.direction,
            encrypted: true,
            text: decrypted
                .plaintext
                .map(|p| String::from_utf8_lossy(&p).into_owned()),
            at: None,
            not_for_this_device: false,
            id: envelope.meta.id,
            markable: envelope.meta.markable,
            archive_id: None,
            untrusted,
            reply_to: envelope.meta.reply_to,
            shared_file: false,
        })
    }

    /// Reads what the account's archive holds since this device last looked: messages
    /// that reached the account while this client was closed went to the clients that
    /// were online, and only the archive still has them. Call it after
    /// [`Omemo::announce`]. On the very first run there is no position yet: the archive
    /// is not read, only marked, so an old history is not replayed.
    pub async fn catch_up(&self) -> Result<CatchUp, Error> {
        self.catch_up_archive(None).await
    }

    /// The same for a group chat's archive: what the room got while this device was not
    /// in it. Call it after joining. The room must be non-anonymous; its archive names
    /// each sender's account. The first time in a room there is no position yet, and its
    /// last [`ROOM_HISTORY`] messages are read instead.
    pub async fn catch_up_room(&self, room: &BareJid) -> Result<CatchUp, Error> {
        self.catch_up_archive(Some(room)).await
    }

    async fn catch_up_archive(&self, room: Option<&BareJid>) -> Result<CatchUp, Error> {
        let own = self.account.jid().clone();
        let archive = room.cloned().unwrap_or_else(|| own.clone());
        let mut after = self.state.lock().await.archive_positions.get(&archive).cloned();
        let mut caught = CatchUp::default();
        if after.is_none() {
            match room {
                Some(_) => {
                    let page = self.account.archive_last(room, ROOM_HISTORY).await?;
                    for archived in page.messages {
                        self.take_archived(room, archived, &mut caught).await?;
                    }
                }
                None => {
                    if let Some(newest) = self.account.archive_newest(room).await? {
                        self.set_archive_position(&archive, &newest).await?;
                    }
                }
            }
            self.caught_up.lock().await.insert(archive);
            return Ok(caught);
        }
        loop {
            let page = match self
                .account
                .archive_after(room, after.as_deref(), ARCHIVE_PAGE)
                .await
            {
                Ok(page) => page,
                // The position is gone from the archive: expired, or the server lost
                // its data. Everything since cannot be found; go on from now, and say so.
                Err(omtalk_xmpp::Error::Stanza(e))
                    if e.defined_condition == omtalk_xmpp::stanza_error::DefinedCondition::ItemNotFound =>
                {
                    match self.account.archive_newest(room).await? {
                        Some(newest) => self.set_archive_position(&archive, &newest).await?,
                        // An empty archive: nothing to miss; next time starts afresh.
                        None => {
                            self.persist.forget_archive_position(&archive)?;
                            self.state.lock().await.archive_positions.remove(&archive);
                        }
                    }
                    caught.position_lost = true;
                    break;
                }
                Err(e) => return Err(e.into()),
            };
            for archived in page.messages {
                after = Some(archived.id.clone());
                self.take_archived(room, archived, &mut caught).await?;
            }
            if page.complete || page.last.is_none() {
                break;
            }
            after = page.last.or(after);
        }
        self.caught_up.lock().await.insert(archive);
        Ok(caught)
    }

    /// The newest `max` messages with `peer` from the account's archive, for a chat this
    /// device has no history of. Read only: the archive's position does not move. Most
    /// encrypted ones were written before this device existed and are only counted.
    pub async fn history_with(&self, peer: &BareJid, max: usize) -> Result<CatchUp, Error> {
        let page = self.account.archive_with(peer, max).await?;
        let mut caught = CatchUp::default();
        for archived in page.messages {
            self.open_archived(None, archived, &mut caught).await;
        }
        Ok(caught)
    }

    /// One archived message: opened, counted, and the archive's position moved past it.
    async fn take_archived(
        &self,
        room: Option<&BareJid>,
        archived: omtalk_xmpp::mam::Archived,
        caught: &mut CatchUp,
    ) -> Result<(), Error> {
        let archive = room.cloned().unwrap_or_else(|| self.account.jid().clone());
        let id = archived.id.clone();
        self.open_archived(room, archived, caught).await;
        self.set_archive_position(&archive, &id).await
    }

    /// One archived message, opened and counted into `caught`.
    async fn open_archived(
        &self,
        room: Option<&BareJid>,
        archived: omtalk_xmpp::mam::Archived,
        caught: &mut CatchUp,
    ) {
        let own = self.account.jid().clone();
        let at = archived.stamp.as_ref().map(|s| s.0.timestamp());
        let envelope = match room {
            None => Ok(envelope::open_archived(&own, archived.message)),
            Some(room) => envelope::open_room_archived(&own, room, archived.message),
        };
        let result = match envelope {
            Ok(Some(envelope)) if self.has_no_key(&envelope) => {
                caught.unreadable += 1;
                Ok(None)
            }
            Ok(Some(envelope)) => self.open_envelope(envelope).await,
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        match result {
            Ok(Some(incoming)) => caught.messages.push(Incoming {
                at,
                archive_id: Some(archived.id.clone()),
                ..incoming
            }),
            Ok(None) => {}
            // One message that does not decrypt must not hold up the rest.
            Err(error) => caught.failed.push((archived.id.clone(), error)),
        }
    }

    /// An encrypted message with text that names no key for this device. Not this
    /// device's own (it holds no key for itself either, and is simply not shown), and
    /// not a key exchange or heartbeat, which carry no text.
    fn has_no_key(&self, envelope: &envelope::Envelope) -> bool {
        let Content::Omemo(encrypted) = &envelope.content else {
            return false;
        };
        let own_device = envelope.sender == *self.account.jid() && encrypted.header.sid == self.device_id;
        encrypted.payload.is_some()
            && !own_device
            && !encrypted.header.keys.iter().any(|k| k.rid == self.device_id)
    }

    /// Moves an archive's position to a live message, but only once that archive was
    /// read in this session: live messages come first after a reconnect, and moving past
    /// them would skip what was missed before.
    async fn live_archive_position(&self, archive: &BareJid, id: &str) -> Result<(), Error> {
        if self.caught_up.lock().await.contains(archive) {
            self.set_archive_position(archive, id).await?;
        }
        Ok(())
    }

    async fn set_archive_position(&self, archive: &BareJid, id: &str) -> Result<(), Error> {
        self.persist.archive_position(archive, id)?;
        self.state
            .lock()
            .await
            .archive_positions
            .insert(archive.clone(), id.to_owned());
        Ok(())
    }

    /// Encrypts `text` for all devices of `to` and all other devices of the own account,
    /// and sends it.
    pub async fn send(&self, to: &BareJid, text: &str) -> Result<SendReport, Error> {
        self.send_with_id(to, text, &omtalk_xmpp::receipts::new_id())
            .await
    }

    /// [`Omemo::send`] under the message id `id`, to which receipts and read markers
    /// will refer.
    pub async fn send_with_id(&self, to: &BareJid, text: &str, id: &str) -> Result<SendReport, Error> {
        self.send_with(to, text, id, Vec::new()).await
    }

    /// [`Omemo::send_with_id`] with more elements beside the encrypted one, outside it:
    /// a reply's (XEP-0461), for instance.
    pub async fn send_with(
        &self,
        to: &BareJid,
        text: &str,
        id: &str,
        extra: Vec<Element>,
    ) -> Result<SendReport, Error> {
        let mut state = self.state.lock().await;
        let mut report = SendReport::default();
        let targets = self
            .targets(&mut state, std::slice::from_ref(to), &mut report)
            .await?;
        if !targets.iter().any(|(jid, _)| jid == to) {
            if report.untrusted.iter().any(|(jid, _)| jid == to) {
                return Err(Error::NoTrustedDevices(to.clone()));
            }
            return Err(Error::NoDevices(to.clone()));
        }
        let encrypted = self.encrypt_for(&mut state, &targets, text)?;
        // Encrypted and kept: the state is free for incoming messages while this one
        // waits for the server's word.
        drop(state);
        self.account
            .send(with(
                stanza(MessageType::Chat, to.clone(), encrypted, Some(id)),
                extra,
            ))
            .await?;
        report.devices = targets;
        Ok(report)
    }

    /// Encrypts `text` for all devices of all members of `room` (owners, admins and
    /// members, present or not, and anyone else present), and sends it to the room.
    /// The room must be non-anonymous, or the members' accounts are unknown.
    pub async fn send_group(&self, room: &BareJid, text: &str) -> Result<SendReport, Error> {
        self.send_group_with_id(room, text, &omtalk_xmpp::receipts::new_id())
            .await
    }

    /// [`Omemo::send_group`] under the message id `id`; the room sends it back with it.
    pub async fn send_group_with_id(
        &self,
        room: &BareJid,
        text: &str,
        id: &str,
    ) -> Result<SendReport, Error> {
        self.send_group_with(room, text, id, Vec::new()).await
    }

    /// [`Omemo::send_group_with_id`] with more elements beside the encrypted one.
    pub async fn send_group_with(
        &self,
        room: &BareJid,
        text: &str,
        id: &str,
        extra: Vec<Element>,
    ) -> Result<SendReport, Error> {
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
        let encrypted = self.encrypt_for(&mut state, &targets, text)?;
        // Encrypted and kept: the state is free for incoming messages while this one
        // waits for the server's word.
        drop(state);
        self.account
            .send(with(
                stanza(MessageType::Groupchat, room.clone(), encrypted, Some(id)),
                extra,
            ))
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
                if !(jid == &own && device == state.keys.device_id) {
                    targets.push((jid.clone(), device));
                }
            }
        }

        // Bundles of devices without a session, all requested at once: one by one, a
        // contact with many devices (or dead ones) costs a round trip each.
        let missing: Vec<_> = targets
            .iter()
            .filter(|key| !state.sessions.contains_key(key))
            .cloned()
            .collect();
        let bundles = futures::future::join_all(
            missing
                .iter()
                .map(|(jid, device)| self.fetch_bundle(jid, *device)),
        )
        .await;
        for ((jid, device), bundle) in missing.into_iter().zip(bundles) {
            let started = bundle.and_then(|bundle| {
                let session = Session::initiate(&state.keys, &bundle)?;
                self.record_identity(state, &jid, device, &bundle.identity_key)?;
                Ok(session)
            });
            match started {
                // Saved once it has encrypted something, in `encrypt_for`.
                Ok(session) => drop(state.sessions.insert((jid, device), session)),
                Err(error) => {
                    targets.retain(|key| key != &(jid.clone(), device));
                    report.failed.push((jid, device, error));
                }
            }
        }
        // Devices refused, or new ones not decided on, get nothing; the report says so.
        targets.retain(|(jid, device)| {
            let receives = state.trust_of(jid, *device).receives();
            if !receives {
                report.untrusted.push((jid.clone(), *device));
            }
            receives
        });
        Ok(targets)
    }

    /// The devices `jid` publishes now, each with its identity key and what this device
    /// knows about it: the material for comparing fingerprints with another client.
    pub async fn devices(&self, jid: &BareJid) -> Result<Vec<DeviceInfo>, Error> {
        let list = match self.account.items(jid, DEVICE_LIST_NODE).await {
            Ok(items) => parse_device_list(&items),
            Err(omtalk_xmpp::Error::Stanza(_)) => BTreeSet::new(),
            Err(error) => return Err(error.into()),
        };
        let bundles =
            futures::future::join_all(list.iter().map(|device| self.fetch_bundle(jid, *device))).await;
        let state = self.state.lock().await;
        Ok(list
            .into_iter()
            .zip(bundles)
            .map(|(device, bundle)| {
                let key = (jid.clone(), device);
                DeviceInfo {
                    device,
                    identity_key: bundle.ok().map(|b| b.identity_key),
                    known_identity_key: state.identities.get(&key).copied(),
                    has_session: state.sessions.contains_key(&key),
                    trust: state.trust_of(jid, device),
                    this_device: jid == self.account.jid() && device == self.device_id,
                }
            })
            .collect())
    }

    async fn fetch_bundle(&self, jid: &BareJid, device: DeviceId) -> Result<omemo::Bundle, Error> {
        let items = self.account.items(jid, &bundle_node(device)).await?;
        let item = items.first().ok_or(Error::NoDevices(jid.clone()))?;
        let bundle = xml::Bundle::try_from(item.payload.clone())
            .map_err(|_| omemo_xml::Error::Malformed("bundle does not parse"))?;
        Ok(bundle::from_xml(&bundle)?)
    }

    /// Records a device's first identity key, with the trust it starts with (BTBV), and
    /// refuses a changed one.
    /// Whether `jid` lists `device`: the cached list, else the list read afresh.
    async fn listed(&self, state: &mut State, jid: &BareJid, device: DeviceId) -> Result<bool, Error> {
        if state
            .device_lists
            .get(jid)
            .is_some_and(|list| list.contains(&device))
        {
            return Ok(true);
        }
        let list = match self.account.items(jid, DEVICE_LIST_NODE).await {
            Ok(items) => parse_device_list(&items),
            // No list at all: no device is listed.
            Err(omtalk_xmpp::Error::Stanza(e))
                if e.defined_condition == omtalk_xmpp::stanza_error::DefinedCondition::ItemNotFound =>
            {
                BTreeSet::new()
            }
            Err(e) => return Err(e.into()),
        };
        let listed = list.contains(&device);
        state.device_lists.insert(jid.clone(), list);
        Ok(listed)
    }

    fn record_identity(
        &self,
        state: &mut State,
        jid: &BareJid,
        device: DeviceId,
        key: &[u8; 32],
    ) -> Result<(), Error> {
        match state.check_identity(jid, device, key) {
            Identity::Known => Ok(()),
            Identity::New => {
                self.persist.identity(jid, device, key)?;
                let trust = state.first_trust(jid);
                if trust != Trust::Blind {
                    state.trust.insert((jid.clone(), device), trust);
                    self.persist.trust(jid, device, trust)?;
                }
                Ok(())
            }
            Identity::Changed => Err(Error::IdentityChanged {
                jid: jid.clone(),
                device,
            }),
        }
    }

    fn encrypt_for(
        &self,
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
        let encrypted = message::encrypt(sender_device, text.as_bytes(), recipients)?;
        // Every one of these sessions moved a step.
        for (jid, device) in targets {
            self.persist
                .session(jid, *device, &state.sessions[&(jid.clone(), *device)])?;
        }
        Ok(encrypted)
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

    /// Decides about a device: verified, refused, or back to blind trust.
    pub async fn set_trust(&self, jid: &BareJid, device: DeviceId, trust: Trust) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        state.trust.insert((jid.clone(), device), trust);
        self.persist.trust(jid, device, trust)?;
        Ok(())
    }

    /// Takes an old device of one's own off the account's device list, so that nobody
    /// encrypts for it any more. Not this one. A device still in use puts itself back.
    pub async fn remove_own_device(&self, device: DeviceId) -> Result<(), Error> {
        if device == self.device_id {
            return Err(Error::ThisDevice);
        }
        let own = self.account.jid().clone();
        let mut list = own_device_list(&self.account.items(&own, DEVICE_LIST_NODE).await?)?;
        if list.remove(&device) {
            let payload: Element = device_list::to_xml(list.iter().copied()).into();
            self.account
                .publish(DEVICE_LIST_NODE, Some("current"), payload, Access::Open)
                .await?;
        }
        let mut state = self.state.lock().await;
        state.device_lists.insert(own.clone(), list);
        state.sessions.remove(&(own, device));
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

/// Messages reach every device of an account, but some are addressed to only one of
/// them: the answer that completes another device's key exchange, for instance. Not
/// being a recipient is normal.
///
/// Copies of one message are normal too: a message to the own account arrives directly
/// and as a carbon, and the archive repeats live messages. The ratchet refuses the
/// second copy; nothing is lost.
fn ignorable(result: Result<Incoming, Error>) -> Result<Option<Incoming>, Error> {
    match result {
        Err(Error::Omemo(omemo_xml::Error::NotForThisDevice))
        | Err(Error::Omemo(omemo_xml::Error::Omemo(omemo::Error::DuplicateMessage))) => Ok(None),
        other => other.map(Some),
    }
}

/// The own device list, read strictly: no item is an empty list, an item that is not a
/// list is an error. What is published back must keep every device that was there.
fn own_device_list(items: &[PepItem]) -> Result<BTreeSet<DeviceId>, Error> {
    match items.first() {
        None => Ok(BTreeSet::new()),
        Some(item) => xml::DeviceList::try_from(item.payload.clone())
            .map(|list| device_list::from_xml(&list))
            .map_err(|_| Error::UnreadableOwnList),
    }
}

/// Someone else's device list; one that cannot be read is no devices to encrypt for.
fn parse_device_list(items: &[PepItem]) -> BTreeSet<DeviceId> {
    items
        .first()
        .and_then(|item| xml::DeviceList::try_from(item.payload.clone()).ok())
        .map(|list| device_list::from_xml(&list))
        .unwrap_or_default()
}

fn with(mut message: Message, extra: Vec<Element>) -> Message {
    message.payloads.extend(extra);
    message
}

/// An OMEMO message. One with content (text, under the id given) says so twice for clients that cannot
/// read it: the encryption mark (XEP-0380) and a fallback body. A key exchange answer or
/// heartbeat carries neither: it reaches every device of the account but concerns one,
/// and the mark alone makes the others show "could not be decrypted" for nothing.
fn stanza(type_: MessageType, to: BareJid, encrypted: xml::Encrypted, content: Option<&str>) -> Message {
    let mut message = Message::new_with_type(type_, Some(to.into())).with_payload(encrypted);
    // Archive it (MAM) and keep it for offline delivery even without a body.
    message
        .payloads
        .push(Element::builder("store", "urn:xmpp:hints").build());
    if let Some(id) = content {
        omtalk_xmpp::receipts::mark_outgoing(&mut message, id);
        message = message
            .with_payload(ExplicitMessageEncryption {
                namespace: omemo::NAMESPACE.into(),
                name: Some("OMEMO".into()),
            })
            .with_body(Lang::default(), FALLBACK_BODY.into());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_own_device_list_is_read_strictly_or_not_at_all() {
        let item = |payload: &str| PepItem {
            id: Some("current".into()),
            payload: payload.parse().unwrap(),
        };
        assert_eq!(
            own_device_list(&[]).unwrap(),
            BTreeSet::new(),
            "no item: no devices yet"
        );
        let list =
            item(r#"<list xmlns="eu.siacs.conversations.axolotl"><device id="1"/><device id="2"/></list>"#);
        assert_eq!(own_device_list(&[list]).unwrap(), BTreeSet::from([1, 2]));
        let other = item(r#"<something xmlns="urn:example"/>"#);
        assert!(
            matches!(
                own_device_list(std::slice::from_ref(&other)),
                Err(Error::UnreadableOwnList)
            ),
            "not a list: nothing to publish over"
        );
        assert!(
            parse_device_list(&[other]).is_empty(),
            "someone else's: just no devices"
        );
    }

    fn encrypted(payload: bool) -> xml::Encrypted {
        xml::Encrypted {
            header: xml::Header {
                sid: 1,
                keys: Vec::new(),
                iv: xml::IV { data: vec![0; 12] },
            },
            payload: payload.then(|| xml::Payload { data: vec![1, 2, 3] }),
        }
    }

    #[test]
    fn only_messages_with_text_carry_the_encryption_mark_and_fallback() {
        let to: BareJid = "bob@example.org".parse().unwrap();
        let eme = |m: &Message| m.payloads.iter().any(|p| p.is("encryption", "urn:xmpp:eme:0"));
        let text = stanza(MessageType::Chat, to.clone(), encrypted(true), Some("m1"));
        assert!(eme(&text) && !text.bodies.is_empty());
        let empty = stanza(MessageType::Chat, to, encrypted(false), None);
        assert!(!eme(&empty) && empty.bodies.is_empty(), "{empty:?}");
        assert!(
            empty.payloads.iter().any(|p| p.is("store", "urn:xmpp:hints")),
            "still kept for offline"
        );
    }
}
