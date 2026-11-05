use super::*;
use base64::Engine;
use omtalk_core::{ConnectionState, Network};

const CAPS: &[&str] = &[
    "message-tags",
    "server-time",
    "batch",
    "echo-message",
    "labeled-response",
    "draft/chathistory",
    "draft/read-marker",
    "draft/metadata-2",
    "multi-prefix",
    "away-notify",
    "extended-join",
    "soju.im/bouncer-networks",
    "soju.im/bouncer-networks-notify",
    "sasl",
];

impl State {
    pub fn start(&self) -> Vec<Message> {
        let mut messages = vec![wire::command("CAP", &["LS", "302"])];
        if let Some(pass) = &self.config.server_password {
            messages.push(wire::command("PASS", &[pass]));
        }
        messages.push(wire::command("NICK", &[&self.nick]));
        messages.push(wire::command(
            "USER",
            &[&self.config.username, "0", "*", &self.config.realname],
        ));
        messages
    }

    fn finish_cap(&mut self, e: &mut Effects) {
        if self.cap_ended || !self.requested.is_empty() {
            return;
        }
        if self.config.sasl.is_some() && !self.authenticated {
            if !self.caps.contains("sasl") {
                self.fatal = Some("IRC: server did not accept SASL".into());
            }
            return;
        }
        if matches!(self.network.network, Network::Bouncer | Network::Soju(_))
            && !self.caps.contains("soju.im/bouncer-networks")
        {
            self.fatal = Some("soju: discovery is unavailable; configure selected_network explicitly".into());
            return;
        }
        if let Network::Soju(id) = &self.network.network {
            e.send.push(wire::command("BOUNCER", &["BIND", id]));
        }
        e.send.push(wire::command("CAP", &["END"]));
        self.cap_ended = true;
    }

    fn ready(&mut self, e: &mut Effects) {
        if self.connected || !self.registered {
            return;
        }
        if let Network::Soju(id) = &self.network.network {
            match &self.support.bound_network {
                None => return,
                Some(bound) if bound != id => {
                    self.fatal = Some("soju: server bound an unexpected network".into());
                    return;
                }
                _ => {}
            }
        }
        self.connected = true;
        if self.snapshot().capabilities.pin_sync {
            e.send
                .push(wire::command("METADATA", &["*", "SUB", "soju.im/pinned"]));
        }
        e.events.push(Event::Connection {
            network: self.network.clone(),
            state: ConnectionState::Connected,
        });
        e.events.push(Event::Snapshot(self.snapshot()));
        if self.network.network == Network::Bouncer && !self.caps.contains("soju.im/bouncer-networks-notify")
        {
            e.send.push(wire::command("BOUNCER", &["LISTNETWORKS"]));
        }
    }

    pub(super) fn registration(
        &mut self,
        message: &Message,
        cmd: &str,
        args: &[String],
        e: &mut Effects,
    ) -> bool {
        match cmd {
            "PING" => {
                if let Some(token) = args.first() {
                    e.send.push(wire::command("PONG", &[token]));
                }
            }
            "PONG" => {}
            "CAP" => {
                let Some(pos) = args
                    .iter()
                    .position(|s| matches!(s.as_str(), "LS" | "ACK" | "NAK" | "NEW" | "DEL"))
                else {
                    return true;
                };
                let sub = args[pos].as_str();
                let list = args.last().map(String::as_str).unwrap_or("");
                match sub {
                    "LS" | "NEW" => {
                        for cap in list.split_whitespace() {
                            let (name, value) = cap.split_once('=').unwrap_or((cap, ""));
                            self.offered.insert(name.into(), value.into());
                        }
                        if sub == "LS" && args.get(pos + 1).is_some_and(|s| s == "*") {
                            return true;
                        }
                        let want: Vec<_> = CAPS
                            .iter()
                            .copied()
                            .filter(|cap| {
                                self.offered.contains_key(*cap)
                                    && !self.caps.contains(*cap)
                                    && !self.requested.contains(*cap)
                                    && (*cap != "sasl" || (self.config.sasl.is_some() && !self.registered))
                                    && (!cap.starts_with("soju.im/bouncer-networks")
                                        || matches!(
                                            self.network.network,
                                            Network::Bouncer | Network::Soju(_)
                                        ))
                                    && (*cap != "labeled-response" || self.offered.contains_key("batch"))
                            })
                            .collect();
                        if !want.is_empty() {
                            self.requested.extend(want.iter().map(|s| (*s).to_owned()));
                            e.send.push(wire::command("CAP", &["REQ", &want.join(" ")]));
                        } else if sub == "LS" {
                            self.finish_cap(e);
                        }
                    }
                    "ACK" | "NAK" => {
                        let pin_sync = self.snapshot().capabilities.pin_sync;
                        for cap in list.split_whitespace() {
                            let negative = cap.starts_with('-');
                            let name = cap.trim_start_matches('-').split('=').next().unwrap_or(cap);
                            self.requested.remove(name);
                            if sub == "ACK" && !negative {
                                self.caps.insert(name.into());
                            } else {
                                self.caps.remove(name);
                            }
                        }
                        if self.caps.contains("sasl") && self.config.sasl.is_some() && !self.auth_started {
                            let value = self.offered.get("sasl").map(String::as_str).unwrap_or("");
                            if !value.is_empty() && !value.split(',').any(|s| s == "PLAIN") {
                                self.fatal = Some("IRC: SASL PLAIN is unavailable".into());
                                return true;
                            }
                            self.auth_started = true;
                            e.send.push(wire::command("AUTHENTICATE", &["PLAIN"]));
                        }
                        self.finish_cap(e);
                        if self.connected {
                            if !pin_sync && self.snapshot().capabilities.pin_sync {
                                e.send
                                    .push(wire::command("METADATA", &["*", "SUB", "soju.im/pinned"]));
                            }
                            e.events.push(Event::Snapshot(self.snapshot()));
                        }
                    }
                    "DEL" => {
                        for cap in list.split_whitespace() {
                            self.offered.remove(cap);
                            self.caps.remove(cap);
                        }
                        e.events.push(Event::Snapshot(self.snapshot()));
                        if !self.snapshot().capabilities.history {
                            self.fail_history("IRC: history capability was removed", e);
                        }
                    }
                    _ => {}
                }
            }
            "AUTHENTICATE" if self.auth_started && !self.authenticated => {
                if args.first().map(String::as_str) != Some("+") {
                    self.fatal = Some("IRC: unsupported SASL challenge".into());
                    return true;
                }
                if let Some(sasl) = &self.config.sasl {
                    let payload = base64::engine::general_purpose::STANDARD.encode(format!(
                        "\0{}\0{}",
                        self.config.login_username(),
                        sasl.password
                    ));
                    for chunk in payload.as_bytes().chunks(400) {
                        e.send.push(wire::command(
                            "AUTHENTICATE",
                            &[std::str::from_utf8(chunk).unwrap()],
                        ));
                    }
                    if payload.len().is_multiple_of(400) {
                        e.send.push(wire::command("AUTHENTICATE", &["+"]));
                    }
                }
            }
            "903" => {
                self.authenticated = true;
                self.finish_cap(e);
            }
            "904" | "905" | "906" | "907" | "908" | "464" => {
                self.fatal = Some("IRC: authentication refused".into())
            }
            "432" | "433" | "436" if !self.registered => {
                self.fatal = Some("IRC: nickname unavailable; choose another nick in the profile".into())
            }
            "001" => {
                self.server_name = message.prefix.as_ref().map(ToString::to_string);
                if let Some(nick) = args.first() {
                    self.nick = nick.clone();
                }
                if self.config.sasl.is_some() && !self.authenticated {
                    self.fatal = Some("IRC: registration completed without required authentication".into());
                } else {
                    self.registered = true;
                    self.ready(e);
                }
            }
            "005" => {
                let previous = self.support.case_mapping;
                self.support.apply(args.get(1..).unwrap_or_default());
                if let Network::Soju(expected) = &self.network.network
                    && self
                        .support
                        .bound_network
                        .as_ref()
                        .is_some_and(|actual| actual != expected)
                {
                    self.fatal = Some("soju: server bound an unexpected network".into());
                    return true;
                }
                if previous != self.support.case_mapping {
                    self.rekey_roster(e);
                }
                self.ready(e);
                e.events.push(Event::Snapshot(self.snapshot()));
            }
            "421" if !self.registered && args.iter().any(|s| s == "CAP") => {
                if self.config.sasl.is_some() {
                    self.fatal = Some("IRC: server does not support required SASL".into());
                } else {
                    self.cap_ended = true;
                }
            }
            _ => return false,
        }
        true
    }
}
