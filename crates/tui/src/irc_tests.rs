use super::*;

fn profile() -> omtalk_irc::Config {
    omtalk_irc::Config {
        id: "fixture".into(),
        name: "Fixture".into(),
        mode: omtalk_irc::Mode::Direct,
        server: "localhost".into(),
        port: 6667,
        tls: false,
        ca_certificate: None,
        username: "fixture".into(),
        nick: "fixture".into(),
        realname: "fixture".into(),
        sasl: None,
        server_password: None,
        selected_network: None,
    }
}

#[tokio::test]
async fn closed_private_chat_stays_closed_after_restore_and_history_but_reopens_for_new_message() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _) = shared(dir.path());
    let config = profile();
    let (mut app, _) = model(&shared, &config);
    let net = config.network();
    let i = app
        .ensure_irc_chat(&net, "friend", ConversationKind::Direct, true)
        .unwrap();
    let id = app.chats[i].jid.conversation().unwrap().clone();
    for action in app.close_chat(i) {
        assert!(local_action(&mut app, &action, &shared));
    }
    persist(&mut app, &shared, &mut BTreeMap::new());
    let mut restored = App::new(vec![], crate::config::Modifier::Ctrl, 42, 30);
    restored.notify.bell_on = crate::notify::When::NEVER;
    restore(&mut restored, &shared, &[config]);
    assert!(index(&restored, &id).is_none());
    apply(
        &mut restored,
        UiEvent::Wire(Event::HistoryTarget {
            network: net,
            target: "friend".into(),
            timestamp: "2026-11-05T12:00:00Z".into(),
        }),
        &shared,
    );
    let message = ChatMessage {
        conversation: id.clone(),
        id: MessageId("past".into()),
        server_id: Some("past".into()),
        server_time: Some("2026-11-05T12:00:00Z".into()),
        at: "2026-11-05T12:00:00Z".parse().unwrap(),
        sender: "friend".into(),
        outgoing: false,
        kind: MessageKind::Text,
        text: "history".into(),
        attachment: None,
    };
    apply(
        &mut restored,
        UiEvent::Wire(Event::Message {
            message: message.clone(),
            target: "friend".into(),
            source: MessageSource::CatchUp,
            label: None,
        }),
        &shared,
    );
    assert!(index(&restored, &id).is_none());
    assert!(restored.irc_conversations[&id].hidden);
    let mut live = message;
    live.id = MessageId("new".into());
    live.server_id = Some("new".into());
    apply(
        &mut restored,
        UiEvent::Wire(Event::Message {
            message: live,
            target: "friend".into(),
            source: MessageSource::Live,
            label: None,
        }),
        &shared,
    );
    assert!(index(&restored, &id).is_some());
    assert!(!restored.irc_conversations[&id].hidden);
}

#[tokio::test]
async fn cached_irc_audio_is_ready_in_the_first_history_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _) = shared(dir.path());
    let (mut app, id) = model(&shared, &profile());
    let url = "https://files.example/voice.ogg";
    let media = omtalk_media::Media::link(url).unwrap();
    let cached = media.cache_path(&shared.cache);
    std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
    std::fs::write(&cached, b"fixture").unwrap();
    let message = ChatMessage {
        conversation: id.clone(),
        id: MessageId("cached".into()),
        server_id: None,
        server_time: None,
        at: chrono::Utc::now(),
        sender: "friend".into(),
        outgoing: false,
        kind: MessageKind::Text,
        text: url.into(),
        attachment: None,
    };
    apply(
        &mut app,
        UiEvent::Older {
            conversation: id.clone(),
            lines: vec![line(&message, false)],
            done: true,
        },
        &shared,
    );
    let slot = app.chats[index(&app, &id).unwrap()].lines[0]
        .media
        .as_ref()
        .unwrap();
    assert_eq!(slot.state, MediaState::Ready(cached));
}

#[tokio::test]
async fn pending_private_upload_uses_the_irc_nick_and_long_nicks_fit_the_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _) = shared(dir.path());
    let (mut app, _) = model(&shared, &profile());
    let net = profile().network();
    let view = app.irc_networks.get_mut(&net).unwrap();
    view.nick = "MyNick".into();
    view.caps.upload = true;
    let i = app
        .ensure_irc_chat(&net, "friend", ConversationKind::Direct, true)
        .unwrap();
    app.select(i);
    app.upload(i, dir.path().join("voice.ogg"));
    assert_eq!(app.chats[i].lines[0].who, "MyNick");
    let mut line = Line::message(1, "liquidsoap-bot".into(), "message marker".into(), false, false);
    line.irc = true;
    app.chats[i].kind = ChatKind::Room {
        nick: "MyNick".into(),
    };
    app.chats[i].lines.push(line);
    app.compact = true;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 30)).unwrap();
    terminal
        .draw(|frame| crate::view::draw(frame, &mut app, &mut Default::default()))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        text.contains("liquidsoap-bot"),
        "the longest IRC nick must fit at this width"
    );
    assert!(text.contains("MyNick"));
}

fn shared(dir: &std::path::Path) -> (Shared, mpsc::UnboundedReceiver<Msg>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (
        Shared {
            irc: omtalk_irc::Client::start(vec![]).unwrap().0,
            irc_jobs: Arc::default(),
            accounts: Default::default(),
            read_sync: Default::default(),
            cache: dir.join("cache"),
            viewer: String::new(),
            auto_download: false,
            image_links: false,
            avatars: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            avatar_loads: Default::default(),
            store: Store::open(&dir.join("test.db")).unwrap(),
            tx,
        },
        rx,
    )
}

fn model(shared: &Shared, config: &omtalk_irc::Config) -> (App, ConversationId) {
    model_channel(shared, config, "#voice")
}

fn model_channel(shared: &Shared, config: &omtalk_irc::Config, channel: &str) -> (App, ConversationId) {
    let mut app = App::new(vec![], crate::config::Modifier::Ctrl, 42, 30);
    app.notify.bell_on = crate::notify::When::NEVER;
    restore(&mut app, shared, std::slice::from_ref(config));
    let net = config.network();
    let i = app
        .ensure_irc_chat(&net, channel, ConversationKind::Channel, true)
        .unwrap();
    app.select(i);
    app.irc_networks.get_mut(&net).unwrap().connected = true;
    let id = app.chats[i].jid.conversation().unwrap().clone();
    app.irc_conversations.get_mut(&id).unwrap().joined = true;
    shared
        .store
        .save_conversation(&id, &app.irc_conversations[&id])
        .unwrap();
    (app, id)
}

#[tokio::test]
async fn history_targets_failure_does_not_create_an_empty_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _) = shared(dir.path());
    let (mut app, id) = model(&shared, &profile());
    let before = app.chats.len();
    apply(
        &mut app,
        UiEvent::Wire(Event::HistoryError {
            network: network(&id).clone(),
            request: HistoryRequest {
                target: String::new(),
                kind: HistoryKind::Targets,
                cursor: None,
            },
            reason: "test failure".into(),
        }),
        &shared,
    );
    assert_eq!(app.chats.len(), before);
    assert!(app.irc_conversations.values().all(|s| !s.target.is_empty()));
    assert_eq!(app.status, "test failure");
}

#[tokio::test]
async fn irc_membership_notices_obey_the_setting_keep_event_time_and_do_not_add_unread() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _) = shared(dir.path());
    let (mut app, id) = model(&shared, &profile());
    let i = index(&app, &id).unwrap();
    let at = "2026-11-05T12:34:56.789Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .unwrap();
    let event = || {
        UiEvent::Wire(Event::Presence {
            network: network(&id).clone(),
            channel: "#voice".into(),
            text: "JOIN alice".into(),
            at,
        })
    };
    app.config_ui.irc_presence = false;
    apply(&mut app, event(), &shared);
    assert!(app.chats[i].lines.is_empty());
    app.config_ui.irc_presence = true;
    app.selected = None;
    apply(&mut app, event(), &shared);
    assert_eq!(app.chats[i].lines.len(), 1);
    assert_eq!(app.chats[i].lines[0].at, at.timestamp());
    assert!(
        app.chats[i].lines[0]
            .text
            .contains(&at.with_timezone(&chrono::Local).format("%H:%M").to_string())
    );
    assert!(app.chats[i].lines[0].text.contains("JOIN alice"));
    assert_eq!(app.chats[i].unread, 0);
}

#[tokio::test]
async fn media_survives_replay_and_restart_without_losing_the_local_recording() {
    let dir = tempfile::tempdir().unwrap();
    let (mut shared, mut rx) = shared(dir.path());
    shared.auto_download = true;
    let config = profile();
    let (mut app, id) = model(&shared, &config);
    let path = dir.path().join("voice.ogg");
    std::fs::write(&path, b"fixture").unwrap();
    let mut message = ChatMessage {
        conversation: id.clone(),
        id: MessageId("local".into()),
        server_id: Some("Opaque".into()),
        server_time: Some("2026-11-05T12:00:00.123456789Z".into()),
        at: "2026-11-05T12:00:00.123456789Z".parse().unwrap(),
        sender: "fixture".into(),
        outgoing: true,
        kind: MessageKind::Text,
        text: "https://files.example/opaque.ogg".into(),
        attachment: Some(Attachment {
            url: "https://files.example/opaque.ogg".into(),
            name: "voice.ogg".into(),
            content_type: Some("audio/ogg".into()),
            size: Some(7),
            local_path: Some(path.clone()),
        }),
    };
    let deliver = |app: &mut App, message: ChatMessage| {
        apply(
            app,
            UiEvent::Wire(Event::Message {
                message,
                target: "#voice".into(),
                source: MessageSource::Older,
                label: None,
            }),
            &shared,
        )
    };
    deliver(&mut app, message.clone());
    message.id = MessageId("replay".into());
    message.attachment = None;
    message.outgoing = false; // replay after the own nickname changed
    deliver(&mut app, message);
    // A known extension with automatic downloads enabled must still use our file.
    assert!(matches!(rx.try_recv().unwrap(), Msg::Media {state:MediaState::Ready(p),..} if p==path));
    let check = |app: &App| {
        let i = index(app, &id).unwrap();
        assert_eq!(app.chats[i].lines.len(), 1);
        let line = &app.chats[i].lines[0];
        assert_eq!(line.id.as_deref(), Some("local"));
        assert!(line.irc && !line.encrypted);
        assert!(line.outgoing);
        assert_eq!(
            line.media.as_ref().unwrap().state,
            MediaState::Ready(path.clone())
        );
        assert_eq!(app.chats[i].unread, 0);
    };
    check(&app);
    let mut merged = shared
        .store
        .chat_message(&MessageId("local".into()))
        .unwrap()
        .unwrap();
    let original_time = merged.at;
    merged.server_id = None;
    merged.server_time = None;
    merged.at = chrono::Utc::now();
    shared.store.save_chat_message(&merged, false).unwrap();
    let merged = shared
        .store
        .chat_message(&MessageId("local".into()))
        .unwrap()
        .unwrap();
    assert_eq!(merged.at, original_time);
    assert_eq!(merged.server_id.as_deref(), Some("Opaque"));
    let mut reopened = App::new(vec![], crate::config::Modifier::Ctrl, 42, 30);
    restore(&mut reopened, &shared, &[config]);
    check(&reopened);
}

#[tokio::test]
async fn now_playing_keeps_origin_and_new_draft_and_does_not_reopen_a_closed_chat() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, mut rx) = shared(dir.path());
    let (mut app, id) = model(&shared, &profile());
    app.open_irc_query(network(&id), "other");
    app.input = "a newer draft --account literal".into();
    let selected = app.selected;
    apply(
        &mut app,
        UiEvent::NowPlaying {
            conversation: id.clone(),
            text: Ok("/me np: Artist — Title".into()),
        },
        &shared,
    );
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    // Empty transport fails this send, exposing the exact original destination and intent.
    assert!(
        matches!(&msg, Msg::Irc(UiEvent::SendFailed {conversation, kind:MessageKind::Action,text,..}) if conversation==&id && text=="np: Artist — Title")
    );
    crate::runtime::apply(&mut app, msg, &shared);
    assert_eq!(app.input, "a newer draft --account literal");
    assert_eq!(app.selected, selected);
    app.chats.retain(|c| c.jid.conversation() != Some(&id));
    apply(
        &mut app,
        UiEvent::NowPlaying {
            conversation: id.clone(),
            text: Ok("/me np: ignored".into()),
        },
        &shared,
    );
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    assert!(index(&app, &id).is_none());
}

#[tokio::test]
async fn cancelled_and_failed_uploads_keep_the_file_url_and_origin_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (shared, _rx) = shared(dir.path());
    let (mut app, id) = model(&shared, &profile());
    let path = dir.path().join("recording.ogg");
    std::fs::write(&path, b"local recording").unwrap();
    let attachment = Attachment {
        url: "https://files.example/ready".into(),
        name: "recording.ogg".into(),
        content_type: Some("audio/ogg".into()),
        size: None,
        local_path: Some(path.clone()),
    };
    let (cancel, _) = tokio::sync::watch::channel(false);
    let job = UploadJob {
        conversation: id.clone(),
        path: path.clone(),
        attachment: Some(attachment.clone()),
        cancel,
        running: false,
        sent: false,
    };
    keep_upload(&shared, "upload", &job);
    shared.irc_jobs.lock().unwrap().insert("upload".into(), job);
    perform(&Action::IrcCancel { id: "upload".into() }, &shared).await;
    assert!(path.is_file());
    perform(&Action::IrcRetry { id: "upload".into() }, &shared).await;
    assert!(!shared.irc_jobs.lock().unwrap()["upload"].sent);
    shared.irc_jobs.lock().unwrap().clear();
    restore(&mut app, &shared, &[profile()]);
    let jobs = shared.irc_jobs.lock().unwrap();
    let restored = &jobs["upload"];
    assert_eq!(restored.conversation, id);
    assert_eq!(restored.attachment, Some(attachment));
    assert!(!restored.running && path.is_file());
}

async fn until(
    app: &mut App,
    shared: &Shared,
    rx: &mut mpsc::UnboundedReceiver<Msg>,
    accept: impl Fn(&App) -> bool,
) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while !accept(app) {
            let msg = rx.recv().await.expect("runtime event");
            if let Msg::Audio(crate::audio::Event::Error(e)) = &msg {
                panic!("audio: {e}");
            }
            crate::runtime::apply(app, msg, shared);
        }
    })
    .await
    .unwrap_or_else(|_| panic!("runtime deadline: {}", app.status));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires disposable fixture: python3 tests/irc/run.py"]
async fn real_soju_channel_history_and_unread_with_and_without_message_tags() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    for (name, port_var) in [
        ("alpha", "OMTALK_TEST_IRC_PORT"),
        ("beta", "OMTALK_TEST_IRC_LEGACY_PORT"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (mut shared, mut rx) = shared(dir.path());
        let mut config = profile();
        config.mode = omtalk_irc::Mode::Soju;
        config.tls = true;
        config.port = std::env::var("OMTALK_TEST_SOJU_PORT").unwrap().parse().unwrap();
        config.ca_certificate = Some(std::env::var("OMTALK_TEST_SOJU_CA").unwrap().into());
        config.sasl = Some(omtalk_irc::Sasl {
            username: "fixture".into(),
            password: "fixture-password".into(),
        });
        config.selected_network = Some(name.into());
        let (client, mut wire) = omtalk_irc::Client::start(vec![config.clone()]).unwrap();
        shared.irc = client;
        let tx = shared.tx.clone();
        tokio::spawn(async move {
            while let Some(event) = wire.recv().await {
                if tx.send(Msg::Irc(UiEvent::Wire(event))).is_err() {
                    break;
                }
            }
        });
        let (mut app, id) = model_channel(&shared, &config, "#history");
        let net = network(&id).clone();
        until(&mut app, &shared, &mut rx, |app| {
            app.irc_networks[&net].registered
        })
        .await;
        assert!(app.irc_networks[&net].caps.history, "history on {name}");
        let session = shared.irc.session(&net).unwrap();
        session.send(Command::Raw("JOIN #history".into())).await.unwrap();
        app.irc_conversations.get_mut(&id).unwrap().joined = false;
        until(&mut app, &shared, &mut rx, |app| {
            app.irc_conversations[&id].joined
        })
        .await;

        let stream = tokio::net::TcpStream::connect((
            "127.0.0.1",
            std::env::var(port_var).unwrap().parse::<u16>().unwrap(),
        ))
        .await
        .unwrap();
        let (read, mut write) = stream.into_split();
        let mut peer = BufReader::new(read).lines();
        write
            .write_all(
                b"NICK history-peer\r\nUSER history-peer 0 * :fixture\r\nFIXTURE_HISTORY #history 1050\r\n",
            )
            .await
            .unwrap();
        while !peer
            .next_line()
            .await
            .unwrap()
            .unwrap()
            .contains("fixture history seeded")
        {}
        until(&mut app, &shared, &mut rx, |app| {
            app.chats[index(app, &id).unwrap()]
                .lines
                .iter()
                .any(|l| l.text == "archive 1049")
        })
        .await;
        let last = shared.store.conversation_cursor(&id).unwrap().unwrap().0;
        shared.irc.stop();

        // Fresh local history while soju retains its backlog: startup must populate
        // channels and unread even when the upstream cannot provide message IDs.
        let fresh = tempfile::tempdir().unwrap();
        shared.store = Store::open(&fresh.path().join("history.db")).unwrap();
        let (client, mut wire) = omtalk_irc::Client::start(vec![config.clone()]).unwrap();
        shared.irc = client;
        let (tx, new_rx) = mpsc::unbounded_channel();
        shared.tx = tx.clone();
        rx = new_rx;
        tokio::spawn(async move {
            while let Some(event) = wire.recv().await {
                if tx.send(Msg::Irc(UiEvent::Wire(event))).is_err() {
                    break;
                }
            }
        });
        let (mut app, id) = model_channel(&shared, &config, "#history");
        until(&mut app, &shared, &mut rx, |app| {
            app.irc_conversations[&id].history_loaded
                && shared.store.unread_conversation(&id).unwrap() == 1000
        })
        .await;
        let i = index(&app, &id).unwrap();
        assert!(app.chats[i].lines.len() <= crate::app::SHOWN + crate::app::SHOWN / 2);
        assert_eq!(app.chats[i].unread, 1000, "replay stays unread on {name}");

        // Real dispatcher -> soju BEFORE -> real runtime -> common chat model.
        app.chats[i].scroll = 12;
        for _ in 0..25 {
            if app.chats[i].older_done {
                break;
            }
            for action in app.ask_older() {
                super::super::perform(action, shared.clone()).await;
            }
            until(&mut app, &shared, &mut rx, |app| !app.chats[i].older_loading).await;
            assert_eq!(
                app.chats[i].scroll, 12,
                "a received page cannot scroll on its own"
            );
        }
        assert_eq!(
            app.chats[i].lines.iter().filter(|l| !l.notice).count(),
            1050,
            "older page on {name}"
        );
        assert_eq!(
            app.chats[i]
                .lines
                .iter()
                .filter(|l| l.text == "archive 0000")
                .count(),
            1
        );
        assert_eq!(app.chats[i].unread, 1000, "paging is not new activity");
        assert!(app.chats[i].older_done);
        assert!(shared.store.conversation_read_time(&id).unwrap().is_none());
        shared.store.read_conversation(&id, &last).unwrap();
        assert_eq!(shared.store.unread_conversation(&id).unwrap(), 0);

        // Upgrade a database which only has one live message: a forward cursor
        // must not hide the unread backlog that the old capabilities check skipped.
        let tail = shared
            .store
            .history_page(&id, None, 1)
            .unwrap()
            .messages
            .pop()
            .unwrap();
        let read = tail.at - chrono::Duration::seconds(149);
        let repair = tempfile::tempdir().unwrap();
        shared.store = Store::open(&repair.path().join("live-only.db")).unwrap();
        let (mut app, id) = model_channel(&shared, &config, "#history");
        shared.store.save_chat_message(&tail, false).unwrap();
        shared
            .store
            .read_conversation(&id, &read.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
            .unwrap();
        let snapshot = shared.irc.session(&net).unwrap().snapshot();
        apply(&mut app, UiEvent::Wire(Event::Snapshot(snapshot)), &shared);
        until(&mut app, &shared, &mut rx, |app| {
            app.irc_conversations[&id].history_loaded
        })
        .await;
        assert_eq!(
            shared.store.unread_conversation(&id).unwrap(),
            149,
            "live-only recovery on {name}"
        );
        assert_eq!(
            shared
                .store
                .history_page(&id, None, 2000)
                .unwrap()
                .messages
                .iter()
                .filter(|m| m.text == "archive 1049")
                .count(),
            1,
            "no overlapping legacy replay"
        );
        shared.irc.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires tests/irc/run.py and its dedicated virtual audio device"]
async fn real_soju_voice_record_preview_upload_retry_echo_restart_and_play() {
    use crate::audio;
    assert!(
        std::env::var("PULSE_SOURCE")
            .unwrap()
            .starts_with("omtalk_audio_test")
    );
    let dir = tempfile::tempdir().unwrap();
    let (mut shared, mut rx) = shared(dir.path());
    let mut config = profile();
    config.mode = omtalk_irc::Mode::Soju;
    config.tls = true;
    config.port = std::env::var("OMTALK_TEST_SOJU_PORT").unwrap().parse().unwrap();
    config.ca_certificate = Some(std::env::var("OMTALK_TEST_SOJU_CA").unwrap().into());
    config.sasl = Some(omtalk_irc::Sasl {
        username: "fixture".into(),
        password: "fixture-password".into(),
    });
    config.selected_network = Some("alpha".into());
    let (client, mut wire) = omtalk_irc::Client::start(vec![config.clone()]).unwrap();
    shared.irc = client;
    let tx = shared.tx.clone();
    tokio::spawn(async move {
        while let Some(event) = wire.recv().await {
            if tx.send(Msg::Irc(UiEvent::Wire(event))).is_err() {
                break;
            }
        }
    });
    let (mut app, id) = model(&shared, &config);
    app.irc_networks.get_mut(network(&id)).unwrap().connected = false;
    until(&mut app, &shared, &mut rx, |app| {
        app.irc_networks[network(&id)].caps.upload
    })
    .await;
    let session = shared.irc.session(network(&id)).unwrap();
    session.send(Command::Raw("JOIN #voice".into())).await.unwrap();
    app.irc_conversations.get_mut(&id).unwrap().joined = false;
    until(&mut app, &shared, &mut rx, |app| {
        app.irc_conversations[&id].joined
    })
    .await;

    // A generated PCM tone goes only to a null sink; the real microphone is never opened.
    let source = dir.path().join("tone.wav");
    let samples: Vec<i16> = (0..48000 * 3)
        .map(|n| ((n as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 3000.0) as i16)
        .collect();
    let size = (samples.len() * 2) as u32;
    let mut wav = b"RIFF".to_vec();
    wav.extend((size + 36).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(48000u32.to_le_bytes());
    wav.extend(96000u32.to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(size.to_le_bytes());
    for sample in samples {
        wav.extend(sample.to_le_bytes());
    }
    std::fs::write(&source, wav).unwrap();
    let tx = shared.tx.clone();
    let engine = audio::Engine::new(move |event| {
        let _ = tx.send(Msg::Audio(event));
    });
    assert!(matches!(
        &app.record_voice()[..],
        [Action::Audio(audio::Command::Record(_))]
    ));
    let path = dir.path().join("voice.ogg");
    app.recorder.as_mut().unwrap().path = path.clone();
    engine.send(audio::Command::Record(path.clone()));
    until(&mut app, &shared, &mut rx, |app| {
        app.recorder.as_ref().unwrap().seconds > 0.05
    })
    .await;
    engine.send(audio::Command::Toggle(source));
    until(&mut app, &shared, &mut rx, |app| app.playback.seconds > 0.5).await;
    engine.send(audio::Command::Stop);
    for action in app.recorder_button(0) {
        if let Action::Audio(c) = action {
            engine.send(c);
        }
    }
    until(&mut app, &shared, &mut rx, |app| {
        app.recorder.as_ref().unwrap().ready
    })
    .await;
    for action in app.recorder_button(0) {
        if let Action::Audio(c) = action {
            engine.send(c);
        }
    }
    until(&mut app, &shared, &mut rx, |app| {
        app.playback.path == path && app.playback.playing
    })
    .await;
    assert!(audio::inspect(&path).unwrap().peaks.iter().any(|p| *p > 2));
    app.open_irc_query(network(&id), "other-chat");
    let actions = app.recorder_button(1);
    for action in &actions {
        if let Action::Audio(c) = action {
            engine.send(c.clone());
        }
    }
    let upload_action = actions
        .into_iter()
        .find(|a| matches!(a, Action::Upload { .. }))
        .unwrap();
    assert!(matches!(&upload_action,Action::Upload{to,..}if to.conversation()==Some(&id)));
    let Action::Upload { id: upload_id, .. } = &upload_action else {
        unreachable!()
    };
    // Closing the original channel after recording must retain the newly uploaded URL.
    app.irc_conversations.get_mut(&id).unwrap().hidden = true;
    shared
        .store
        .save_conversation(&id, &app.irc_conversations[&id])
        .unwrap();
    perform(&upload_action, &shared).await;
    let ready = shared.irc_jobs.lock().unwrap()[upload_id]
        .attachment
        .clone()
        .expect("uploaded URL retained");
    assert!(!shared.irc_jobs.lock().unwrap()[upload_id].sent);
    // A retry with a missing source file proves it reused the ready URL, not a second POST.
    let aside = path.with_extension("kept");
    std::fs::rename(&path, &aside).unwrap();
    app.irc_conversations.get_mut(&id).unwrap().hidden = false;
    shared
        .store
        .save_conversation(&id, &app.irc_conversations[&id])
        .unwrap();
    perform(
        &Action::IrcRetry {
            id: upload_id.clone(),
        },
        &shared,
    )
    .await;
    std::fs::rename(&aside, &path).unwrap();
    assert!(shared.irc_jobs.lock().unwrap()[upload_id].sent);
    until(&mut app, &shared, &mut rx, |app| {
        app.chats[index(app, &id).unwrap()]
            .lines
            .iter()
            .any(|l| l.text == ready.url && l.delivery == Some(Delivery::Sent))
    })
    .await;
    assert_eq!(
        app.chats[index(&app, &id).unwrap()]
            .lines
            .iter()
            .filter(|l| l.text == ready.url)
            .count(),
        1
    );
    let mut reopened = App::new(vec![], crate::config::Modifier::Ctrl, 42, 30);
    restore(&mut reopened, &shared, &[config]);
    reopened.select(index(&reopened, &id).unwrap());
    for action in reopened.audio_command(None) {
        if let Action::Audio(c) = action {
            engine.send(c);
        }
    }
    until(&mut reopened, &shared, &mut rx, |app| {
        app.playback.path == path && app.playback.playing
    })
    .await;
    engine.send(audio::Command::Quit);
    shared.irc.stop();
}
