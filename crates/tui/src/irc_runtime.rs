//! IRC dispatch into omtalk's existing chat, history, notifications and media UI.
use super::*;
use crate::address::Address;
use omtalk_core::{
    Attachment, ChatMessage, ConnectionState, ConversationId, ConversationKind, MessageId, MessageKind,
    MessageSource, Network, NetworkId,
};
use omtalk_irc::{Command, Event, HistoryKind, HistoryRequest};
use omtalk_store::{ConversationState, SaveOutcome};
use std::collections::BTreeMap;

pub enum UiEvent {
    Wire(Event),
    NowPlaying {
        conversation: ConversationId,
        text: Result<String, String>,
    },
    File {
        conversation: ConversationId,
        id: String,
        state: Delivery,
        attachment: Option<Attachment>,
    },
    Older {
        conversation: ConversationId,
        lines: Vec<Line>,
        done: bool,
    },
    Found {
        conversation: ConversationId,
        words: String,
        found: Vec<crate::app::Found>,
    },
    Jump {
        conversation: ConversationId,
        at: i64,
        lines: Vec<Line>,
    },
    Read(ConversationId),
    SendFailed {
        conversation: ConversationId,
        text: String,
        kind: MessageKind,
        error: String,
    },
}

pub struct UploadJob {
    conversation: ConversationId,
    path: PathBuf,
    attachment: Option<Attachment>,
    cancel: tokio::sync::watch::Sender<bool>,
    running: bool,
    sent: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct KeptUpload {
    conversation: ConversationId,
    path: PathBuf,
    attachment: Option<Attachment>,
}

fn keep_upload(shared: &Shared, id: &str, job: &UploadJob) {
    let key = format!("irc/upload/{id}");
    let value = (!job.sent)
        .then(|| {
            serde_json::to_string(&KeptUpload {
                conversation: job.conversation.clone(),
                path: job.path.clone(),
                attachment: job.attachment.clone(),
            })
        })
        .transpose();
    match value {
        Ok(value) => {
            if let Err(e) = shared.store.keep(&key, value.as_deref()) {
                let _ = shared.tx.send(Msg::Status(format!("IRC upload draft: {e}")));
            }
        }
        Err(e) => {
            let _ = shared.tx.send(Msg::Status(format!("IRC upload draft: {e}")));
        }
    }
}

fn network(id: &ConversationId) -> &NetworkId {
    match id {
        ConversationId::Irc { network, .. } => network,
        _ => unreachable!("IRC dispatcher only"),
    }
}
fn kind(id: &ConversationId) -> ConversationKind {
    match id {
        ConversationId::Irc { kind, .. } => *kind,
        _ => unreachable!("IRC dispatcher only"),
    }
}
fn index(app: &App, id: &ConversationId) -> Option<usize> {
    app.chats.iter().position(|c| c.jid.conversation() == Some(id))
}
fn send(shared: &Shared, event: UiEvent) {
    let _ = shared.tx.send(Msg::Irc(event));
}
fn command(shared: &Shared, net: &NetworkId, command: Command) {
    let Some(session) = shared.irc.session(net) else {
        return;
    };
    let tx = shared.tx.clone();
    tokio::spawn(async move {
        if let Err(error) = session.send(command).await {
            let _ = tx.send(Msg::Status(error));
        }
    });
}

pub fn restore(app: &mut App, shared: &Shared, profiles: &[omtalk_irc::Config]) {
    for profile in profiles {
        app.add_irc_network(
            profile.network(),
            profile.name.clone(),
            profile.name.clone(),
            profile.tls,
        );
        let account = omtalk_core::AccountId::irc(&profile.id);
        let mut conversations = shared.store.conversations(&account).unwrap_or_default();
        conversations.sort_by_key(|(_, s)| s.position);
        for (id, mut state) in conversations {
            let net = network(&id).clone();
            app.add_irc_network(
                net.clone(),
                profile.name.clone(),
                match &net.network {
                    Network::Soju(id) => id.clone(),
                    _ => profile.name.clone(),
                },
                profile.tls,
            );
            // Joined is authoritative only after this session's own JOIN. soju owns
            // upstream membership; restoring a local window never joins or parts it.
            state.joined = false;
            let (target, hidden) = (state.target.clone(), state.hidden);
            app.irc_conversations.insert(id.clone(), state.clone());
            if hidden {
                continue;
            }
            if let Some(i) = app.ensure_irc_chat(&net, &target, kind(&id), false) {
                app.chats[i].lines = shared
                    .store
                    .history_page(&id, None, HISTORY)
                    .unwrap_or_default()
                    .messages
                    .into_iter()
                    .map(|m| line(&m, shared.image_links))
                    .collect();
                app.chats[i].unread = shared.store.unread_conversation(&id).unwrap_or(0);
                app.chats[i].read_mark = shared.store.conversation_read_time(&id).ok().flatten();
                app.drafts
                    .insert((net.clone().into(), id.clone().into()), state.draft);
                if let Some(prefs) = state.notify.and_then(|n| toml::from_str(&n).ok()) {
                    app.chat_notify
                        .insert((net.clone().into(), id.clone().into()), prefs);
                }
                if let Some(folder) = state.folder {
                    if let Some(f) = app
                        .folders
                        .iter_mut()
                        .find(|f| f.account.network() == Some(&net) && f.name == folder)
                    {
                        if !f.chats.iter().any(|c| c.conversation() == Some(&id)) {
                            f.chats.push(id.into());
                        }
                    } else {
                        app.folders.push(crate::address::Folder {
                            account: net.into(),
                            name: folder,
                            chats: vec![id.into()],
                        });
                    }
                }
            }
        }
    }
    let mut irc_folders: Vec<_> = app
        .folders
        .iter()
        .filter(|f| f.account.is_irc())
        .cloned()
        .collect();
    irc_folders.sort_by_key(|f| {
        app.irc_conversations
            .iter()
            .filter(|(id, s)| {
                Some(network(id)) == f.account.network() && s.folder.as_deref() == Some(&f.name)
            })
            .map(|(_, s)| s.folder_position)
            .min()
            .unwrap_or_default()
    });
    let mut ordered = irc_folders.into_iter();
    for folder in &mut app.folders {
        if folder.account.is_irc() {
            *folder = ordered.next().unwrap();
        }
    }
    for (key, value) in shared.store.kept_under("irc/upload/").unwrap_or_default() {
        let Ok(kept) = serde_json::from_str::<KeptUpload>(&value) else {
            continue;
        };
        if !app.irc_conversations.contains_key(&kept.conversation) {
            continue;
        }
        let id = key.as_str();
        let (cancel, _) = tokio::sync::watch::channel(false);
        let job = UploadJob {
            conversation: kept.conversation,
            path: kept.path,
            attachment: kept.attachment,
            cancel,
            running: false,
            sent: false,
        };
        restore_upload_line(app, id, &job);
        shared.irc_jobs.lock().unwrap().insert(id.into(), job);
    }
}

fn restore_upload_line(app: &mut App, id: &str, job: &UploadJob) {
    let Some(i) = index(app, &job.conversation) else {
        return;
    };
    if app.chats[i].lines.iter().any(|l| l.id.as_deref() == Some(id)) {
        return;
    }
    let name = job
        .path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut pending = Line::message_sharing(
        now(),
        app.irc_networks[network(&job.conversation)].nick.clone(),
        name,
        false,
        true,
        true,
    );
    pending.irc = true;
    pending.id = Some(id.into());
    pending.delivery = Some(Delivery::Failed(
        tr!("file kept; /retry to send", "файл сохранён; /retry для отправки").into(),
    ));
    pending.media = Some(crate::app::MediaSlot {
        url: job.attachment.as_ref().map(|a| a.url.clone()).unwrap_or_default(),
        name: job
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        image: job
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(content_type)
            .is_some_and(|m| m.starts_with("image/")),
        state: MediaState::Ready(job.path.clone()),
        link: job.attachment.as_ref().map(|a| a.url.clone()).unwrap_or_default(),
        preview: false,
    });
    app.chats[i].lines.push(pending);
}

/// Keep the existing folder/draft controls; persist IRC under typed keys only.
pub fn persist(app: &mut App, shared: &Shared, kept: &mut BTreeMap<ConversationId, ConversationState>) {
    for (id, job) in shared
        .irc_jobs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, job)| !job.sent)
    {
        restore_upload_line(app, id, job);
    }
    let current = app.current().and_then(|c| c.jid.conversation()).cloned();
    let mut states = app.irc_conversations.clone();
    for (id, state) in &mut states {
        if let Some(i) = index(app, id) {
            let chat = &app.chats[i];
            state.muted = chat.muted;
            state.position = i as i64;
            state.draft = if current.as_ref() == Some(id) {
                app.input.clone()
            } else {
                app.drafts
                    .get(&(chat.account.clone(), chat.jid.clone()))
                    .cloned()
                    .unwrap_or_default()
            };
            // Credentials sent to services are never retained as a draft.
            if omtalk_irc::is_sensitive(&state.target, &state.draft) {
                state.draft.clear();
            }
            state.notify = app
                .chat_notify
                .get(&(chat.account.clone(), chat.jid.clone()))
                .and_then(|p| toml::to_string(p).ok());
            state.folder = None;
            for (pos, folder) in app.folders.iter().enumerate() {
                if folder.account == chat.account && folder.chats.contains(&chat.jid) {
                    state.folder = Some(folder.name.clone());
                    state.folder_position = pos as i64;
                }
            }
        }
        if kept.get(id) != Some(state) {
            match shared.store.save_conversation(id, state) {
                Ok(()) => {
                    kept.insert(id.clone(), state.clone());
                }
                Err(error) => app.status = format!("IRC: {error}"),
            }
        }
    }
    app.irc_conversations = states;
    if let Some(id) = current
        && let Ok(id) = serde_json::to_string(&id)
    {
        let _ = shared.store.keep("irc/open", Some(&id));
    } else if app.current().is_some() {
        let _ = shared.store.keep("irc/open", None);
    }
}

pub fn local_action(app: &mut App, action: &Action, shared: &Shared) -> bool {
    match action {
        Action::SaveIrcConversation { conversation, state } => {
            if let Err(error) = shared.store.save_conversation(conversation, state) {
                app.status = error.to_string();
            }
            true
        }
        Action::SetMuted { chat, muted, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation()
                && let Some(state) = app.irc_conversations.get_mut(id)
            {
                state.muted = *muted;
            }
            true
        }
        Action::SetChatNotify { chat, prefs, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation()
                && let Some(state) = app.irc_conversations.get_mut(id)
            {
                state.notify = toml::to_string(prefs).ok();
            }
            true
        }
        _ => false,
    }
}

fn line(message: &ChatMessage, image_links: bool) -> Line {
    let mut line = Line::message_sharing(
        message.at.timestamp(),
        message.sender.clone(),
        message.text.clone(),
        false,
        message.outgoing,
        message.attachment.is_some(),
    );
    line.irc = true;
    line.set_image_links(image_links);
    if let Some(file) = &message.attachment {
        line.media = Some(crate::app::MediaSlot {
            url: file.url.clone(),
            link: file.url.clone(),
            name: file.name.clone(),
            image: file
                .content_type
                .as_ref()
                .is_some_and(|m| m.starts_with("image/")),
            state: file
                .local_path
                .as_ref()
                .filter(|p| p.is_file())
                .map(|p| MediaState::Ready(p.clone()))
                .unwrap_or(MediaState::Offered),
            preview: false,
        });
    }
    line.echoed = message.outgoing;
    line.kind = message.kind;
    line.server_time = message.server_time.clone();
    line.id = Some(message.id.0.clone());
    line.archive_id = message.server_id.clone();
    // No IRC peer delivery/read receipts. "Sent" describes only our downstream send.
    line.delivery = message.outgoing.then_some(Delivery::Sent);
    line
}

fn conversation(
    app: &mut App,
    shared: &Shared,
    net: &NetworkId,
    target: &str,
    reveal: bool,
) -> Option<ConversationId> {
    if target.is_empty() || target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let view = app.irc_networks.get(net)?;
    let kind = if view.support.is_channel(target) {
        ConversationKind::Channel
    } else if target.eq_ignore_ascii_case("BouncerServ") {
        ConversationKind::Control
    } else {
        ConversationKind::Direct
    };
    let found = app.irc_conversations.iter().find_map(|(id, state)| {
        (network(id) == net
            && self::kind(id) == kind
            && view.support.case_mapping.equivalent(target, &state.target))
        .then(|| id.clone())
    });
    if let Some(id) = found {
        if reveal {
            app.ensure_irc_chat(net, target, kind, true);
            for (key, job) in shared
                .irc_jobs
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, j)| j.conversation == id && !j.sent)
            {
                restore_upload_line(app, key, job);
            }
        }
        return Some(id);
    }
    let id = app.irc_conversation_id(net, target, kind)?;
    let state = ConversationState {
        target: target.into(),
        ..Default::default()
    };
    if let Err(e) = shared.store.save_conversation(&id, &state) {
        app.status = e.to_string();
        return None;
    }
    app.irc_conversations.insert(id.clone(), state);
    app.ensure_irc_chat(net, target, kind, false);
    Some(id)
}

fn catch_up(app: &mut App, shared: &Shared, id: &ConversationId) {
    let net = network(id);
    if kind(id) == ConversationKind::Control
        || !app.irc_networks.get(net).is_some_and(|n| n.caps.history)
        || !app.irc_catching.insert(id.clone())
    {
        return;
    }
    let Some(state) = app.irc_conversations.get(id) else {
        return;
    };
    let cursor = shared.store.conversation_cursor(id).ok().flatten();
    if cursor.is_some() && !state.history_loaded {
        // A live message advances the forward cursor even when history was
        // unavailable. Recover the preceding backlog separately, without replaying
        // those live messages (legacy networks have no IDs to deduplicate them).
        if let Ok(Some(first)) = shared.store.first_conversation_message(id)
            && let Some(time) = &first.server_time
        {
            command(
                shared,
                net,
                Command::History(HistoryRequest {
                    target: state.target.clone(),
                    kind: HistoryKind::BeforeCatchUp,
                    cursor: Some(reference(
                        &app.irc_networks[net].support,
                        time,
                        first.server_id.as_deref(),
                    )),
                }),
            );
        }
    }
    let request = HistoryRequest {
        target: state.target.clone(),
        kind: if cursor.is_some() {
            HistoryKind::After
        } else {
            HistoryKind::Latest
        },
        cursor: cursor.map(|(time, id)| reference(&app.irc_networks[net].support, &time, id.as_deref())),
    };
    command(shared, net, Command::History(request));
    if app.irc_networks[net].caps.read_sync {
        command(
            shared,
            net,
            Command::Read {
                target: state.target.clone(),
                timestamp: None,
            },
        );
    }
}

fn reference(support: &omtalk_irc::Support, time: &str, id: Option<&str>) -> String {
    if support.reference_types.iter().any(|s| s == "msgid")
        && let Some(id) = id
    {
        return format!("msgid={id}");
    }
    format!("timestamp={time}")
}

pub fn apply(app: &mut App, event: UiEvent, shared: &Shared) {
    match event {
        UiEvent::SendFailed {
            conversation,
            text,
            kind,
            error,
        } => {
            app.status = error;
            let target = app
                .irc_conversations
                .get(&conversation)
                .map(|s| s.target.as_str())
                .unwrap_or("");
            if omtalk_irc::is_sensitive(target, &text) {
                return;
            }
            let text = if kind == MessageKind::Action {
                format!("/me {text}")
            } else if kind == MessageKind::Notice {
                format!("/notice {target} {text}")
            } else if text.starts_with('/') || text.starts_with(':') {
                format!("{}{text}", &text[..1])
            } else {
                text
            };
            let key = (network(&conversation).clone().into(), conversation.clone().into());
            if app
                .current()
                .is_some_and(|c| c.jid.conversation() == Some(&conversation))
            {
                if !app.input.is_empty() {
                    app.input.push('\n');
                }
                app.input.push_str(&text);
                app.cursor = app.input.chars().count();
            } else {
                let draft = app.drafts.entry(key).or_default();
                if !draft.is_empty() {
                    draft.push('\n');
                }
                draft.push_str(&text);
            }
        }
        UiEvent::NowPlaying { conversation, text } => match text {
            Ok(text) => {
                if let Some(i) = index(app, &conversation) {
                    for action in app.send_irc(i, text, MessageKind::Action) {
                        tokio::spawn(super::perform(action, shared.clone()));
                    }
                }
            }
            Err(error) => app.status = error,
        },
        UiEvent::File {
            conversation,
            id,
            state,
            attachment,
        } => {
            if let Some(i) = index(app, &conversation) {
                let account = app.chats[i].account.clone();
                if let Some(file) = attachment {
                    app.uploaded(&account, &id, &file.url);
                    if let Some(line) = app.chats[i].lines.iter_mut().find(|l| l.id.as_ref() == Some(&id)) {
                        line.irc = true;
                        line.media = Some(crate::app::MediaSlot {
                            url: file.url.clone(),
                            link: file.url,
                            name: file.name,
                            image: file.content_type.is_some_and(|m| m.starts_with("image/")),
                            state: file
                                .local_path
                                .map(MediaState::Ready)
                                .unwrap_or(MediaState::Offered),
                            preview: false,
                        });
                    }
                }
                let snapshot = shared.irc.session(network(&conversation)).map(|s| s.snapshot());
                if state == Delivery::Sent
                    && snapshot
                        .as_ref()
                        .is_some_and(|s| s.capabilities.echo && !s.labels)
                {
                    app.chats[i]
                        .lines
                        .retain(|l| l.id.as_deref() != Some(&id) || l.echoed);
                } else if let Some(line) = app.chats[i]
                    .lines
                    .iter_mut()
                    .find(|l| l.id.as_deref() == Some(&id))
                {
                    line.delivery = Some(state.clone());
                }
                if let Delivery::Failed(error) = state {
                    app.status = error;
                }
            }
        }
        UiEvent::Older {
            conversation,
            mut lines,
            done,
        } => {
            trace_scroll(
                app,
                &format!(
                    "IRC local older conversation={conversation:?} lines={} done={done}",
                    lines.len()
                ),
            );
            prepare_media(&mut lines, shared);
            app.older(
                &network(&conversation).clone().into(),
                &conversation.into(),
                lines,
                None,
                done,
            );
        }
        UiEvent::Found {
            conversation,
            words,
            found,
        } => app.found(
            &network(&conversation).clone().into(),
            &conversation.into(),
            &words,
            found,
        ),
        UiEvent::Jump {
            conversation,
            at,
            mut lines,
        } => {
            prepare_media(&mut lines, shared);
            app.jumped(
                &network(&conversation).clone().into(),
                &conversation.into(),
                lines,
                at,
            );
        }
        UiEvent::Read(id) => {
            if let Some(i) = index(app, &id) {
                for action in app.mark_chat_read(i) {
                    tokio::spawn(super::perform(action, shared.clone()));
                }
            }
        }
        UiEvent::Wire(event) => apply_wire(app, event, shared),
    }
}

fn apply_wire(app: &mut App, event: Event, shared: &Shared) {
    match event {
        Event::Pinned {
            network,
            target,
            pinned,
        } => {
            if let Some(id) = conversation(app, shared, &network, &target, false) {
                app.update_pin((network.into(), id.into()), pinned);
            }
        }
        Event::Network(info) => {
            let Some(config) = shared.irc.config(&info.id.account.id) else {
                return;
            };
            app.add_irc_network(
                info.id.clone(),
                config.name.clone(),
                info.id.account.id.clone(),
                config.tls,
            );
            let view = app.irc_networks.get_mut(&info.id).unwrap();
            if info.attributes.contains_key("name") {
                view.name = info.attributes["name"]
                    .clone()
                    .unwrap_or_else(|| info.id.account.id.clone());
            }
            if info.attributes.contains_key("state") {
                view.upstream = info.attributes["state"].clone().unwrap_or_default();
            }
            view.removed = info.removed;
            if info.removed {
                view.connected = false;
            }
            if info.removed || matches!(view.upstream.as_str(), "disconnected" | "connecting") {
                app.irc_catching.retain(|id| network(id) != &info.id);
                for (id, state) in &mut app.irc_conversations {
                    if network(id) == &info.id {
                        state.joined = false;
                    }
                }
                for chat in &mut app.chats {
                    if chat.account.network() == Some(&info.id) {
                        chat.occupants.clear();
                        chat.irc_prefixes.clear();
                    }
                }
            }
        }
        Event::Connection { network, state } => {
            let Some(config) = shared.irc.config(&network.account.id) else {
                return;
            };
            app.add_irc_network(
                network.clone(),
                config.name.clone(),
                config.name.clone(),
                config.tls,
            );
            let view = app.irc_networks.get_mut(&network).unwrap();
            view.connected = state == ConnectionState::Connected;
            view.removed = state == ConnectionState::Removed;
            let state = match state {
                ConnectionState::Connecting => Connection::Connecting,
                ConnectionState::Connected => Connection::Online,
                ConnectionState::Reconnecting(error) => Connection::Reconnecting(error),
                ConnectionState::Failed(error) => Connection::Failed(error),
                ConnectionState::Removed => Connection::Failed(tr!("network removed", "сеть удалена").into()),
            };
            if !state.online() {
                app.irc_networks.get_mut(&network).unwrap().caps = Default::default();
                app.irc_networks.get_mut(&network).unwrap().registered = false;
                app.irc_catching.retain(|id| self::network(id) != &network);
                for (id, state) in &mut app.irc_conversations {
                    if self::network(id) == &network {
                        state.joined = false;
                    }
                }
                for chat in &mut app.chats {
                    if chat.account.network() == Some(&network) {
                        chat.occupants.clear();
                        chat.irc_prefixes.clear();
                        chat.older_loading = false;
                    }
                }
            }
            let address: Address = network.into();
            app.server_notice(&address, format!("IRC: {}", state.words()), now());
            app.connection.insert(address, state);
        }
        Event::Snapshot(snapshot) => {
            let net = snapshot.network;
            let Some(view) = app.irc_networks.get_mut(&net) else {
                return;
            };
            let history_new = snapshot.connected && snapshot.capabilities.history && !view.caps.history;
            let registered = snapshot.connected && !view.registered;
            let pins_new = snapshot.connected && snapshot.capabilities.pin_sync && !view.caps.pin_sync;
            let reads_new = snapshot.connected && snapshot.capabilities.read_sync && !view.caps.read_sync;
            view.registered = snapshot.connected;
            view.nick = snapshot.nick;
            if net.network == Network::Selected
                && let Some(name) = &snapshot.support.network_name
            {
                view.name = name.clone();
            }
            view.support = snapshot.support;
            view.caps = snapshot.capabilities;
            view.connected = snapshot.connected;
            if reads_new {
                // Retry only a previously explicit local frontier, never mark newly
                // replayed messages read just because a session registered.
                for (id, state) in app.irc_conversations.iter().filter(|(id, _)| network(id) == &net) {
                    if let Ok(Some(timestamp)) = shared.store.conversation_read_time(id) {
                        command(
                            shared,
                            &net,
                            Command::Read {
                                target: state.target.clone(),
                                timestamp: Some(timestamp),
                            },
                        );
                    }
                }
            }
            if pins_new {
                // SUB supplies existing pins. GET also clears locally cached pins
                // which another client removed while this one was disconnected.
                for (_, chat) in app
                    .pins
                    .iter()
                    .filter(|(account, _)| account.network() == Some(&net))
                {
                    if let Some(state) = chat.conversation().and_then(|id| app.irc_conversations.get(id)) {
                        command(
                            shared,
                            &net,
                            Command::Raw(format!("METADATA {} GET soju.im/pinned", state.target)),
                        );
                    }
                }
            }
            for chat in &mut app.chats {
                if chat.account.network() == Some(&net) && chat.is_room() {
                    chat.kind = ChatKind::Room {
                        nick: view.nick.clone(),
                    };
                }
            }
            if registered && net.network == Network::Direct {
                for (id, state) in &app.irc_conversations {
                    if network(id) == &net && kind(id) == ConversationKind::Channel && state.desired_join {
                        command(shared, &net, Command::Raw(format!("JOIN {}", state.target)));
                    }
                }
            }
            if history_new {
                let ids: Vec<_> = app
                    .irc_conversations
                    .keys()
                    .filter(|id| network(id) == &net)
                    .cloned()
                    .collect();
                for id in ids {
                    catch_up(app, shared, &id);
                }
                command(
                    shared,
                    &net,
                    Command::History(HistoryRequest {
                        target: String::new(),
                        kind: HistoryKind::Targets,
                        cursor: None,
                    }),
                );
            }
        }
        Event::Joined { network, channel } => {
            let Some(id) = conversation(app, shared, &network, &channel, false) else {
                return;
            };
            if let Some(state) = app.irc_conversations.get_mut(&id) {
                state.joined = true;
                if network.network == Network::Direct {
                    state.desired_join = true;
                }
                let _ = shared.store.save_conversation(&id, state);
            }
            catch_up(app, shared, &id);
        }
        Event::Left { network, channel } => {
            if let Some(id) = conversation(app, shared, &network, &channel, false) {
                app.irc_catching.remove(&id);
                if let Some(state) = app.irc_conversations.get_mut(&id) {
                    state.joined = false;
                    state.desired_join = false;
                    let _ = shared.store.save_conversation(&id, state);
                }
                if let Some(i) = index(app, &id) {
                    app.chats[i].occupants.clear();
                    app.chats[i].irc_prefixes.clear();
                    app.chats[i].older_loading = false;
                    app.chats[i].older_done = false;
                }
            }
        }
        Event::Participants {
            network,
            channel,
            participants,
        } => {
            if let Some(id) = conversation(app, shared, &network, &channel, false)
                && let Some(i) = index(app, &id)
            {
                let chat = &mut app.chats[i];
                chat.occupants.clear();
                chat.irc_prefixes.clear();
                chat.occupant_status.clear();
                chat.irc_prefix_order = app.irc_networks[&network].support.prefix_symbols.clone();
                for p in participants {
                    chat.occupants.insert(p.nick.clone(), None);
                    chat.irc_prefixes.insert(p.nick.clone(), p.prefixes);
                    if p.away {
                        chat.occupant_status
                            .insert(p.nick, omtalk_xmpp::presence::OwnStatus::from_kept("away"));
                    }
                }
            }
        }
        Event::Presence {
            network,
            channel,
            text,
            at,
        } => {
            if app.config_ui.irc_presence
                && let Some(id) = conversation(app, shared, &network, &channel, false)
                && let Some(i) = index(app, &id)
            {
                let clock = at.with_timezone(&chrono::Local).format("%H:%M");
                app.notice(Some(i), format!("{clock}  {text}"), at.timestamp());
            }
        }
        Event::Topic {
            network,
            channel,
            text,
        } => {
            if let Some(id) = conversation(app, shared, &network, &channel, false)
                && let Some(i) = index(app, &id)
            {
                app.chats[i].topic = Some(text);
                app.chats[i].irc_topic_author = None;
            }
        }
        Event::TopicAuthor {
            network,
            channel,
            nick,
            at,
        } => {
            if let Some(id) = conversation(app, shared, &network, &channel, false)
                && let Some(i) = index(app, &id)
            {
                app.chats[i].irc_topic_author = Some((nick, at.timestamp()));
            }
        }
        Event::Nick {
            network,
            old,
            new,
            own,
        } => {
            let Some(view) = app.irc_networks.get_mut(&network) else {
                return;
            };
            if own {
                view.nick = new.clone();
            }
            let mapping = view.support.case_mapping;
            let collision = app.irc_conversations.iter().any(|(id, s)| {
                self::network(id) == &network
                    && kind(id) == ConversationKind::Direct
                    && mapping.equivalent(&s.target, &new)
            });
            if !collision {
                for (id, state) in &mut app.irc_conversations {
                    if self::network(id) == &network
                        && kind(id) == ConversationKind::Direct
                        && mapping.equivalent(&state.target, &old)
                    {
                        state.target = new.clone();
                        if let Some(chat) = app.chats.iter_mut().find(|c| c.jid.conversation() == Some(id)) {
                            chat.name = Some(new.clone());
                        }
                    }
                }
            }
            if own {
                for chat in &mut app.chats {
                    if chat.account.network() == Some(&network) && chat.is_room() {
                        chat.kind = ChatKind::Room { nick: new.clone() };
                    }
                }
            }
        }
        Event::Read {
            network,
            target,
            timestamp,
        } => {
            if let Some(time) = timestamp
                && let Some(id) = conversation(app, shared, &network, &target, false)
            {
                let _ = shared.store.read_conversation(&id, &time);
                if let Some(i) = index(app, &id) {
                    app.chats[i].read_mark = shared.store.conversation_read_time(&id).ok().flatten();
                    app.chats[i].unread = shared
                        .store
                        .unread_conversation(&id)
                        .unwrap_or(app.chats[i].unread);
                }
            }
        }
        Event::HistoryTarget { network, target, .. } => {
            if let Some(id) = conversation(app, shared, &network, &target, false) {
                catch_up(app, shared, &id);
            }
        }
        Event::HistoryDone {
            network,
            request,
            count,
            exhausted,
            last_time,
            last_id,
        } => {
            trace_scroll(
                app,
                &format!(
                    "IRC history done network={network:?} target={} kind={:?} count={count} exhausted={exhausted}",
                    request.target, request.kind
                ),
            );
            if request.kind == HistoryKind::Targets {
                if !exhausted && let Some(time) = last_time {
                    let cursor = format!("timestamp={time}");
                    if request.cursor.as_ref() != Some(&cursor) {
                        command(
                            shared,
                            &network,
                            Command::History(HistoryRequest {
                                target: String::new(),
                                kind: HistoryKind::Targets,
                                cursor: Some(cursor),
                            }),
                        );
                    }
                }
                return;
            }
            if let Some(id) = conversation(app, shared, &network, &request.target, false) {
                if matches!(request.kind, HistoryKind::Latest | HistoryKind::BeforeCatchUp) {
                    let oldest = shared.store.first_conversation_message(&id).ok().flatten();
                    let before = oldest.as_ref().and_then(|m| {
                        m.server_time.as_ref().map(|time| {
                            reference(&app.irc_networks[&network].support, time, m.server_id.as_deref())
                        })
                    });
                    let read = shared
                        .store
                        .conversation_read_time(&id)
                        .ok()
                        .flatten()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(&t).ok());
                    // Fetch all known-unread backlog, but do not walk the entire
                    // archive when there is no read marker. Manual paging stays manual.
                    let more_unread = !exhausted
                        && count > 0
                        && before != request.cursor
                        && oldest.as_ref().zip(read).is_some_and(|(m, read)| m.at > read);
                    if more_unread {
                        command(
                            shared,
                            &network,
                            Command::History(HistoryRequest {
                                target: request.target.clone(),
                                kind: HistoryKind::BeforeCatchUp,
                                cursor: before,
                            }),
                        );
                    } else if let Some(state) = app.irc_conversations.get_mut(&id) {
                        state.history_loaded = true;
                        let _ = shared.store.save_conversation(&id, state);
                    }
                    if let Some(i) = index(app, &id) {
                        app.chats[i].older_loading = false;
                        let first_visible = app.chats[i].lines.iter().find_map(|l| l.id.as_deref());
                        app.chats[i].older_done =
                            exhausted && oldest.as_ref().is_none_or(|m| first_visible == Some(&m.id.0));
                    }
                }
                if request.kind == HistoryKind::Before
                    && let Some(i) = index(app, &id)
                {
                    app.chats[i].older_loading = false;
                    let oldest = shared.store.first_conversation_message(&id).ok().flatten();
                    let cursor = oldest.and_then(|m| {
                        m.server_time.as_ref().map(|t| {
                            reference(&app.irc_networks[&network].support, t, m.server_id.as_deref())
                        })
                    });
                    app.chats[i].older_done = exhausted || cursor == request.cursor;
                }
                if matches!(request.kind, HistoryKind::After)
                    && !exhausted
                    && count > 0
                    && let Some(time) = last_time
                {
                    let cursor = reference(&app.irc_networks[&network].support, &time, last_id.as_deref());
                    if request.cursor.as_ref() != Some(&cursor) {
                        command(
                            shared,
                            &network,
                            Command::History(HistoryRequest {
                                target: request.target,
                                kind: HistoryKind::After,
                                cursor: Some(cursor),
                            }),
                        );
                    } else {
                        app.status = tr!(
                            "IRC history cursor did not advance",
                            "курсор истории IRC не продвинулся"
                        )
                        .into();
                    }
                }
            }
        }
        Event::HistoryError {
            network,
            request,
            reason,
        } => {
            if let Some(id) = conversation(app, shared, &network, &request.target, false)
                && let Some(i) = index(app, &id)
            {
                app.chats[i].older_loading = false;
            }
            app.status = reason;
        }
        Event::Diagnostic {
            network,
            target,
            text,
        } => {
            if let Some(target) = target
                && let Some(id) = conversation(app, shared, &network, &target, false)
                && let Some(i) = index(app, &id)
            {
                app.notice(Some(i), text, now());
            } else {
                app.server_notice(&network.into(), text, now());
            }
        }
        Event::Sent { network, id, error } => {
            if let Some(error) = error {
                app.delivery(&network.into(), &id.0, Delivery::Failed(error));
            }
        }
        Event::NetworksListed { .. } => {}
        Event::Message {
            mut message,
            target,
            source,
            label,
        } => {
            let net = network(&message.conversation).clone();
            let Some(id) = conversation(app, shared, &net, &target, source == MessageSource::Live) else {
                return;
            };
            message.conversation = id.clone();
            // Use labels only for echoes of our own pending upload; never body/time
            // matching, which would eat identical messages from another client.
            if message.outgoing
                && let Some(label) = label
            {
                let jobs = shared.irc_jobs.lock().unwrap();
                if let Some(job) = jobs.get(&label).filter(|job| job.conversation == id) {
                    message.id = MessageId(label);
                    message.attachment = job.attachment.clone();
                }
            }
            if message.outgoing && message.attachment.is_none() {
                let jobs = shared.irc_jobs.lock().unwrap();
                if let Some(job) = jobs
                    .get(&message.id.0)
                    .filter(|job| job.conversation == id)
                    .or_else(|| {
                        jobs.values().find(|job| {
                            job.conversation == id
                                && job.attachment.as_ref().is_some_and(|a| a.url == message.text)
                        })
                    })
                {
                    message.attachment = job.attachment.clone();
                }
            }
            // Sensitive service exchanges remain visible for this session only.
            let sensitive = omtalk_irc::is_sensitive(&target, &message.text);
            let outcome = if sensitive {
                Ok(SaveOutcome::Inserted)
            } else {
                shared
                    .store
                    .save_chat_message(&message, source == MessageSource::Older)
            };
            let duplicate = match outcome {
                Ok(SaveOutcome::Merged(local)) => {
                    message.id = local.clone();
                    if let Ok(Some(saved)) = shared.store.chat_message(&local) {
                        message = saved;
                    }
                    true
                }
                Ok(_) => false,
                Err(error) => {
                    app.status = format!("IRC history: {error}");
                    false
                }
            };
            let Some(i) = index(app, &id) else {
                return;
            };
            let mut line = line(&message, shared.image_links);
            prepare_media(std::slice::from_mut(&mut line), shared);
            let shown = app.irc_read_enabled
                && app.focused
                && app.selected == Some(i)
                && app.chats[i].scroll == 0
                && !app.chats[i].older_loading;
            if let Some(old) = app.chats[i]
                .lines
                .iter()
                .position(|l| l.id.as_deref() == Some(&message.id.0))
            {
                app.chats[i].lines.remove(old);
            }
            // Insert into the ordered messages without re-sorting the entire
            // transcript on every replay frame. Notices have no ordering cursor.
            let at = app.chats[i]
                .lines
                .iter()
                .rposition(|l| {
                    !l.notice
                        && l.server_time
                            .as_deref()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|t| t.to_utc())
                            .unwrap_or_else(|| chrono::DateTime::from_timestamp(l.at, 0).unwrap_or_default())
                            <= message.at
                })
                .map_or(0, |i| i + 1);
            app.chats[i].lines.insert(at, line);
            let chat = &mut app.chats[i];
            if source != MessageSource::Older
                && chat.scroll == 0
                && !chat.older_loading
                && chat.lines.len() > crate::app::SHOWN + crate::app::SHOWN / 2
            {
                chat.lines.drain(..chat.lines.len() - crate::app::SHOWN);
                chat.older_done = false;
            }
            if !duplicate
                && source == MessageSource::Live
                && !message.outgoing
                && !shown
                && !app.chats[i].muted
            {
                notify(app, shared, i, &message);
            }
            app.chats[i].unread = shared
                .store
                .unread_conversation(&id)
                .unwrap_or(app.chats[i].unread);
            if shown && source == MessageSource::Live {
                for action in app.read_irc(i) {
                    tokio::spawn(super::perform(action, shared.clone()));
                }
            }
        }
    }
}

fn notify(app: &App, shared: &Shared, i: usize, message: &ChatMessage) {
    let chat = &app.chats[i];
    let net = network(&message.conversation);
    let nick = &app.irc_networks[net].nick;
    let mention = crate::notify::mentions(&message.text, nick);
    let private = kind(&message.conversation) == ConversationKind::Direct;
    let alert = app
        .chat_notify
        .get(&(chat.account.clone(), chat.jid.clone()))
        .map_or_else(
            || app.notify.alert(private, mention),
            |p| p.alert(&app.notify, private, mention),
        );
    if alert.bell {
        use std::io::Write;
        let _ = io::stdout().write_all(b"\x07");
    }
    if alert.sound {
        crate::notify::run(&app.notify.sound);
    }
    if alert.desktop {
        let title = format!("{} · {} [IRC]", chat.title(), app.irc_name(net));
        let text = if message.kind == MessageKind::Action {
            format!("* {} {}", message.sender, message.text)
        } else {
            message.text.clone()
        };
        let account = serde_json::to_string(net).unwrap_or_default();
        let id = message.conversation.clone();
        let key = serde_json::to_string(&id).unwrap_or_default();
        let shared = shared.clone();
        tokio::spawn(async move {
            if crate::notify::read_action(&crate::notify::desktop(&account, &key, &title, None, &text, None))
                .await
            {
                send(&shared, UiEvent::Read(id));
            }
        });
    }
}

pub async fn perform(action: &Action, shared: &Shared) -> bool {
    match action {
        Action::IrcSend {
            conversation,
            target,
            id,
            kind,
            text,
        } => {
            let result = match shared.irc.session(network(conversation)) {
                Some(session) => {
                    session
                        .send(Command::Send {
                            id: MessageId(id.clone()),
                            target: target.clone(),
                            kind: *kind,
                            text: text.clone(),
                        })
                        .await
                }
                None => Err("IRC: network is unavailable".into()),
            };
            if let Err(error) = result {
                send(
                    shared,
                    UiEvent::SendFailed {
                        conversation: conversation.clone(),
                        text: text.clone(),
                        kind: *kind,
                        error,
                    },
                );
            }
        }
        Action::IrcRaw { network, line } => {
            let result = match shared.irc.session(network) {
                Some(session) => session.send(Command::Raw(line.clone())).await,
                None => Err("IRC: network is unavailable; command not sent".into()),
            };
            if let Err(error) = result {
                let _ = shared.tx.send(Msg::Status(error));
            }
        }
        Action::IrcRead {
            conversation,
            target,
            timestamp,
        } => {
            let _ = shared.store.read_conversation(conversation, timestamp);
            if let Some(session) = shared.irc.session(network(conversation))
                && session.snapshot().capabilities.read_sync
                && let Err(error) = session
                    .send(Command::Read {
                        target: target.clone(),
                        timestamp: Some(timestamp.clone()),
                    })
                    .await
            {
                let _ = shared.tx.send(Msg::Status(error));
            }
        }
        Action::IrcHistory {
            conversation,
            request,
        } => command(shared, network(conversation), Command::History(request.clone())),
        Action::SaveIrcConversation { conversation, state } => {
            let _ = shared.store.save_conversation(conversation, state);
        }
        Action::NowPlaying { chat, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation() {
                send(
                    shared,
                    UiEvent::NowPlaying {
                        conversation: id.clone(),
                        text: crate::mpris::now_playing().await,
                    },
                );
            }
        }
        Action::Upload { to, path, id, .. } if to.is_irc() => {
            if let Some(conversation) = to.conversation() {
                let (cancel, _) = tokio::sync::watch::channel(false);
                shared.irc_jobs.lock().unwrap().insert(
                    id.clone(),
                    UploadJob {
                        conversation: conversation.clone(),
                        path: path.clone(),
                        attachment: None,
                        cancel,
                        running: false,
                        sent: false,
                    },
                );
                {
                    let jobs = shared.irc_jobs.lock().unwrap();
                    if let Some(job) = jobs.get(id) {
                        keep_upload(shared, id, job);
                    }
                }
                upload(shared.clone(), id.clone()).await;
            }
        }
        Action::IrcRetry { id } => upload(shared.clone(), id.clone()).await,
        Action::IrcCancel { id } => {
            if let Some(job) = shared.irc_jobs.lock().unwrap().get_mut(id).filter(|j| !j.sent) {
                let _ = job.cancel.send(true);
                send(
                    shared,
                    UiEvent::File {
                        conversation: job.conversation.clone(),
                        id: id.clone(),
                        state: Delivery::Failed(
                            tr!("cancelled; local file kept", "отменено; локальный файл сохранён").into(),
                        ),
                        attachment: job.attachment.clone(),
                    },
                );
            }
        }
        Action::Older { chat, before, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation() {
                let page = shared
                    .store
                    .history_page(id, before.as_deref(), OLDER_PAGE)
                    .unwrap_or_default();
                if !page.messages.is_empty() {
                    send(
                        shared,
                        UiEvent::Older {
                            conversation: id.clone(),
                            lines: page
                                .messages
                                .iter()
                                .map(|m| line(m, shared.image_links))
                                .collect(),
                            done: false,
                        },
                    );
                } else if let Some(session) = shared.irc.session(network(id)) {
                    let snapshot = session.snapshot();
                    let first = shared.store.first_conversation_message(id).ok().flatten();
                    if snapshot.capabilities.history {
                        let target = shared
                            .store
                            .conversations(id.account())
                            .unwrap_or_default()
                            .into_iter()
                            .find(|(c, _)| c == id)
                            .map(|(_, s)| s.target)
                            .unwrap_or_default();
                        let cursor = first.as_ref().and_then(|m| {
                            m.server_time
                                .as_ref()
                                .map(|t| reference(&snapshot.support, t, m.server_id.as_deref()))
                        });
                        if let Err(error) = session
                            .send(Command::History(HistoryRequest {
                                target,
                                kind: if cursor.is_some() {
                                    HistoryKind::Before
                                } else {
                                    HistoryKind::Latest
                                },
                                cursor,
                            }))
                            .await
                        {
                            send(
                                shared,
                                UiEvent::Older {
                                    conversation: id.clone(),
                                    lines: vec![],
                                    done: false,
                                },
                            );
                            let _ = shared.tx.send(Msg::Status(error));
                        }
                    } else {
                        send(
                            shared,
                            UiEvent::Older {
                                conversation: id.clone(),
                                lines: vec![],
                                done: true,
                            },
                        );
                    }
                } else {
                    send(
                        shared,
                        UiEvent::Older {
                            conversation: id.clone(),
                            lines: vec![],
                            done: false,
                        },
                    );
                }
            }
        }
        Action::Search { chat, words, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation() {
                let found = shared
                    .store
                    .search_conversation(id, words, SEARCH_LIMIT)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| crate::app::Found {
                        at: m.at.timestamp(),
                        who: m.sender,
                        text: m.text,
                    })
                    .collect();
                send(
                    shared,
                    UiEvent::Found {
                        conversation: id.clone(),
                        words: words.clone(),
                        found,
                    },
                );
            }
        }
        Action::Jump { chat, at, .. } if chat.is_irc() => {
            if let Some(id) = chat.conversation() {
                let lines = shared
                    .store
                    .conversation_from_time(id, *at, JUMP_LIMIT)
                    .unwrap_or_default()
                    .iter()
                    .map(|m| line(m, shared.image_links))
                    .collect();
                send(
                    shared,
                    UiEvent::Jump {
                        conversation: id.clone(),
                        at: *at,
                        lines,
                    },
                );
            }
        }
        Action::ReopenChat { jid, .. } if jid.is_irc() => {}
        _ => return false,
    }
    true
}

#[cfg(test)]
#[path = "irc_tests.rs"]
mod tests;

async fn upload(shared: Shared, id: String) {
    let (conversation, path, ready, mut cancel) = {
        let mut jobs = shared.irc_jobs.lock().unwrap();
        let Some(job) = jobs.get_mut(&id) else {
            return;
        };
        if job.running || job.sent {
            return;
        }
        job.running = true;
        job.cancel.send_replace(false);
        (
            job.conversation.clone(),
            job.path.clone(),
            job.attachment.clone(),
            job.cancel.subscribe(),
        )
    };
    let result = tokio::select! {
        _ = cancel.changed() => Err(tr!("cancelled; local file kept", "отменено; локальный файл сохранён").to_owned()),
        result = upload_and_send(&shared,&conversation,&id,&path,ready) => result,
    };
    let mut jobs = shared.irc_jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(&id) {
        job.running = false;
        match result {
            Ok(()) => job.sent = true,
            Err(error) => send(
                &shared,
                UiEvent::File {
                    conversation,
                    id: id.clone(),
                    state: Delivery::Failed(error),
                    attachment: job.attachment.clone(),
                },
            ),
        }
        keep_upload(&shared, &id, job);
    }
}

async fn upload_and_send(
    shared: &Shared,
    conversation: &ConversationId,
    id: &str,
    path: &std::path::Path,
    ready: Option<Attachment>,
) -> Result<(), String> {
    let net = network(conversation);
    let session = shared
        .irc
        .session(net)
        .ok_or("IRC: network removed; local file kept")?;
    let snapshot = session.snapshot();
    let attachment = if let Some(attachment) = ready {
        attachment
    } else {
        let endpoint = snapshot
            .support
            .filehost
            .as_deref()
            .ok_or("IRC: this server does not advertise FILEHOST; local file kept")?;
        let config = shared.irc.config(&net.account.id).ok_or("IRC: profile removed")?;
        let auth = config
            .sasl
            .as_ref()
            .map(|s| (config.login_username(), s.password.clone()));
        let ca = config
            .ca_certificate
            .as_ref()
            .map(std::fs::read)
            .transpose()
            .map_err(|_| "IRC: cannot read FILEHOST CA certificate")?;
        let host = omtalk_media::filehost::FileHost::new(endpoint, snapshot.tls, auth, ca.as_deref())
            .map_err(|e| e.to_string())?;
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let mime = content_type(name).unwrap_or("application/octet-stream");
        let (tx, conversation, id) = (shared.tx.clone(), conversation.clone(), id.to_owned());
        host.upload(path, mime, move |sent, total| {
            let _ = tx.send(Msg::Irc(UiEvent::File {
                conversation: conversation.clone(),
                id: id.clone(),
                state: Delivery::Uploading(
                    sent.saturating_mul(100)
                        .checked_div(total)
                        .unwrap_or(100)
                        .min(100) as u8,
                ),
                attachment: None,
            }));
        })
        .await
        .map_err(|e| e.to_string())?
    };
    if let Some(job) = shared.irc_jobs.lock().unwrap().get_mut(id) {
        job.attachment = Some(attachment.clone());
        keep_upload(shared, id, job);
    }
    send(
        shared,
        UiEvent::File {
            conversation: conversation.clone(),
            id: id.into(),
            state: Delivery::Sending,
            attachment: Some(attachment.clone()),
        },
    );
    let state = shared
        .store
        .conversations(conversation.account())
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|(c, _)| c == conversation)
        .map(|(_, s)| s)
        .ok_or("IRC: original conversation is unavailable; uploaded URL kept for retry")?;
    if state.hidden || (kind(conversation) == ConversationKind::Channel && !state.joined) {
        return Err("IRC: original channel was closed or left; uploaded URL kept for retry".into());
    }
    session
        .send(Command::Send {
            id: MessageId(id.into()),
            target: state.target,
            kind: MessageKind::Text,
            text: attachment.url,
        })
        .await?;
    // A reliable echo replaces this line via its label; without labels remove the
    // progress row and let the ordinary server echo be the sole visible message.
    send(
        shared,
        UiEvent::File {
            conversation: conversation.clone(),
            id: id.into(),
            state: Delivery::Sent,
            attachment: None,
        },
    );
    Ok(())
}
