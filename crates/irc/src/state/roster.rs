use super::*;

#[derive(Default)]
pub(super) struct Channel {
    name: String,
    members: BTreeMap<String, Participant>,
    names: bool,
    seen_during_names: BTreeSet<String>,
    modes_during_names: BTreeSet<String>,
    // Keep original spelling: CASEMAPPING can change during a NAMES burst.
    removed_during_names: BTreeSet<String>,
}

impl State {
    fn presence(&self, e: &mut Effects, m: &Message, channel: String, text: String) {
        let at = wire::tag(m, "time")
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .map(|time| time.to_utc())
            .unwrap_or_else(chrono::Utc::now);
        e.events.push(Event::Presence {
            network: self.network.clone(),
            channel,
            text: wire::plain(&text),
            at,
        });
    }

    pub(super) fn rekey_roster(&mut self, e: &mut Effects) {
        let mapping = self.support.case_mapping;
        let channels = std::mem::take(&mut self.channels);
        for (_, mut channel) in channels {
            channel.members = channel
                .members
                .into_values()
                .map(|p| (mapping.fold(&p.nick), p))
                .collect();
            self.channels.insert(mapping.fold(&channel.name), channel);
        }
        // Rebuild from the server if a coarser earlier mapping had collapsed two
        // members. This is a query only, never a JOIN (senpai b209af1).
        for (key, channel) in &self.channels {
            e.send.push(wire::command("NAMES", &[&channel.name]));
            self.roster_event(key, e);
        }
        self.read_frontiers.clear();
    }

    fn roster_event(&self, key: &str, e: &mut Effects) {
        if let Some(channel) = self.channels.get(key) {
            let mut participants: Vec<_> = channel.members.values().cloned().collect();
            for p in &mut participants {
                p.prefixes = self
                    .support
                    .prefix_symbols
                    .chars()
                    .filter(|c| p.prefixes.contains(*c))
                    .collect();
            }
            participants.sort_by_key(|p| {
                (
                    self.support.rank(&p.prefixes),
                    self.support.case_mapping.fold(&p.nick),
                )
            });
            e.events.push(Event::Participants {
                network: self.network.clone(),
                channel: channel.name.clone(),
                participants,
            });
        }
    }

    pub(super) fn roster_message(
        &mut self,
        m: &Message,
        cmd: &str,
        args: &[String],
        e: &mut Effects,
    ) -> bool {
        let who = m.source_nickname().unwrap_or_default();
        let mapping = self.support.case_mapping;
        let own = mapping.equivalent(who, &self.nick);
        let actor = mapping.fold(who);
        match (cmd, args) {
            ("JOIN", [name, ..]) => {
                let key = mapping.fold(name);
                let channel = self.channels.entry(key.clone()).or_default();
                if own {
                    *channel = Channel {
                        name: name.clone(),
                        names: true,
                        ..Channel::default()
                    };
                }
                channel.name = name.clone();
                channel.members.insert(
                    actor.clone(),
                    Participant {
                        nick: display_text(who),
                        ..Participant::default()
                    },
                );
                channel
                    .removed_during_names
                    .retain(|n| !mapping.equivalent(n, who));
                if channel.names {
                    channel.seen_during_names.insert(who.into());
                }
                if own {
                    e.events.push(Event::Joined {
                        network: self.network.clone(),
                        channel: name.clone(),
                    });
                } else {
                    self.presence(e, m, name.clone(), format!("JOIN {who}"));
                }
                self.roster_event(&key, e);
            }
            ("353", [_, _, name, names, ..]) => {
                let key = mapping.fold(name);
                let channel = self.channels.entry(key.clone()).or_default();
                if !channel.names {
                    channel.removed_during_names.clear();
                    channel.seen_during_names.clear();
                    channel.modes_during_names.clear();
                    channel.names = true;
                }
                channel.name = name.clone();
                for written in names.split_whitespace() {
                    let nick = written.trim_start_matches(|c| self.support.prefix_symbols.contains(c));
                    let prefix_len = written.len() - nick.len();
                    let nick = nick.split('!').next().unwrap_or(nick);
                    let key = mapping.fold(nick);
                    if !channel
                        .removed_during_names
                        .iter()
                        .any(|n| mapping.equivalent(n, nick))
                    {
                        channel.seen_during_names.insert(nick.into());
                        let member = channel.members.entry(key).or_insert_with(|| Participant {
                            nick: display_text(nick),
                            ..Participant::default()
                        });
                        if !channel
                            .modes_during_names
                            .iter()
                            .any(|n| mapping.equivalent(n, nick))
                        {
                            member.prefixes = written[..prefix_len].into();
                        }
                    }
                }
                self.roster_event(&key, e);
            }
            ("366", [_, name, ..]) => {
                let key = mapping.fold(name);
                if let Some(channel) = self.channels.get_mut(&key) {
                    if channel.names {
                        channel.members.retain(|_, p| {
                            channel
                                .seen_during_names
                                .iter()
                                .any(|n| mapping.equivalent(n, &p.nick))
                        });
                    }
                    channel.names = false;
                    channel.removed_during_names.clear();
                    channel.seen_during_names.clear();
                    channel.modes_during_names.clear();
                }
                self.roster_event(&key, e);
            }
            ("PART", [name, ..]) | ("KICK", [name, ..]) => {
                let nick = if cmd == "KICK" {
                    args.get(1).map(String::as_str).unwrap_or_default()
                } else {
                    who
                };
                let key = mapping.fold(name);
                if mapping.equivalent(nick, &self.nick) {
                    self.channels.remove(&key);
                    e.events.push(Event::Left {
                        network: self.network.clone(),
                        channel: name.clone(),
                    });
                } else if let Some(channel) = self.channels.get_mut(&key) {
                    channel.members.remove(&mapping.fold(nick));
                    if channel.names {
                        channel.removed_during_names.insert(nick.into());
                    }
                }
                self.presence(
                    e,
                    m,
                    name.clone(),
                    format!(
                        "{cmd} {nick} {}",
                        args.last()
                            .filter(|s| *s != name && *s != nick)
                            .cloned()
                            .unwrap_or_default()
                    ),
                );
                self.roster_event(&key, e);
            }
            ("QUIT", _) => {
                let affected: Vec<_> = self
                    .channels
                    .iter_mut()
                    .filter_map(|(key, c)| {
                        c.members.remove(&actor).map(|_| {
                            if c.names {
                                c.removed_during_names.insert(who.into());
                            }
                            (key.clone(), c.name.clone())
                        })
                    })
                    .collect();
                for (key, name) in affected {
                    self.presence(
                        e,
                        m,
                        name,
                        format!(
                            "QUIT {who} {}",
                            args.first().map(String::as_str).unwrap_or_default()
                        ),
                    );
                    self.roster_event(&key, e);
                }
            }
            ("NICK", [new, ..]) => {
                for channel in self.channels.values_mut() {
                    if let Some(mut member) = channel.members.remove(&actor) {
                        member.nick = display_text(new);
                        channel.members.insert(mapping.fold(new), member);
                        if channel.names {
                            channel.removed_during_names.insert(who.into());
                            channel.seen_during_names.insert(new.clone());
                            channel.modes_during_names.insert(new.clone());
                        }
                    }
                }
                if own {
                    self.nick = new.clone();
                    e.events.push(Event::Snapshot(self.snapshot()));
                }
                e.events.push(Event::Nick {
                    network: self.network.clone(),
                    old: who.into(),
                    new: display_text(new),
                    own,
                });
                for key in self.channels.keys() {
                    self.roster_event(key, e);
                }
            }
            ("TOPIC", [channel, topic, ..]) | ("332", [_, channel, topic, ..]) => {
                e.events.push(Event::Topic {
                    network: self.network.clone(),
                    channel: channel.clone(),
                    text: wire::plain(topic),
                })
            }
            ("331", [_, channel, ..]) => e.events.push(Event::Topic {
                network: self.network.clone(),
                channel: channel.clone(),
                text: String::new(),
            }),
            ("333", [_, channel, nick, timestamp, ..]) => {
                if let Some(at) = timestamp
                    .parse::<i64>()
                    .ok()
                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                {
                    e.events.push(Event::TopicAuthor {
                        network: self.network.clone(),
                        channel: channel.clone(),
                        nick: wire::plain(nick),
                        at,
                    });
                }
            }
            ("MODE", [name, modes, rest @ ..]) if self.support.is_channel(name) => {
                let key = mapping.fold(name);
                let mut adding = true;
                let mut arguments = rest.iter();
                for mode in modes.chars() {
                    match mode {
                        '+' => {
                            adding = true;
                            continue;
                        }
                        '-' => {
                            adding = false;
                            continue;
                        }
                        _ => {}
                    }
                    let role = self
                        .support
                        .prefix_modes
                        .chars()
                        .position(|c| c == mode)
                        .and_then(|i| self.support.prefix_symbols.chars().nth(i));
                    let takes = role.is_some()
                        || self.support.chanmodes[0].contains(mode)
                        || self.support.chanmodes[1].contains(mode)
                        || (adding && self.support.chanmodes[2].contains(mode));
                    let argument = if takes { arguments.next() } else { None };
                    if let (Some(role), Some(nick)) = (role, argument)
                        && let Some(channel) = self.channels.get_mut(&key)
                    {
                        if channel.names {
                            channel.modes_during_names.insert(nick.clone());
                        }
                        let member =
                            channel
                                .members
                                .entry(mapping.fold(nick))
                                .or_insert_with(|| Participant {
                                    nick: nick.clone(),
                                    ..Participant::default()
                                });
                        if adding && !member.prefixes.contains(role) {
                            member.prefixes.push(role);
                        }
                        if !adding {
                            member.prefixes.retain(|c| c != role);
                        }
                    }
                }
                self.roster_event(&key, e);
                self.diagnostic(e, Some(name.clone()), format!("MODE {modes} {}", rest.join(" ")));
            }
            ("AWAY", _) => {
                for channel in self.channels.values_mut() {
                    if let Some(member) = channel.members.get_mut(&actor) {
                        member.away = !args.is_empty();
                    }
                }
                for key in self.channels.keys() {
                    self.roster_event(key, e);
                }
            }
            ("352", [_, channel, user, host, _, nick, flags, ..]) => {
                let key = mapping.fold(channel);
                if let Some(member) = self
                    .channels
                    .get_mut(&key)
                    .and_then(|c| c.members.get_mut(&mapping.fold(nick)))
                {
                    member.user = Some(display_text(user));
                    member.host = Some(display_text(host));
                    member.away = flags.contains('G');
                }
                self.roster_event(&key, e);
            }
            _ => return false,
        }
        true
    }
}
