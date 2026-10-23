//! Who wrote a message must come from a room one is in, never from the writer: a member
//! cannot write into a room's archive as someone else, and a stranger cannot make up a
//! room and speak as one of its members. (From an outside review.)

use std::time::Duration;

use omemo::{OwnKeys, Session};
use omemo_xml::{bundle, bundle_node, message, xml};
use omtalk_omemo::{Direction, Omemo, State};
use omtalk_testkit::{Device, converge, sees, stop_all, text};
use omtalk_xmpp::{
    Account, AccountConfig, AccountEvent, Accounts, Affiliation, BareJid, Element, Event, FullJid, Server,
};
use tokio::sync::mpsc::UnboundedReceiver;
use xmpp_parsers::message::{Lang, Message, MessageType};
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::presence::Presence;

/// A room message carrying its own `<x muc#user><item jid=…/>`, naming `jid` as writer.
fn claiming(room: &BareJid, body: &str, jid: &BareJid) -> Message {
    let mut message = Message::groupchat(Some(room.clone().into())).with_body(Lang::default(), body.into());
    message.payloads.push(item_naming(jid.as_str()));
    message
}

fn item_naming(jid: &str) -> Element {
    let item = Element::builder("item", "http://jabber.org/protocol/muc#user")
        .attr(xml_ncname!("jid").into(), jid)
        .attr(xml_ncname!("affiliation").into(), "member")
        .attr(xml_ncname!("role").into(), "participant")
        .build();
    Element::builder("x", "http://jabber.org/protocol/muc#user")
        .append(item)
        .build()
}

async fn room_of(owner: &Device, name: &str, members: &[&BareJid]) -> BareJid {
    let room: BareJid = format!("{name}-{}@muc.localhost", owner.omemo.device_id())
        .parse()
        .unwrap();
    owner.account.join(&room, "owner").await.unwrap();
    owner.account.make_private(&room).await.unwrap();
    for member in members {
        owner
            .account
            .set_affiliation(&room, member, Affiliation::Member)
            .await
            .unwrap();
    }
    room
}

#[tokio::test(flavor = "multi_thread")]
async fn a_member_cannot_write_as_someone_else_into_the_room_archive() {
    let (bob, mallory, carol) =
        tokio::join!(Device::start("u66"), Device::start("u67"), Device::start("u68"));
    let room = room_of(&bob, "spoof", &[&mallory.jid, &carol.jid]).await;
    mallory.account.join(&room, "mallory").await.unwrap();
    carol.account.join(&room, "carol").await.unwrap();
    sees(&bob, &room, &["carol", "mallory", "owner"]).await;

    for (body, who) in [("written as carol", &carol.jid), ("written as bob", &bob.jid)] {
        mallory.account.send(claiming(&room, body, who)).await.unwrap();
    }
    tokio::time::sleep(Duration::from_secs(1)).await;

    // A new device of bob's enters and reads the room's past from the archive.
    let laptop = Device::start("u66").await;
    laptop.account.join(&room, "bob-laptop").await.unwrap();
    let caught = laptop.omemo.catch_up_room(&room).await.unwrap();
    let said: Vec<_> = caught
        .messages
        .iter()
        .filter(|m| text(m).starts_with("written as"))
        .map(|m| (text(m).to_owned(), m.sender.clone(), m.direction))
        .collect();
    assert_eq!(said.len(), 2, "{said:?}");
    // The room adds its own item but keeps the writer's: with two, which is the room's
    // cannot be told, and the writer is not known (the nick the room gives still shows).
    // What matters: never someone else, never oneself.
    for (body, sender, direction) in &said {
        assert!(
            sender != &carol.jid && sender != &bob.jid,
            "{body:?} is said to be {sender}'s, but mallory wrote it"
        );
        assert_eq!(*direction, Direction::Incoming, "{body:?} shows as one's own");
    }
    stop_all([laptop, bob, mallory, carol]).await;
}

/// The same with OMEMO: mallory's own device and keys, the archive names carol. A device
/// reading it from the archive must not take mallory's key for one of carol's devices.
#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_message_in_the_archive_cannot_borrow_another_account() {
    let (bob, mallory, carol) =
        tokio::join!(Device::start("u69"), Device::start("u70"), Device::start("u71"));
    // Bob's second device exists (and is listed) before the message, but is not in the room.
    let laptop = Device::start("u69").await;
    converge(&[&bob, &laptop]).await;
    let room = room_of(&bob, "spoof-omemo", &[&mallory.jid, &carol.jid]).await;
    mallory.account.join(&room, "mallory").await.unwrap();
    carol.account.join(&room, "carol").await.unwrap();
    sees(&mallory, &room, &["carol", "mallory", "owner"]).await;

    let fake = item_naming(carol.jid.as_str());
    mallory
        .omemo
        .send_group_with(&room, "encrypted, written as carol", "spoof-1", vec![fake])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;

    laptop.account.join(&room, "bob-laptop").await.unwrap();
    let caught = laptop.omemo.catch_up_room(&room).await.unwrap();
    for m in caught.messages.iter().filter(|m| m.text.is_some()) {
        assert_eq!(
            m.sender,
            mallory.jid,
            "{:?} is said to be {}'s",
            text(m),
            m.sender
        );
    }
    let carols = laptop
        .omemo
        .with_state(|s| {
            s.identities
                .keys()
                .filter(|(jid, _)| *jid == carol.jid)
                .map(|(_, d)| *d)
                .collect::<Vec<_>>()
        })
        .await;
    assert!(
        !carols.contains(&mallory.omemo.device_id()),
        "carol's devices now include mallory's: {carols:?}"
    );
    stop_all([laptop, bob, mallory, carol]).await;
}

async fn login(
    user: &str,
    resource: Option<&str>,
) -> (Accounts, UnboundedReceiver<AccountEvent>, Account, FullJid) {
    let mut config = AccountConfig::new(format!("{user}@localhost").parse().unwrap(), "test");
    if let Some(r) = resource {
        config.resource = r.into();
    }
    config.server = Server::Plaintext {
        addr: "127.0.0.1:15222".into(),
    };
    config.iq_timeout = Duration::from_secs(5);
    let (mut accounts, mut events) = Accounts::new();
    let account = accounts.add(config).await;
    loop {
        if let Event::Online { jid, .. } = events.recv().await.unwrap().event {
            return (accounts, events, account, jid);
        }
    }
}

/// Only a room one is in may say who its occupants are. Here a plain account (no room
/// anywhere) sends a directed presence with a muc#user item naming carol, then a
/// groupchat message, encrypted with throwaway keys, from that full JID to bob's client.
#[tokio::test(flavor = "multi_thread")]
async fn a_stranger_cannot_make_up_a_room_and_speak_as_carol_in_it() {
    let carol: BareJid = "u62@localhost".parse().unwrap();
    let (_b, mut bob_events, bob_account, bob_full) = login("u63", None).await;
    let bob = Omemo::new(bob_account.clone(), State::new(OwnKeys::generate(0).unwrap()));
    bob.announce().await.unwrap();
    bob.catch_up().await.unwrap();
    let (_m, _mallory_events, mallory, _) = login("u61", Some("carol")).await;

    // "I am carol, in the room u61@localhost."
    let mut presence = Presence::available().with_to(bob_full.clone());
    presence.payloads.push(item_naming("u62@localhost/phone"));
    mallory.send(presence).await.unwrap();

    // Throwaway keys, a session from bob's published bundle.
    let keys = OwnKeys::generate(0).unwrap();
    let items = mallory
        .items(bob_account.jid(), &bundle_node(bob.device_id()))
        .await
        .unwrap();
    let parsed = bundle::from_xml(&xml::Bundle::try_from(items[0].payload.clone()).unwrap()).unwrap();
    let mut session = Session::initiate(&keys, &parsed).unwrap();
    let encrypted = message::encrypt(
        keys.device_id,
        b"hi, it's carol",
        [(bob.device_id(), &mut session)],
    )
    .unwrap();
    let mut spoof = Message::new_with_type(MessageType::Groupchat, Some(bob_full.clone().into()));
    spoof.payloads.push(encrypted.into());
    mallory.send(spoof).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = bob_events.recv().await.unwrap();
            match bob.handle(&event.event).await {
                Ok(Some(m)) if m.text.is_some() => return Ok(m),
                Ok(_) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    })
    .await;
    let recorded = bob
        .with_state(|s| s.identities.contains_key(&(carol.clone(), keys.device_id)))
        .await;
    mallory.stop().await;
    bob_account.stop().await;
    if let Ok(Ok(m)) = got {
        assert_ne!(m.sender, carol, "a stranger's message shows as carol's: {m:?}");
    }
    assert!(!recorded, "carol's devices now include a key mallory made up");
}

/// A device never seen before is taken at once, as Gajim and Conversations take it, without
/// asking for its account's list first: the server vouches for the account it came from.
/// One made up on the spot, with throwaway keys, speaks only for that account.
#[tokio::test(flavor = "multi_thread")]
async fn a_made_up_device_speaks_only_for_its_own_account() {
    let (_b, mut bob_events, bob_account, bob_full) = login("u72", None).await;
    let bob = Omemo::new(bob_account.clone(), State::new(OwnKeys::generate(0).unwrap()));
    bob.announce().await.unwrap();
    bob.catch_up().await.unwrap();
    let (_m, _mallory_events, mallory, _) = login("u73", None).await;

    let keys = OwnKeys::generate(0).unwrap();
    let items = mallory
        .items(bob_account.jid(), &bundle_node(bob.device_id()))
        .await
        .unwrap();
    let parsed = bundle::from_xml(&xml::Bundle::try_from(items[0].payload.clone()).unwrap()).unwrap();
    let mut session = Session::initiate(&keys, &parsed).unwrap();
    let encrypted =
        message::encrypt(keys.device_id, b"from nowhere", [(bob.device_id(), &mut session)]).unwrap();
    let mut message = Message::new_with_type(MessageType::Chat, Some(bob_full.clone().into()));
    message.payloads.push(encrypted.into());
    mallory.send(message).await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = bob_events.recv().await.unwrap();
            match bob.handle(&event.event).await {
                Ok(Some(m)) if m.text.is_some() => return Ok(m),
                Ok(_) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    })
    .await
    .expect("an answer");
    let mallory_jid: BareJid = "u73@localhost".parse().unwrap();
    let elsewhere = bob
        .with_state(|s| {
            s.identities
                .keys()
                .any(|(jid, device)| *device == keys.device_id && *jid != mallory_jid)
        })
        .await;
    mallory.stop().await;
    bob_account.stop().await;
    let message = got.expect("taken, as other clients take it");
    assert_eq!(message.sender, mallory_jid, "shown as the account it came from");
    assert_eq!(text(&message), "from nowhere");
    assert!(!elsewhere, "the made-up key belongs to no other account");
}
