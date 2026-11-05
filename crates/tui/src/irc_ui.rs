//! IRC context in the existing omtalk model, input and panels.
use super::*;
use omtalk_core::{Capabilities, ConversationId, ConversationKind, MessageKind, Network, NetworkId};

#[derive(Debug, Clone)]
pub struct IrcNetworkView {
    pub name: String,
    pub profile: String,
    pub nick: String,
    pub upstream: String,
    pub support: omtalk_irc::Support,
    pub caps: Capabilities,
    pub tls: bool,
    pub connected: bool,
    pub registered: bool,
    pub removed: bool,
}

impl App {
    pub fn irc_upload_error(&self, index: usize) -> Option<&'static str> {
        let chat = self.chats.get(index)?;
        let network = chat.account.network()?;
        let view = self.irc_networks.get(network)?;
        if !view.connected || view.removed || matches!(view.upstream.as_str(), "connecting" | "disconnected")
        {
            return Some(tr!(
                "IRC is disconnected; local file kept",
                "IRC отключён; локальный файл сохранён"
            ));
        }
        if !view.caps.upload {
            return Some(tr!(
                "this IRC server has no FILEHOST upload service",
                "этот IRC-сервер не предоставляет загрузку FILEHOST"
            ));
        }
        if chat.is_room()
            && !chat
                .jid
                .conversation()
                .and_then(|id| self.irc_conversations.get(id))
                .is_some_and(|s| s.joined && !s.hidden)
        {
            return Some(tr!(
                "join the original channel first; local file kept",
                "сначала войдите в исходный канал; файл сохранён"
            ));
        }
        None
    }
    pub fn add_irc_network(&mut self, network: NetworkId, profile: String, name: String, tls: bool) {
        self.irc_networks
            .entry(network.clone())
            .or_insert_with(|| IrcNetworkView {
                name,
                profile,
                nick: String::new(),
                upstream: String::new(),
                support: Default::default(),
                caps: Default::default(),
                tls,
                connected: false,
                registered: false,
                removed: false,
            });
        let address = Address::Network(network);
        if !self.accounts.contains(&address) {
            self.accounts.push(address.clone());
        }
        self.connection
            .entry(address.clone())
            .or_insert(Connection::Connecting);
        self.server_chat(&address);
    }

    pub fn irc_name(&self, network: &NetworkId) -> String {
        self.irc_networks
            .get(network)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| network.account.id.clone())
    }

    pub fn irc_target(&self, index: usize) -> Option<(ConversationId, String)> {
        let id = self.chats.get(index)?.jid.conversation()?.clone();
        let target = self.irc_conversations.get(&id)?.target.clone();
        Some((id, target))
    }

    pub fn ensure_irc_chat(
        &mut self,
        network: &NetworkId,
        target: &str,
        kind: ConversationKind,
        reveal: bool,
    ) -> Option<usize> {
        let id = self.irc_conversation_id(network, target, kind)?;
        let view = self.irc_networks.get(network)?;
        // A query's original local ID survives a witnessed NICK. An existing query
        // with the new nick is not merged: that could belong to somebody else.
        let state =
            self.irc_conversations
                .entry(id.clone())
                .or_insert_with(|| omtalk_store::ConversationState {
                    target: target.into(),
                    ..Default::default()
                });
        if state.hidden && !reveal {
            return None;
        }
        if reveal {
            state.hidden = false;
        }
        let muted = state.muted;
        let folder = state.folder.clone();
        let nick = view.nick.clone();
        let kind = match kind {
            ConversationKind::Channel => ChatKind::Room { nick },
            ConversationKind::Server => ChatKind::Server,
            _ => ChatKind::Direct,
        };
        let index = self.chat(&network.clone().into(), &id.clone().into(), kind);
        self.chats[index].name = Some(target.to_owned());
        self.chats[index].omemo = false;
        self.chats[index].muted = muted;
        if let Some(name) = folder {
            if let Some(folder) = self
                .folders
                .iter_mut()
                .find(|f| f.account.network() == Some(network) && f.name == name)
            {
                if !folder.chats.iter().any(|c| c.conversation() == Some(&id)) {
                    folder.chats.push(id.into());
                }
            } else {
                self.folders.push(crate::address::Folder {
                    account: network.clone().into(),
                    name,
                    chats: vec![id.into()],
                });
            }
        }
        Some(index)
    }

    pub fn irc_conversation_id(
        &self,
        network: &NetworkId,
        target: &str,
        kind: ConversationKind,
    ) -> Option<ConversationId> {
        let mapping = self.irc_networks.get(network)?.support.case_mapping;
        if let Some((id, _)) = self.irc_conversations.iter().find(|(id, state)| {
            matches!(id, ConversationId::Irc {network:n,kind:k,..} if n==network && *k==kind)
                && mapping.equivalent(&state.target, target)
        }) {
            return Some(id.clone());
        }
        let mut id = ConversationId::irc(network.clone(), kind, target, mapping);
        // The old folded spelling is a stable *local* ID. After a change to a
        // finer CASEMAPPING it may collide with a new, distinct wire target.
        let base = mapping.fold(target);
        let mut suffix = 0;
        while self.irc_conversations.contains_key(&id) {
            suffix += 1;
            if let ConversationId::Irc { target, .. } = &mut id {
                *target = format!("{base}\0{suffix}");
            }
        }
        Some(id)
    }

    pub fn open_irc_query(&mut self, network: &NetworkId, target: &str) -> Vec<Action> {
        let kind = if self
            .irc_networks
            .get(network)
            .is_some_and(|n| n.support.is_channel(target))
        {
            ConversationKind::Channel
        } else if target.eq_ignore_ascii_case("BouncerServ") {
            ConversationKind::Control
        } else {
            ConversationKind::Direct
        };
        let Some(index) = self.ensure_irc_chat(network, target, kind, true) else {
            return Vec::new();
        };
        let mut actions = self.select(index);
        actions.extend(self.save_irc_state(index));
        if kind == ConversationKind::Direct {
            let (conversation, _) = self.irc_target(index).unwrap();
            if self.irc_networks.get(network).is_some_and(|n| n.caps.history) {
                actions.push(Action::IrcHistory {
                    conversation,
                    request: omtalk_irc::HistoryRequest {
                        target: target.into(),
                        kind: omtalk_irc::HistoryKind::Latest,
                        cursor: None,
                    },
                });
            }
            if self.irc_networks.get(network).is_some_and(|n| n.caps.read_sync) {
                actions.push(Action::IrcRaw {
                    network: network.clone(),
                    line: format!("MARKREAD {target}"),
                });
            }
        }
        actions
    }

    pub fn save_irc_state(&self, index: usize) -> Option<Action> {
        let (conversation, _) = self.irc_target(index)?;
        let state = self.irc_conversations.get(&conversation)?.clone();
        Some(Action::SaveIrcConversation { conversation, state })
    }

    pub(super) fn close_irc_chat(&mut self, index: usize) -> Vec<Action> {
        if self.chats[index].kind == ChatKind::Server {
            return Vec::new();
        }
        let Some((id, _)) = self.irc_target(index) else {
            return Vec::new();
        };
        let state = self.irc_conversations.get_mut(&id).unwrap();
        state.hidden = true;
        let action = Action::SaveIrcConversation {
            conversation: id,
            state: state.clone(),
        };
        self.remove_chat(index);
        vec![action]
    }

    pub fn read_irc(&mut self, index: usize) -> Vec<Action> {
        if !self.irc_read_enabled || self.chats[index].scroll != 0 || self.chats[index].older_loading {
            return Vec::new();
        }
        self.read_irc_explicit(index)
    }

    pub fn read_irc_explicit(&mut self, index: usize) -> Vec<Action> {
        let Some((id, target)) = self.irc_target(index) else {
            return Vec::new();
        };
        let chat = &mut self.chats[index];
        let Some(time) = chat
            .lines
            .iter()
            .rev()
            .filter(|l| {
                !l.notice
                    && !matches!(
                        l.delivery,
                        Some(Delivery::Uploading(_) | Delivery::Sending | Delivery::Failed(_))
                    )
            })
            .find_map(|l| {
                l.server_time.clone().or_else(|| {
                    chrono::DateTime::from_timestamp(l.at, 0)
                        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                })
            })
        else {
            return Vec::new();
        };
        let forward = chat.read_mark.as_ref().is_none_or(|old| {
            chrono::DateTime::parse_from_rfc3339(&time).ok() > chrono::DateTime::parse_from_rfc3339(old).ok()
        });
        if !forward {
            return Vec::new();
        }
        chat.read_mark = Some(time.clone());
        chat.unread = 0;
        vec![Action::IrcRead {
            conversation: id,
            target,
            timestamp: time,
        }]
    }

    pub fn send_irc(&mut self, index: usize, text: String, kind: MessageKind) -> Vec<Action> {
        let Some((conversation, target)) = self.irc_target(index) else {
            self.refused = true;
            return self.say(tr!("only commands here", "здесь только команды"));
        };
        let Some(network) = self.chats[index].account.network() else {
            return Vec::new();
        };
        if !self.irc_networks.get(network).is_some_and(|n| {
            n.connected && !n.removed && n.upstream != "disconnected" && n.upstream != "connecting"
        }) {
            self.refused = true;
            return self.say(tr!(
                "IRC is disconnected; message was not queued",
                "IRC не подключён; сообщение не поставлено в очередь"
            ));
        }
        if matches!(
            conversation,
            ConversationId::Irc {
                kind: ConversationKind::Channel,
                ..
            }
        ) && !self
            .irc_conversations
            .get(&conversation)
            .is_some_and(|s| s.joined && !s.hidden)
        {
            self.refused = true;
            return self.say(tr!(
                "join this channel before sending",
                "перед отправкой войдите в канал"
            ));
        }
        let text = if kind == MessageKind::Action {
            action_of(text.trim_start()).unwrap_or(&text).to_owned()
        } else {
            text
        };
        let id = match omtalk_core::MessageId::new() {
            Ok(id) => id.0,
            Err(_) => {
                self.refused = true;
                return self.say("IRC: cannot create message ID");
            }
        };
        // Without a reliable label the server echo is the visible own message. A
        // speculative local copy could duplicate another client's identical text.
        self.chats[index].scroll = 0;
        vec![Action::IrcSend {
            conversation,
            target,
            id,
            kind,
            text,
        }]
    }

    pub(super) fn sensitive_irc_input(&self, text: &str) -> bool {
        let mut words = text.split_whitespace().skip(1);
        let mut irc_selected = false;
        while let Some(flag) = words.next() {
            if !matches!(flag, "--account" | "--network") {
                break;
            }
            let Some(value) = words.next() else {
                break;
            };
            irc_selected |= flag == "--account" && self.irc_networks.keys().any(|n| n.account.id == value);
        }
        (irc_selected || self.current().is_some_and(|c| c.account.is_irc()))
            && (self
                .selected
                .and_then(|i| self.irc_target(i))
                .is_some_and(|(_, target)| omtalk_irc::is_sensitive(&target, text))
                || omtalk_irc::is_sensitive("", text)
                || text
                    .split_whitespace()
                    .any(|w| w.to_ascii_lowercase().ends_with("serv")))
    }

    /// IRC selectors are parsed before protocol arguments. Only a prefix is consumed;
    /// text after the target, including literal --account and spaces, stays intact.
    pub(super) fn irc_command(&mut self, line: &str) -> Option<Vec<Action>> {
        let (name, mut rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let normalized = name.to_ascii_lowercase();
        let name = normalized.as_str();
        rest = rest.trim_start();
        let mut network = self.account_for().and_then(|a| a.network().cloned());
        let mut account_selector = None;
        let mut network_selector = None;
        while rest.starts_with("--account ") || rest.starts_with("--network ") {
            let (flag, tail) = rest.split_once(char::is_whitespace).unwrap();
            let tail = tail.trim_start();
            let (value, tail) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
            let slot = if flag == "--account" {
                &mut account_selector
            } else {
                &mut network_selector
            };
            if slot.is_some() {
                return Some(self.say(tr!("duplicate selector", "селектор указан дважды")));
            }
            *slot = Some(value);
            rest = tail.trim_start();
        }
        if let Some(account) = account_selector {
            let networks: Vec<_> = self
                .irc_networks
                .keys()
                .filter(|n| n.account.id == account)
                .cloned()
                .collect();
            if networks.is_empty() {
                return None;
            } // existing --account JID path
            network = networks
                .iter()
                .find(|n| n.network == Network::Bouncer)
                .cloned()
                .or_else(|| networks.first().cloned());
        }
        let network = network?;
        let mut candidates: Vec<_> = self
            .irc_networks
            .iter()
            .filter(|(n, view)| {
                n.account == network.account && !view.removed && n.network != Network::Bouncer
            })
            .filter(|(n, view)| {
                network_selector.is_none_or(|selector| {
                    view.name == selector || matches!(&n.network, Network::Soju(id) if id == selector)
                })
            })
            .map(|(n, _)| n.clone())
            .collect();
        let common = crate::complete::owner(name) == crate::complete::Owner::Common;
        if common && account_selector.is_none() && network_selector.is_none() {
            return None;
        }
        if crate::complete::owner(name) == crate::complete::Owner::Xmpp {
            self.refused = true;
            return Some(self.say(tr!(
                "this command is only available in XMPP",
                "эта команда доступна только в XMPP"
            )));
        }
        let network = if matches!(name, "/bouncer" | "/networks")
            || (matches!(name, "/msg" | "/query")
                && rest
                    .split_whitespace()
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case("BouncerServ")))
        {
            if self.irc_networks.contains_key(&NetworkId {
                account: network.account.clone(),
                network: Network::Bouncer,
            }) {
                NetworkId {
                    account: network.account.clone(),
                    network: Network::Bouncer,
                }
            } else {
                network
            }
        } else if network_selector.is_some()
            || (network.network == Network::Bouncer
                && matches!(
                    name,
                    "/join"
                        | "/part"
                        | "/leave"
                        | "/detach"
                        | "/msg"
                        | "/query"
                        | "/notice"
                        | "/nick"
                        | "/topic"
                        | "/away"
                        | "/online"
                        | "/whois"
                        | "/names"
                        | "/invite"
                        | "/kick"
                        | "/mode"
                        | "/ns"
                        | "/nickserv"
                        | "/cs"
                        | "/chanserv"
                        | "/ms"
                        | "/memoserv"
                        | "/hs"
                        | "/hostserv"
                        | "/os"
                        | "/operserv"
                ))
        {
            if candidates.len() != 1 {
                self.refused = true;
                return Some(self.say(tr!(
                    "choose a network with --network ID or open one of its chats",
                    "выберите сеть через --network ID или откройте её чат"
                )));
            }
            candidates.remove(0)
        } else {
            network
        };
        if common {
            return Some(self.say(tr!(
                "this local command does not take network selectors",
                "локальная команда не принимает селекторы сети"
            )));
        }
        let current = self.selected.and_then(|i| self.irc_target(i));
        let current_target = current
            .as_ref()
            .filter(|(id, _)| matches!(id,ConversationId::Irc{network:n,..}if n==&network))
            .map(|(_, t)| t.as_str());
        let (first, tail) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let tail = tail.trim_start();
        let raw = |line| {
            vec![Action::IrcRaw {
                network: network.clone(),
                line,
            }]
        };
        let actions = match name {
            "/ns" | "/nickserv" | "/cs" | "/chanserv" | "/ms" | "/memoserv" | "/hs" | "/hostserv" | "/os"
            | "/operserv" => {
                let service = match name {
                    "/ns" | "/nickserv" => "NickServ",
                    "/cs" | "/chanserv" => "ChanServ",
                    "/ms" | "/memoserv" => "MemoServ",
                    "/hs" | "/hostserv" => "HostServ",
                    _ => "OperServ",
                };
                let mut actions = self.open_irc_query(&network, service);
                if !rest.is_empty()
                    && let Some(index) = self.selected
                {
                    actions.extend(self.send_irc(index, rest.into(), MessageKind::Text));
                }
                actions
            }
            "/bouncer" if rest.is_empty() => self.open_irc_query(&network, "BouncerServ"),
            "/msg" | "/query" | "/notice" if valid_target(first) => {
                let mut actions = self.open_irc_query(&network, first);
                if !tail.is_empty()
                    && let Some(index) = self.selected
                {
                    actions.extend(self.send_irc(
                        index,
                        tail.into(),
                        if name == "/notice" {
                            MessageKind::Notice
                        } else {
                            MessageKind::Text
                        },
                    ));
                }
                actions
            }
            "/join"
                if valid_target(first)
                    && !first.contains(',')
                    && (tail.is_empty() || valid_target(tail))
                    && self.irc_networks[&network].support.is_channel(first) =>
            {
                let mut actions = self.open_irc_query(&network, first);
                actions.extend(raw(format!(
                    "JOIN {first}{}",
                    if tail.is_empty() {
                        String::new()
                    } else {
                        format!(" {tail}")
                    }
                )));
                actions
            }
            "/nick" if valid_target(first) && tail.is_empty() => raw(format!("NICK {first}")),
            "/topic" => match current_target {
                Some(target) if self.irc_networks[&network].support.is_channel(target) => {
                    raw(if rest.is_empty() {
                        format!("TOPIC {target}")
                    } else {
                        format!("TOPIC {target} :{rest}")
                    })
                }
                _ => self.say(tr!("open an IRC channel", "откройте IRC-канал")),
            },
            "/part" | "/leave" | "/detach" => {
                let target = if self.irc_networks[&network].support.is_channel(first) {
                    first
                } else {
                    current_target.unwrap_or("")
                };
                if !self.irc_networks[&network].support.is_channel(target) {
                    self.say(tr!("open an IRC channel", "откройте IRC-канал"))
                } else if name == "/detach"
                    && !matches!(network.network, Network::Soju(_) | Network::Selected)
                {
                    self.say(tr!("detach requires soju", "detach доступен только в soju"))
                } else {
                    raw(if name == "/detach" {
                        format!("PART {target} :detach")
                    } else {
                        format!(
                            "PART {target}{}",
                            if rest.is_empty() || rest == target {
                                String::new()
                            } else {
                                format!(" :{}", if first == target { tail } else { rest })
                            }
                        )
                    })
                }
            }
            "/away" => raw(if rest.is_empty() {
                "AWAY :away".into()
            } else {
                format!("AWAY :{rest}")
            }),
            "/online" => raw("AWAY".into()),
            "/whois" if valid_target(first) => raw(format!("WHOIS {first}")),
            "/names" => raw(format!(
                "NAMES {}",
                if rest.is_empty() {
                    current_target.unwrap_or("")
                } else {
                    rest
                }
            )),
            "/invite" if valid_target(first) => {
                let channel = if tail.is_empty() {
                    current_target.unwrap_or("")
                } else {
                    tail
                };
                if self.irc_networks[&network].support.is_channel(channel) {
                    raw(format!("INVITE {first} {channel}"))
                } else {
                    self.say("/invite NICK #CHANNEL")
                }
            }
            "/kick" => {
                if let Some(channel) = current_target {
                    raw(format!("KICK {channel} {first} :{tail}"))
                } else {
                    self.say("/kick NICK [REASON]")
                }
            }
            "/mode" => {
                if first.starts_with(['+', '-']) {
                    match current_target {
                        Some(channel) => raw(format!("MODE {channel} {rest}")),
                        None => self.say("/mode TARGET MODES"),
                    }
                } else {
                    raw(format!("MODE {rest}"))
                }
            }
            "/raw" | "/quote" if !rest.is_empty() => raw(rest.into()),
            "/motd" | "/list" | "/version" | "/time" | "/stats" | "/who" => raw(format!(
                "{}{}",
                name[1..].to_ascii_uppercase(),
                if rest.is_empty() {
                    String::new()
                } else {
                    format!(" {rest}")
                }
            )),
            "/networks" => raw("BOUNCER LISTNETWORKS".into()),
            "/retry" | "/cancel" => {
                let id = self.current().and_then(|c| {
                    c.lines
                        .iter()
                        .rev()
                        .find(|l| {
                            matches!(
                                l.delivery,
                                Some(Delivery::Failed(_) | Delivery::Uploading(_) | Delivery::Sending)
                            )
                        })
                        .and_then(|l| l.id.clone())
                });
                match id {
                    Some(id) => vec![if name == "/retry" {
                        Action::IrcRetry { id }
                    } else {
                        Action::IrcCancel { id }
                    }],
                    None => self.say(tr!("no pending file", "нет ожидающего отправки файла")),
                }
            }
            _ if self.current().is_some_and(|c| c.kind == ChatKind::Server)
                && !crate::complete::IRC_COMMANDS.iter().any(|c| c.name == name)
                && name
                    .strip_prefix('/')
                    .is_some_and(|cmd| !cmd.is_empty() && cmd.bytes().all(|b| b.is_ascii_alphabetic())) =>
            {
                raw(format!(
                    "{}{}",
                    name[1..].to_ascii_uppercase(),
                    if rest.is_empty() {
                        String::new()
                    } else {
                        format!(" {rest}")
                    }
                ))
            }
            _ => {
                self.refused = true;
                self.say(tr!(
                    "unknown command or invalid arguments; /help",
                    "неизвестная команда или неверные аргументы; /help"
                ))
            }
        };
        Some(actions)
    }
}

fn valid_target(s: &str) -> bool {
    !s.is_empty() && !s.starts_with(':') && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_commands_and_service_aliases_use_the_selected_network() {
        let (mut app, a, b) = fixture();
        for network in [a, b] {
            let server = app.server_chat(&network.clone().into());
            for (input, expected) in [
                ("/motd", "MOTD"),
                ("/MOTD", "MOTD"),
                ("MOTD", "MOTD"),
                ("motd", "MOTD"),
                ("/version", "VERSION"),
                ("/lusers", "LUSERS"),
            ] {
                app.select(server);
                assert!(submit(&mut app, input).iter().any(|action| matches!(action,
                    Action::IrcRaw { network: n, line } if n == &network && line == expected)));
            }
            for alias in ["/ns", "/nickserv", "/NS", "/NickServ", "NickServ", "ns"] {
                app.select(server);
                let actions = submit(&mut app, &format!("{alias} HELP REGISTER"));
                assert!(actions.iter().any(|action| matches!(action,
                    Action::IrcSend { conversation: ConversationId::Irc { network: n, .. }, target, text, .. }
                    if n == &network && target == "NickServ" && text == "HELP REGISTER")));
                assert!(app.sensitive_irc_input(&format!("{alias} SET PASSWORD example")));
            }
        }
    }
    use crate::config::Modifier;
    fn fixture() -> (App, NetworkId, NetworkId) {
        let mut app = App::new(
            vec![Address::Xmpp("me@xmpp.test".parse().unwrap())],
            Modifier::Ctrl,
            26,
            30,
        );
        let a = NetworkId {
            account: omtalk_core::AccountId::irc("home"),
            network: Network::Soju("1".into()),
        };
        let b = NetworkId {
            network: Network::Soju("2".into()),
            ..a.clone()
        };
        for (n, name) in [(&a, "Alpha"), (&b, "Beta")] {
            app.add_irc_network(n.clone(), "Home".into(), name.into(), true);
            let v = app.irc_networks.get_mut(n).unwrap();
            v.connected = true;
            v.nick = "self".into();
            v.caps.history = true;
            let i = app
                .ensure_irc_chat(n, "#same", ConversationKind::Channel, true)
                .unwrap();
            let id = app.chats[i].jid.conversation().unwrap().clone();
            app.irc_conversations.get_mut(&id).unwrap().joined = true;
        }
        let i = app
            .ensure_irc_chat(&a, "#same", ConversationKind::Channel, true)
            .unwrap();
        app.select(i);
        (app, a, b)
    }
    fn submit(app: &mut App, text: &str) -> Vec<Action> {
        app.input = text.into();
        app.cursor = text.chars().count();
        app.submit()
    }

    #[test]
    fn irc_intent_selectors_and_unknown_commands_keep_the_correct_context() {
        let (mut app, a, b) = fixture();
        for (text, kind, body) in [
            ("/me waves", MessageKind::Action, "waves"),
            ("//me waves", MessageKind::Text, "/me waves"),
            ("::colon", MessageKind::Text, ":colon"),
        ] {
            let actions = submit(&mut app, text);
            assert!(
                matches!(&actions[..],[Action::IrcSend {conversation,kind:k,text,..}] if conversation.network()==Some(&a)&&*k==kind&&text==body),
                "{actions:?}"
            );
        }
        let actions = submit(
            &mut app,
            "/msg --account home --network 2 friend hello  --account literal",
        );
        assert!(actions.iter().any(|a|matches!(a,Action::IrcSend{conversation,text,..}if conversation.network()==Some(&b)&&text=="hello  --account literal")));
        for text in [
            "/joim #c",
            "/devices",
            "/keys",
            "/commands",
            "/services",
            "/dnd",
            "/msg --network unknown friend hi",
            "/join #one,#two",
            "/join #one key extra",
        ] {
            assert!(submit(&mut app, text).is_empty());
            assert_eq!(app.input, text);
        }
        assert!(app.keys.is_none() && app.services.is_none());
    }

    #[test]
    fn local_hide_part_detach_and_join_have_separate_semantics() {
        let (mut app, a, _) = fixture();
        let id = app.current().unwrap().jid.conversation().unwrap().clone();
        let actions = submit(&mut app, "/close");
        assert!(actions.iter().all(|a| !matches!(a, Action::IrcRaw { .. })));
        assert!(app.irc_conversations[&id].hidden && app.irc_conversations[&id].joined);
        assert!(
            app.ensure_irc_chat(&a, "#same", ConversationKind::Channel, false)
                .is_none()
        );
        let actions = submit(&mut app, "/join --account home --network 1 #same");
        assert!(
            actions
                .iter()
                .any(|a| matches!(a,Action::IrcRaw{line,..}if line=="JOIN #same"))
        );
        assert!(!app.irc_conversations[&id].hidden);
        assert!(
            submit(&mut app, "/part")
                .iter()
                .any(|a| matches!(a,Action::IrcRaw{line,..}if line=="PART #same"))
        );
        assert!(
            submit(&mut app, "/detach")
                .iter()
                .any(|a| matches!(a,Action::IrcRaw{line,..}if line=="PART #same :detach"))
        );
        assert_eq!(submit(&mut app, "/quit"), vec![Action::Quit]);
    }

    #[test]
    fn casemap_changes_never_reuse_another_conversations_local_key() {
        let (mut app, a, _) = fixture();
        let i = app
            .ensure_irc_chat(&a, "[Friend]", ConversationKind::Direct, true)
            .unwrap();
        let old = app.chats[i].jid.clone();
        app.irc_networks.get_mut(&a).unwrap().support.case_mapping = omtalk_core::CaseMapping::Ascii;
        let j = app
            .ensure_irc_chat(&a, "{Friend}", ConversationKind::Direct, true)
            .unwrap();
        assert_ne!(old, app.chats[j].jid);
        assert_eq!(
            app.irc_conversations[old.conversation().unwrap()].target,
            "[Friend]"
        );
    }

    #[test]
    fn completion_does_not_mix_protocols_or_networks_and_drafts_survive_switches() {
        let (mut app, a, b) = fixture();
        app.open_irc_query(&a, "alice");
        app.open_irc_query(&b, "bob");
        app.input = "/msg --account home --network 1 a".into();
        app.cursor = app.input.chars().count();
        let context = app.completion_context();
        assert!(context.irc);
        assert!(context.people.contains(&"alice".into()));
        assert!(!context.people.contains(&"bob".into()));
        app.input = "draft beta".into();
        let i = app
            .ensure_irc_chat(&a, "#same", ConversationKind::Channel, true)
            .unwrap();
        app.select(i);
        app.input = "draft alpha".into();
        app.open_irc_query(&b, "bob");
        assert_eq!(app.input, "draft beta");
        app.select(i);
        assert_eq!(app.input, "draft alpha");
    }

    #[test]
    fn restoring_a_window_and_scrolling_history_do_not_mark_it_read() {
        let (mut app, _, _) = fixture();
        let i = app.selected.unwrap();
        let mut line = Line::message(1, "friend".into(), "unread".into(), false, false);
        line.irc = true;
        line.server_time = Some("2026-11-05T12:00:00.123456789Z".into());
        app.chats[i].lines.push(line);
        app.chats[i].unread = 1;
        assert!(app.read_irc(i).is_empty());
        assert_eq!(app.chats[i].unread, 1);
        app.irc_read_enabled = true;
        app.chats[i].scroll = 1;
        assert!(app.read_irc(i).is_empty());
        app.chats[i].scroll = 0;
        assert!(
            matches!(&app.read_irc(i)[..],[Action::IrcRead{timestamp,..}]if timestamp=="2026-11-05T12:00:00.123456789Z")
        );
        assert!(app.read_irc(i).is_empty());
    }

    #[test]
    fn pinned_chats_remain_above_collapsed_profiles_and_networks_without_duplicates() {
        let (mut app, a, b) = fixture();
        app.add_irc_network(
            NetworkId {
                account: a.account.clone(),
                network: Network::Bouncer,
            },
            "Home".into(),
            "Control".into(),
            true,
        );
        let i = app.selected.unwrap();
        submit(&mut app, "/pin");
        assert!(app.is_pinned(i));
        for group in [
            Group::Profile(a.account.clone()),
            Group::Account(a.clone().into()),
            Group::Account(b.into()),
        ] {
            app.toggle_group(&group);
        }
        let tree = app.tree();
        assert_eq!(tree[0], (Entry::Heading(Group::Pinned), 0));
        assert_eq!(tree[1], (Entry::Chat(i), 1));
        assert_eq!(tree.iter().filter(|(e, _)| *e == Entry::Chat(i)).count(), 1);
        app.select(i);
        assert!(app.collapsed.contains(&Group::Profile(a.account.clone())));
        assert!(app.collapsed.contains(&Group::Account(a.clone().into())));
        submit(&mut app, "/unpin");
        assert!(!app.visible_chats().contains(&i));
        app.toggle_group(&Group::Profile(a.account.clone()));
        app.toggle_group(&Group::Account(a.into()));
        assert!(app.visible_chats().contains(&i));
    }
}
