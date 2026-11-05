use crate::{
    state::{Effects, State},
    *,
};
use omtalk_core::{ConversationKind, MessageKind, MessageSource, Network};

fn config(mode: Mode) -> Config {
    Config {
        id: "test".into(),
        name: "Test".into(),
        mode,
        server: "localhost".into(),
        port: 6697,
        tls: true,
        ca_certificate: None,
        username: "login".into(),
        nick: "client".into(),
        realname: "omtalk".into(),
        sasl: Some(Sasl {
            username: "login".into(),
            password: "secret-never-in-debug".into(),
        }),
        server_password: None,
        selected_network: None,
    }
}
fn feed(state: &mut State, line: &str) -> Effects {
    state.receive(crate::wire::parse(line).unwrap())
}
fn sent(e: &Effects) -> Vec<String> {
    e.send.iter().map(|m| m.to_string()).collect()
}
fn ready() -> State {
    let mut c = config(Mode::Direct);
    c.sasl = None;
    let mut s = State::new(c.clone(), c.network());
    feed(
        &mut s,
        ":server CAP * LS :message-tags server-time batch echo-message draft/chathistory draft/read-marker",
    );
    feed(
        &mut s,
        ":server CAP * ACK :message-tags server-time batch echo-message draft/chathistory draft/read-marker",
    );
    feed(&mut s, ":server 001 client :welcome");
    s
}

#[test]
fn topic_author_numeric_is_metadata_not_a_chat_diagnostic() {
    let mut state = ready();
    let events = feed(&mut state, ":server 333 client #usue vAnGogh 1780992523").events;
    assert!(
        matches!(events.as_slice(), [Event::TopicAuthor { channel, nick, at, .. }]
        if channel == "#usue" && nick == "vAnGogh" && at.timestamp() == 1780992523)
    );
    assert!(
        feed(&mut state, ":server 333 client #usue someone invalid")
            .events
            .is_empty()
    );
}

#[test]
fn history_works_after_soju_removes_message_tags_for_a_legacy_network() {
    let mut s = ready();
    let removed = feed(&mut s, ":server CAP client DEL :message-tags");
    assert!(
        s.snapshot().capabilities.history,
        "soju still provides timestamp-based history without upstream message-tags"
    );
    assert!(
        !removed
            .events
            .iter()
            .any(|e| matches!(e, Event::HistoryError { .. }))
    );
    let mut e = Effects::default();
    s.queue_history(
        HistoryRequest {
            target: "#legacy".into(),
            kind: HistoryKind::Latest,
            cursor: None,
        },
        &mut e,
    );
    assert!(
        sent(&e)
            .iter()
            .any(|s| s == "CHATHISTORY LATEST #legacy * 100\r\n")
    );
    feed(&mut s, ":server BATCH +history chathistory #legacy");
    let e = feed(
        &mut s,
        "@batch=history;time=2026-11-05T12:00:00.000Z :alice!u@host PRIVMSG #legacy :unread before connecting",
    );
    assert!(e.events.iter().any(|e| matches!(e, Event::Message {
        message, source: MessageSource::CatchUp, ..
    } if message.server_id.is_none() && message.server_time.as_deref() == Some("2026-11-05T12:00:00.000Z"))));
    let e = feed(&mut s, ":server BATCH -history");
    assert!(
        e.events
            .iter()
            .any(|e| matches!(e, Event::HistoryDone { count: 1, .. }))
    );
}

#[test]
fn soju_pin_subscription_is_scoped_and_handles_live_and_get_responses() {
    let mut c = config(Mode::Soju);
    c.selected_network = Some("alpha".into());
    c.sasl = None;
    let mut s = State::new(c.clone(), c.network());
    feed(&mut s, ":s CAP * LS :draft/metadata-2 batch");
    feed(&mut s, ":s CAP * ACK :draft/metadata-2 batch");
    let e = feed(&mut s, ":s 001 client :welcome");
    assert!(sent(&e).iter().any(|l| l == "METADATA * SUB soju.im/pinned\r\n"));
    for (line, pinned) in [
        (":s METADATA #om soju.im/pinned * 1", true),
        (":s 761 client #om soju.im/pinned * 0", false),
    ] {
        assert!(
            matches!(&feed(&mut s, line).events[..], [Event::Pinned {target, pinned:p,..}] if target=="#om" && *p==pinned)
        );
    }
    feed(&mut s, ":s BATCH +old chathistory #om");
    assert!(
        feed(&mut s, "@batch=old :s METADATA #om soju.im/pinned * 1")
            .events
            .is_empty()
    );
    let mut direct = ready();
    feed(&mut direct, ":s CAP client ACK :draft/metadata-2");
    assert!(!direct.snapshot().capabilities.pin_sync);
}

#[test]
fn membership_events_preserve_server_time_and_do_not_turn_replay_into_presence() {
    let mut s = ready();
    feed(&mut s, ":client!u@h JOIN #om");
    for (command, text) in [
        ("JOIN #om", "JOIN alice"),
        ("PART #om :bye", "PART alice bye"),
        ("JOIN #om", "JOIN alice"),
        ("QUIT :gone", "QUIT alice gone"),
    ] {
        let e = feed(
            &mut s,
            &format!("@time=2026-11-05T12:34:56.789Z :alice!u@h {command}"),
        );
        assert!(e.events.iter().any(|e| matches!(e, Event::Presence { channel, text:t, at,.. }
            if channel=="#om" && t==text && at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)=="2026-11-05T12:34:56.789Z")));
    }
    feed(&mut s, ":server BATCH +old chathistory #om");
    let e = feed(
        &mut s,
        "@batch=old;time=2026-11-04T00:00:00.000Z :alice!u@h JOIN #om",
    );
    assert!(
        !e.events
            .iter()
            .any(|e| matches!(e, Event::Presence { .. } | Event::Participants { .. }))
    );
}

#[test]
fn fragmented_ls_authenticates_then_binds_before_cap_end_and_verifies_netid() {
    let c = config(Mode::Soju);
    let mut n = c.network();
    n.network = Network::Soju("42".into());
    let mut s = State::new(c, n);
    assert!(
        !sent(&Effects {
            send: s.start(),
            events: vec![]
        })
        .iter()
        .any(|l| l.contains("CAP END"))
    );
    assert!(
        feed(
            &mut s,
            ":soju CAP * LS * :server-time message-tags sasl=PLAIN,EXTERNAL"
        )
        .send
        .is_empty()
    );
    let e = feed(
        &mut s,
        ":soju CAP * LS :batch soju.im/bouncer-networks soju.im/bouncer-networks-notify",
    );
    assert_eq!(e.send.len(), 1);
    let e = feed(
        &mut s,
        ":soju CAP * ACK :server-time message-tags batch sasl soju.im/bouncer-networks soju.im/bouncer-networks-notify",
    );
    assert_eq!(sent(&e), ["AUTHENTICATE PLAIN\r\n"]);
    let e = feed(&mut s, "AUTHENTICATE +");
    assert_eq!(e.send.len(), 1);
    let e = feed(&mut s, ":soju 903 client :SASL success");
    assert_eq!(sent(&e), ["BOUNCER BIND 42\r\n", "CAP END\r\n"]);
    feed(&mut s, ":soju 001 savednick :welcome");
    assert!(!s.connected);
    feed(&mut s, ":soju 005 savednick BOUNCER_NETID=42 :supported");
    assert!(s.connected);
    assert_eq!(s.nick, "savednick");
    assert!(!format!("{:?}", s.config).contains("secret"));
    feed(
        &mut s,
        ":soju 005 savednick BOUNCER_NETID=wrong :late binding change",
    );
    assert!(s.fatal.is_some());
}

#[test]
fn auth_and_binding_fail_closed_with_no_plaintext_retry() {
    let mut c = config(Mode::Soju);
    c.tls = false;
    assert!(c.validate().is_err());
    c.tls = true;
    let mut s = State::new(c.clone(), c.network());
    feed(&mut s, ":s CAP * LS :batch");
    feed(&mut s, ":s CAP * ACK :batch");
    assert!(s.fatal.is_some());
    let mut n = c.network();
    n.network = Network::Soju("one".into());
    let mut s = State::new(c, n);
    s.registered = true;
    feed(&mut s, ":s 005 client BOUNCER_NETID=other :supported");
    assert!(s.fatal.is_some());
    assert!(!s.connected);
}

#[test]
fn sasl_exact_400_chunk_has_terminator_and_optional_cap_rejection_finishes() {
    let mut c = config(Mode::Direct);
    c.sasl.as_mut().unwrap().username = "u".into();
    c.sasl.as_mut().unwrap().password = "p".repeat(297);
    let mut s = State::new(c.clone(), c.network());
    feed(&mut s, ":s CAP * LS :sasl=PLAIN");
    feed(&mut s, ":s CAP * ACK :sasl");
    let e = feed(&mut s, "AUTHENTICATE +");
    assert_eq!(e.send.len(), 2);
    assert_eq!(sent(&e)[1], "AUTHENTICATE +\r\n");
    let mut c = config(Mode::Direct);
    c.sasl = None;
    let mut s = State::new(c.clone(), c.network());
    feed(&mut s, ":s CAP * LS :echo-message");
    assert_eq!(sent(&feed(&mut s, ":s CAP * NAK :echo-message")), ["CAP END\r\n"]);
}

#[test]
fn replay_nested_batches_keep_fractional_time_and_cannot_change_live_roster() {
    let mut s = ready();
    feed(&mut s, ":client!u@h JOIN #chan");
    feed(&mut s, ":s 353 client = #chan :@client +friend");
    feed(&mut s, ":s 366 client #chan :end");
    let request = HistoryRequest {
        target: "#chan".into(),
        kind: HistoryKind::Before,
        cursor: Some("timestamp=2026-11-05T12:01:00.123456789Z".into()),
    };
    s.queue_history(request, &mut Effects::default());
    feed(&mut s, ":s BATCH +h chathistory #chan");
    feed(&mut s, "@batch=h :s BATCH +inner labeled-response");
    assert!(
        feed(&mut s, "@batch=inner :friend!u@h PART #chan :historic")
            .events
            .iter()
            .all(|e| !matches!(e, Event::Participants { .. }))
    );
    let e = feed(
        &mut s,
        "@batch=inner;time=2026-11-05T12:00:00.123456789Z;msgid=Aa :friend!u@h PRIVMSG #chan :same",
    );
    let Event::Message { message, source, .. } = &e.events[0] else {
        panic!()
    };
    assert_eq!(*source, MessageSource::Older);
    assert_eq!(
        message.server_time.as_deref(),
        Some("2026-11-05T12:00:00.123456789Z")
    );
    assert_eq!(message.server_id.as_deref(), Some("Aa"));
    feed(&mut s, ":s BATCH -inner");
    let e = feed(&mut s, ":s BATCH -h");
    assert!(e.events.iter().any(|e| matches!(
        e,
        Event::HistoryDone {
            count: 2,
            exhausted: true,
            ..
        }
    )));
    let e = feed(&mut s, ":friend!u@h AWAY :later");
    assert!(e.events.iter().any(
        |e| matches!(e,Event::Participants{participants,..}if participants.iter().any(|p|p.nick=="friend"))
    ));
}

#[test]
fn ctcp_action_and_literal_me_have_different_intent() {
    let mut s = ready();
    for (body, expected, text) in [
        ("/me waves", MessageKind::Text, "/me waves"),
        ("\x01ACTION waves\x01", MessageKind::Action, "waves"),
    ] {
        let e = feed(&mut s, &format!(":friend!u@h PRIVMSG client :{body}"));
        let Event::Message { message, .. } = &e.events[0] else {
            panic!()
        };
        assert_eq!(message.kind, expected);
        assert_eq!(message.text, text);
        assert!(
            matches!(&message.conversation,omtalk_core::ConversationId::Irc{kind:ConversationKind::Direct,target,..}if target=="friend")
        );
    }
}

#[test]
fn custom_prefix_modes_and_names_part_race_do_not_resurrect_member() {
    let mut s = ready();
    feed(
        &mut s,
        ":s 005 client PREFIX=(Yohv)!@%+ CHANMODES=b,k,l,imn CASEMAPPING=ascii :supported",
    );
    feed(&mut s, ":client!u@h JOIN #c");
    feed(&mut s, ":s 353 client = #c :!client @friend");
    feed(&mut s, ":friend!u@h PART #c :bye");
    feed(&mut s, ":s 353 client = #c :@friend +other");
    feed(&mut s, ":s 366 client #c :end");
    let e = feed(&mut s, ":client!u@h MODE #c +kY key other");
    let people = e
        .events
        .iter()
        .find_map(|e| match e {
            Event::Participants { participants, .. } => Some(participants),
            _ => None,
        })
        .unwrap();
    assert!(!people.iter().any(|p| p.nick == "friend"));
    assert!(
        people
            .iter()
            .any(|p| p.nick == "other" && p.prefixes.contains('!'))
    );
}

#[test]
fn capability_removal_releases_pending_history_and_targets_use_valid_bounds() {
    let mut s = ready();
    let mut e = Effects::default();
    s.queue_history(
        HistoryRequest {
            target: String::new(),
            kind: HistoryKind::Targets,
            cursor: None,
        },
        &mut e,
    );
    assert!(!sent(&e)[0].contains(" * "));
    assert!(sent(&e)[0].contains("timestamp=1970-01-01"));
    let e = feed(&mut s, ":s CAP client DEL :draft/chathistory");
    assert!(s.history_pending.is_none());
    assert!(e.events.iter().any(|e| matches!(e, Event::HistoryError { .. })));
}

#[test]
fn senpai_end_tag_on_full_mixed_case_batch_stops_history_and_replay_is_inert() {
    let mut s = ready();
    s.support.history_limit = 2;
    s.queue_history(
        HistoryRequest {
            target: "#MixedCase".into(),
            kind: HistoryKind::Before,
            cursor: Some("timestamp=2026-11-05T12:01:00Z".into()),
        },
        &mut Effects::default(),
    );
    feed(
        &mut s,
        "@draft/chathistory-end :s BATCH +h chathistory #mixedcase",
    );
    feed(
        &mut s,
        "@batch=h;time=2026-11-05T12:00:00Z :friend!u@h TAGMSG #MixedCase",
    );
    feed(
        &mut s,
        "@batch=h;time=2026-11-05T12:00:01Z :friend!u@h PRIVMSG #MixedCase :hello",
    );
    assert!(
        feed(&mut s, "@batch=h :s 005 client CASEMAPPING=ascii :old")
            .events
            .is_empty()
    );
    assert!(
        feed(&mut s, "@batch=h :s ERROR :old disconnect")
            .events
            .is_empty()
    );
    assert!(!s.disconnected);
    assert_eq!(s.support.case_mapping, omtalk_core::CaseMapping::Rfc1459);
    let e = feed(&mut s, ":s BATCH -h");
    assert!(
        e.events
            .iter()
            .any(|e| matches!(e, Event::HistoryDone { exhausted: true, .. }))
    );
    assert!(s.history_pending.is_none());
}

#[test]
fn changing_casemap_rekeys_roster_and_names_does_not_undo_live_modes_or_away() {
    let mut s = ready();
    feed(&mut s, ":client!u@h JOIN #[Room]");
    feed(&mut s, ":s 353 client = #[Room] :@client +[Friend]");
    feed(&mut s, ":[Friend]!u@h AWAY :gone");
    feed(&mut s, ":s MODE #[Room] +o [Friend]");
    feed(&mut s, ":s 353 client = #[Room] :[Friend]");
    feed(&mut s, ":s 366 client #[Room] :end");
    feed(&mut s, ":s 005 client CASEMAPPING=ascii :supported");
    let e = feed(&mut s, ":[Friend]!u@h AWAY :still gone");
    let people = e
        .events
        .iter()
        .find_map(|e| match e {
            Event::Participants { participants, .. } => Some(participants),
            _ => None,
        })
        .unwrap();
    assert!(
        people
            .iter()
            .any(|p| p.nick == "[Friend]" && p.away && p.prefixes == "@+")
    );
    let e = feed(&mut s, ":[Friend]!u@h PART #[Room]");
    assert!(e.events.iter().any(|e|matches!(e,Event::Participants {participants,..} if !participants.iter().any(|p|p.nick=="[Friend]"))));
}

#[test]
fn statusmsg_and_irc_formatting_keep_correct_channel_and_literal_digits() {
    let mut s = ready();
    feed(&mut s, ":s 005 client STATUSMSG=@+ :supported");
    let e = feed(
        &mut s,
        ":friend!u@h PRIVMSG @#Room :\x0304client\x0f: 12,34\r\r\n",
    );
    let Event::Message { message, target, .. } = &e.events[0] else {
        panic!()
    };
    assert_eq!(target, "#Room");
    assert_eq!(message.text, "client: 12,34");
    assert!(matches!(
        &message.conversation,
        omtalk_core::ConversationId::Irc {
            kind: ConversationKind::Channel,
            ..
        }
    ));
    feed(&mut s, ":s 005 client -STATUSMSG -CASEMAPPING :supported");
    assert!(s.support.statusmsg.is_empty());
}

#[test]
fn discovery_batch_returns_complete_ids_and_partial_updates_exclude_secrets() {
    let mut s = ready();
    s.network.network = Network::Bouncer;
    s.caps.insert("soju.im/bouncer-networks".into());
    feed(&mut s, ":s BATCH +nets soju.im/bouncer-networks");
    let e = feed(
        &mut s,
        "@batch=nets :s BOUNCER NETWORK 4 :name=Net\\sFour;password=never;state=connected",
    );
    assert!(
        matches!(&e.events[0],Event::Network(n) if n.attributes.get("name")==Some(&Some("Net Four".into())) && !n.attributes.contains_key("password"))
    );
    let e = feed(&mut s, ":s BATCH -nets");
    assert!(
        matches!(&e.events[0],Event::NetworksListed{ids,..}if ids.iter().cloned().collect::<Vec<_>>()==["4"])
    );
    let e = feed(&mut s, ":s BOUNCER NETWORK 4 :state=disconnected");
    assert!(matches!(&e.events[0],Event::Network(n) if n.attributes.len()==1 && !n.removed));
}

#[test]
fn abandoned_history_is_not_live_and_unsupported_history_releases_the_queue() {
    let mut s = ready();
    for target in ["#one", "#two"] {
        s.queue_history(
            HistoryRequest {
                target: target.into(),
                kind: HistoryKind::Before,
                cursor: Some("timestamp=2026-11-05T00:00:00.000Z".into()),
            },
            &mut Effects::default(),
        );
    }
    feed(&mut s, ":s BATCH +old chathistory #one");
    s.fail_history("timeout", &mut Effects::default());
    assert!(
        feed(&mut s, "@batch=old :friend!u@h PRIVMSG #one :late history")
            .events
            .is_empty()
    );
    assert!(feed(&mut s, "@batch=old :friend!u@h JOIN #one").events.is_empty());
    let e = feed(&mut s, ":s 421 client CHATHISTORY :unsupported");
    assert!(!s.snapshot().capabilities.history);
    assert!(s.history_pending.is_none() && s.history_queue.is_empty());
    assert!(
        e.events
            .iter()
            .any(|e| matches!(e,Event::HistoryError{request,..}if request.target=="#two"))
    );
    assert!(
        matches!(&feed(&mut s,":irc.example NOTICE client :maintenance").events[..], [Event::Diagnostic{target:None,text,..}] if text=="maintenance")
    );
}

#[test]
fn history_failures_release_each_waiter_and_late_caps_restore_features() {
    let mut s = ready();
    for target in ["#a", "#b", "#c"] {
        s.queue_history(
            HistoryRequest {
                target: target.into(),
                kind: HistoryKind::Latest,
                cursor: None,
            },
            &mut Effects::default(),
        );
    }
    for target in ["#a", "#b", "#c"] {
        let mut e = Effects::default();
        s.fail_history("timeout", &mut e);
        assert!(
            e.events
                .iter()
                .any(|e| matches!(e,Event::HistoryError{request,..}if request.target==target))
        );
    }
    assert!(s.history_pending.is_none() && s.history_queue.is_empty());
    feed(&mut s, ":s CAP client DEL :echo-message");
    assert!(!s.snapshot().capabilities.echo);
    let e = feed(&mut s, ":s CAP client NEW :echo-message");
    assert!(sent(&e).iter().any(|s| s.contains("REQ echo-message")));
    feed(&mut s, ":s CAP client ACK :echo-message");
    assert!(s.snapshot().capabilities.echo);
}
