//! Against a real server: the disposable Prosody from `tests/prosody` (`mise run prosody`).
//!
//! Every test uses its own accounts, so tests running in parallel do not see each
//! other's PEP items.

use std::time::Duration;

use omemo::OwnKeys;
use omemo_xml::{DEVICE_LIST_NODE, bundle, bundle_node, device_list, xml};
use omtalk_xmpp::{
    Access, Account, AccountConfig, AccountEvent, Accounts, Affiliation, BareJid, Element, Event, Jid, Server,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

fn config(user: &str) -> AccountConfig {
    let mut config = AccountConfig::new(format!("{user}@localhost").parse().unwrap(), "test");
    config.server = Server::Plaintext {
        addr: "127.0.0.1:15222".into(),
    };
    config.iq_timeout = Duration::from_secs(5);
    config
}

/// Waits for the first event that `pick` accepts, skipping the others.
async fn wait_for<T>(
    events: &mut UnboundedReceiver<AccountEvent>,
    mut pick: impl FnMut(&AccountEvent) -> Option<T>,
) -> T {
    let found = timeout(Duration::from_secs(20), async {
        loop {
            let event = events.recv().await.expect("event channel open");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    })
    .await;
    found.expect("no matching event within 20 s; is the test server up (`mise run prosody`)?")
}

/// Waits until all `accounts` are online, in whatever order their events come.
async fn online(events: &mut UnboundedReceiver<AccountEvent>, accounts: &[&Account]) -> Vec<Jid> {
    let mut found: Vec<Option<Jid>> = vec![None; accounts.len()];
    while found.iter().any(Option::is_none) {
        let (index, jid) = wait_for(events, |e| match &e.event {
            Event::Online { jid, .. } => accounts
                .iter()
                .position(|a| a.jid() == &e.account)
                .map(|index| (index, Jid::from(jid.clone()))),
            _ => None,
        })
        .await;
        found[index] = Some(jid);
    }
    found.into_iter().map(Option::unwrap).collect()
}

#[tokio::test]
async fn several_accounts_connect_and_their_events_are_tagged() {
    let (mut accounts, mut events) = Accounts::new();
    let alice = accounts.add(config("alice")).await;
    let bob = accounts.add(config("bob")).await;

    let mut seen: Vec<BareJid> = Vec::new();
    while seen.len() < 2 {
        let account = wait_for(&mut events, |e| match &e.event {
            Event::Online { jid, resumed: false } => {
                assert_eq!(jid.to_bare(), e.account, "event tagged with the wrong account");
                Some(e.account.clone())
            }
            _ => None,
        })
        .await;
        seen.push(account);
    }
    seen.sort();
    assert_eq!(seen, [alice.jid().clone(), bob.jid().clone()]);

    accounts.remove(alice.jid()).await;
    assert!(accounts.get(alice.jid()).is_none());
    assert!(
        alice.items(bob.jid(), DEVICE_LIST_NODE).await.is_err(),
        "a stopped account must refuse work"
    );
}

#[tokio::test]
async fn omemo_nodes_are_published_open_and_readable_by_others() {
    let (mut accounts, mut events) = Accounts::new();
    let carol = accounts.add(config("carol")).await;
    let dave = accounts.add(config("dave")).await;
    online(&mut events, &[&carol, &dave]).await;

    let keys = OwnKeys::generate(0).unwrap();
    carol
        .publish(
            DEVICE_LIST_NODE,
            Some("current"),
            device_list::to_xml([keys.device_id]).into(),
            Access::Open,
        )
        .await
        .unwrap();
    carol
        .publish(
            &bundle_node(keys.device_id),
            Some("current"),
            bundle::to_xml(&keys.bundle()).into(),
            Access::Open,
        )
        .await
        .unwrap();

    // Dave has no presence subscription to Carol: only an open node lets him read it.
    let items = dave.items(carol.jid(), DEVICE_LIST_NODE).await.unwrap();
    let list = xml::DeviceList::try_from(items[0].payload.clone()).unwrap();
    assert_eq!(
        device_list::from_xml(&list).into_iter().collect::<Vec<_>>(),
        [keys.device_id]
    );

    let items = dave
        .items(carol.jid(), &bundle_node(keys.device_id))
        .await
        .unwrap();
    let fetched = bundle::from_xml(&xml::Bundle::try_from(items[0].payload.clone()).unwrap()).unwrap();
    assert_eq!(fetched, keys.bundle());
}

#[tokio::test]
async fn own_other_device_is_notified_of_device_list_changes() {
    // Two clients of one account, as on two machines: separate `Accounts`.
    let (mut laptop_accounts, mut laptop_events) = Accounts::new();
    let (mut phone_accounts, mut phone_events) = Accounts::new();
    let laptop = laptop_accounts.add(config("erin")).await;
    let phone = phone_accounts.add(config("erin")).await;
    online(&mut laptop_events, &[&laptop]).await;
    online(&mut phone_events, &[&phone]).await;

    // The server sends notifications only if our caps say `+notify`; that takes a
    // disco round trip from the server to the phone, which the client must answer.
    let device_id = 1_234_567;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let notified = loop {
        laptop
            .publish(
                DEVICE_LIST_NODE,
                Some("current"),
                device_list::to_xml([device_id]).into(),
                Access::Open,
            )
            .await
            .unwrap();
        let got = timeout(Duration::from_millis(500), phone_events.recv()).await;
        if let Ok(Some(AccountEvent {
            event: Event::Pep {
                from, node, items, ..
            },
            ..
        })) = got
            && node == DEVICE_LIST_NODE
        {
            break (from, items);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the phone never got a device list notification"
        );
    };
    assert_eq!(notified.0, *laptop.jid());
    let list = xml::DeviceList::try_from(notified.1[0].payload.clone()).unwrap();
    assert!(device_list::from_xml(&list).contains(&device_id));
}

#[tokio::test]
async fn client_answers_disco_and_ping() {
    let (mut accounts, mut events) = Accounts::new();
    let frank = accounts.add(config("frank")).await;
    let frank_full = online(&mut events, &[&frank]).await.remove(0);
    let (mut others, mut other_events) = Accounts::new();
    let asker = others.add(config("frank")).await;
    online(&mut other_events, &[&asker]).await;

    let disco: Element = xmpp_parsers::disco::DiscoInfoQuery { node: None }.into();
    let answer = asker.get(Some(frank_full.clone()), disco).await.unwrap().unwrap();
    let info = xmpp_parsers::disco::DiscoInfoResult::try_from(answer).unwrap();
    assert!(
        info.features
            .contains("eu.siacs.conversations.axolotl.devicelist+notify")
    );

    let ping: Element = xmpp_parsers::ping::Ping.into();
    assert!(asker.get(Some(frank_full.clone()), ping).await.is_ok());

    // Anything else is refused, not left unanswered.
    let unknown = Element::builder("query", "urn:example:unknown").build();
    assert!(matches!(
        asker.get(Some(frank_full), unknown).await,
        Err(omtalk_xmpp::Error::Stanza(_))
    ));
}

#[tokio::test]
async fn wrong_password_is_reported_and_stops_the_account() {
    let (mut accounts, mut events) = Accounts::new();
    let mut wrong = config("u38");
    wrong.password = "not the password".into();
    let account = accounts.add(wrong).await;

    let reason = wait_for(&mut events, |e| match &e.event {
        Event::AuthFailed { reason } => Some(reason.clone()),
        Event::Online { .. } => panic!("logged in with a wrong password"),
        _ => None,
    })
    .await;
    assert!(
        reason.contains("NotAuthorized"),
        "reason names the server's answer: {reason}"
    );
    assert!(
        matches!(
            account.items(account.jid(), DEVICE_LIST_NODE).await,
            Err(omtalk_xmpp::Error::Stopped)
        ),
        "a refused account must not keep retrying in the background"
    );
}

#[tokio::test]
async fn unreachable_server_is_reported_and_retried() {
    let (mut accounts, mut events) = Accounts::new();
    let mut nowhere = config("u37");
    nowhere.server = Server::Plaintext {
        addr: "127.0.0.1:1".into(),
    };
    accounts.add(nowhere).await;
    wait_for(&mut events, |e| match &e.event {
        Event::Disconnected { .. } => Some(()),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn own_archive_is_read_page_by_page() {
    let (mut accounts, mut events) = Accounts::new();
    let reader = accounts.add(config("u35")).await;
    let writer = accounts.add(config("u34")).await;
    online(&mut events, &[&reader, &writer]).await;

    let start = reader.archive_newest(None).await.unwrap();
    let texts: Vec<String> = (0..5)
        .map(|i| format!("archived {i} {}", rand_suffix()))
        .collect();
    for text in &texts {
        let message = xmpp_parsers::message::Message::chat(Some(reader.jid().clone().into()))
            .with_body(xmpp_parsers::message::Lang::default(), text.clone());
        writer.send(message).await.unwrap();
    }
    // The server archives as it delivers; wait until the last one arrived.
    let last = texts.last().unwrap().clone();
    wait_for(&mut events, |e| match &e.event {
        Event::Message(m) if m.get_best_body(vec![]).is_some_and(|(_, b)| *b == last) => Some(()),
        _ => None,
    })
    .await;

    // Two at a time, from where we started, until the archive says it is complete.
    let mut after = start;
    let mut read = Vec::new();
    loop {
        let page = reader.archive_after(None, after.as_deref(), 2).await.unwrap();
        assert!(page.messages.len() <= 2);
        read.extend(
            page.messages
                .iter()
                .filter_map(|m| m.message.get_best_body(vec![]).map(|(_, b)| b.clone())),
        );
        after = page.last.or(after);
        if page.complete {
            break;
        }
    }
    assert_eq!(
        read, texts,
        "every message once, in order, nothing from before the start"
    );
    assert_eq!(reader.archive_newest(None).await.unwrap(), after);
}

fn rand_suffix() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
}

#[tokio::test]
async fn node_created_with_other_access_is_reconfigured_on_publish() {
    let (mut accounts, mut events) = Accounts::new();
    let owner = accounts.add(config("u33")).await;
    let stranger = accounts.add(config("u32")).await;
    online(&mut events, &[&owner, &stranger]).await;

    // Another client made the node earlier, readable by contacts only.
    let node = format!("urn:omtalk:test:{}", rand_suffix());
    let payload = || Element::builder("note", "urn:omtalk:test").build();
    owner
        .publish(&node, Some("current"), payload(), Access::Presence)
        .await
        .unwrap();
    assert!(
        stranger.items(owner.jid(), &node).await.is_err(),
        "the node starts closed to strangers"
    );

    // Publishing with other options than the node has must not fail: OMEMO needs open
    // nodes whatever created them.
    owner
        .publish(&node, Some("current"), payload(), Access::Open)
        .await
        .unwrap();
    let items = stranger.items(owner.jid(), &node).await.unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn contacts_ask_accept_and_then_see_each_other() {
    let (mut accounts, mut events) = Accounts::new();
    let alice = accounts.add(config("u28")).await;
    let bob = accounts.add(config("u29")).await;
    online(&mut events, &[&alice, &bob]).await;
    // The server keeps rosters across runs; start as strangers.
    let _ = alice.remove_contact(bob.jid()).await;
    let _ = bob.remove_contact(alice.jid()).await;

    alice.add_contact(bob.jid(), Some("Bob")).await.unwrap();
    // The server pushes the new item to Alice's clients; this one acknowledges it.
    wait_for(&mut events, |e| match &e.event {
        Event::Roster(contacts) if e.account == *alice.jid() => contacts
            .iter()
            .find(|c| c.jid == *bob.jid() && !c.removed)
            .map(|_| ()),
        _ => None,
    })
    .await;
    let asker = wait_for(&mut events, |e| match &e.event {
        Event::SubscriptionRequest { from } if e.account == *bob.jid() => Some(from.clone()),
        _ => None,
    })
    .await;
    assert_eq!(asker, *alice.jid());

    // Accepting also asks back, so Alice is asked in turn.
    bob.accept_contact(alice.jid()).await.unwrap();
    wait_for(&mut events, |e| match &e.event {
        Event::SubscriptionRequest { from } if e.account == *alice.jid() && from == bob.jid() => Some(()),
        _ => None,
    })
    .await;
    alice.accept_contact(bob.jid()).await.unwrap();

    // Each now sees the other online, which is what made Gajim show omtalk grey before.
    wait_for(&mut events, |e| match &e.event {
        Event::Presence(p)
            if e.account == *alice.jid()
                && p.type_ == xmpp_parsers::presence::Type::None
                && p.from.as_ref().is_some_and(|f| f.to_bare() == *bob.jid()) =>
        {
            Some(())
        }
        _ => None,
    })
    .await;
    let contact = timeout(Duration::from_secs(10), async {
        loop {
            let roster = alice.roster().await.unwrap();
            if let Some(c) = roster
                .into_iter()
                .find(|c| c.jid == *bob.jid() && c.we_see_them && c.they_see_us)
            {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the roster shows a mutual subscription");
    assert_eq!(contact.name.as_deref(), Some("Bob"));
}

#[tokio::test]
async fn members_only_room_refuses_strangers_and_admits_members() {
    let (mut accounts, mut events) = Accounts::new();
    let owner = accounts.add(config("u27")).await;
    let guest = accounts.add(config("u26")).await;
    online(&mut events, &[&owner, &guest]).await;
    let room: BareJid = format!("closed-{}@muc.localhost", rand_suffix()).parse().unwrap();
    owner.join(&room, "owner").await.unwrap();
    owner.make_private(&room).await.unwrap();

    match guest.join(&room, "guest").await {
        Err(omtalk_xmpp::Error::Stanza(error)) => assert_eq!(
            error.defined_condition,
            xmpp_parsers::stanza_error::DefinedCondition::RegistrationRequired,
            "refused as a non-member, and told so"
        ),
        other => panic!("a stranger got in or no answer: {other:?}"),
    }
    owner
        .set_affiliation(&room, guest.jid(), Affiliation::Member)
        .await
        .unwrap();
    guest.join(&room, "guest").await.unwrap();
}

#[tokio::test]
async fn services_are_discovered_and_commands_run_step_by_step() {
    use omtalk_xmpp::services::{CommandAction, CommandStatus};
    let (mut accounts, mut events) = Accounts::new();
    let user = accounts.add(config("u39")).await;
    let admin = accounts.add(config("admin")).await;
    online(&mut events, &[&user, &admin]).await;
    let server: Jid = "localhost".parse().unwrap();

    // What the server offers: the group chat service among others.
    let items = user.disco_items(&server, None).await.unwrap();
    let muc = items
        .iter()
        .find(|i| i.jid.as_str() == "muc.localhost")
        .expect("muc.localhost is listed");
    let info = user.disco_info(&muc.jid, None).await.unwrap();
    assert!(info.identities.iter().any(|i| i.category == "conference"));

    // A plain user is offered the commands open to everyone, not the administration.
    let commands = user.commands(&server).await.unwrap();
    let nodes: Vec<&str> = commands.iter().filter_map(|c| c.node.as_deref()).collect();
    assert!(
        nodes.contains(&"uptime") && nodes.contains(&"urn:xmpp:invite#invite"),
        "{nodes:?}"
    );
    assert!(!nodes.iter().any(|n| n.contains("admin#")), "{nodes:?}");
    let uptime = user
        .execute(&server, "uptime", None, CommandAction::Execute, None)
        .await
        .unwrap();
    assert_eq!(uptime.status, CommandStatus::Completed);
    assert!(
        uptime.notes.iter().any(|n| n.text.contains("running for")),
        "{:?}",
        uptime.notes
    );

    // One step, done: the admin lists the server's modules.
    let modules = admin
        .execute(
            &server,
            "http://prosody.im/protocol/modules#list",
            None,
            CommandAction::Execute,
            None,
        )
        .await
        .unwrap();
    assert_eq!(modules.status, CommandStatus::Completed);
    let listed = modules.form.expect("the result comes as a form");
    assert!(
        listed
            .fields
            .iter()
            .any(|f| f.values.iter().any(|v| v.contains("adhoc"))),
        "{listed:?}"
    );

    // An administration command: a form first, filled and sent back, then the result.
    let admin_nodes: Vec<_> = admin
        .commands(&server)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|c| c.node)
        .collect();
    let roster_node = admin_nodes
        .iter()
        .find(|n| n.ends_with("#get-user-roster"))
        .unwrap_or_else(|| panic!("the admin is offered administration: {admin_nodes:?}"))
        .clone();
    let first = admin
        .execute(&server, &roster_node, None, CommandAction::Execute, None)
        .await
        .unwrap();
    assert_eq!(first.status, CommandStatus::Executing);
    let mut form = first.form.expect("a form to fill");
    let field = form
        .fields
        .iter_mut()
        .find(|f| f.var.as_deref() == Some("accountjid"))
        .expect("asks for the account");
    field.values = vec!["u39@localhost".into()];
    form.type_ = xmpp_parsers::data_forms::DataFormType::Submit;
    let done = admin
        .execute(
            &server,
            &roster_node,
            first.session.as_deref(),
            CommandAction::Complete,
            Some(form),
        )
        .await
        .unwrap();
    assert_eq!(done.status, CommandStatus::Completed, "notes: {:?}", done.notes);
}
