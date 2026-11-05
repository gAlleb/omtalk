use omtalk_core::{Capabilities, CaseMapping};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Support {
    pub case_mapping: CaseMapping,
    pub chantypes: String,
    pub statusmsg: String,
    pub prefix_modes: String,
    pub prefix_symbols: String,
    pub chanmodes: [String; 4],
    pub history_limit: usize,
    pub reference_types: Vec<String>,
    pub line_limit: usize,
    pub filehost: Option<String>,
    pub bound_network: Option<String>,
    pub network_name: Option<String>,
    pub tokens: BTreeMap<String, String>,
}

impl Default for Support {
    fn default() -> Self {
        Self {
            case_mapping: CaseMapping::Rfc1459,
            chantypes: "#&".into(),
            statusmsg: String::new(),
            prefix_modes: "ov".into(),
            prefix_symbols: "@+".into(),
            chanmodes: ["b".into(), "k".into(), "l".into(), "imnpst".into()],
            history_limit: 100,
            reference_types: vec!["timestamp".into()],
            line_limit: 510,
            filehost: None,
            bound_network: None,
            network_name: None,
            tokens: BTreeMap::new(),
        }
    }
}

impl Support {
    pub fn message_target<'a>(&self, value: &'a str) -> &'a str {
        let channel = value.trim_start_matches(|c| self.statusmsg.contains(c));
        if self.is_channel(channel) { channel } else { value }
    }
    pub fn is_channel(&self, value: &str) -> bool {
        value.chars().next().is_some_and(|c| self.chantypes.contains(c))
    }

    pub fn apply(&mut self, tokens: &[String]) {
        for token in tokens {
            if token.contains(' ') {
                continue;
            }
            if let Some(name) = token.strip_prefix('-') {
                self.tokens.remove(name);
                let default = Self::default();
                match name {
                    "soju.im/FILEHOST" => self.filehost = None,
                    "BOUNCER_NETID" => self.bound_network = None,
                    "NETWORK" => self.network_name = None,
                    "STATUSMSG" => self.statusmsg.clear(),
                    "CASEMAPPING" => self.case_mapping = default.case_mapping,
                    "CHANTYPES" => self.chantypes = default.chantypes,
                    "PREFIX" => {
                        self.prefix_modes = default.prefix_modes;
                        self.prefix_symbols = default.prefix_symbols;
                    }
                    "CHANMODES" => self.chanmodes = default.chanmodes,
                    "CHATHISTORY" => self.history_limit = default.history_limit,
                    "MSGREFTYPES" => self.reference_types = default.reference_types,
                    "LINELEN" => self.line_limit = default.line_limit,
                    _ => {}
                }
                continue;
            }
            let (key, value) = token.split_once('=').unwrap_or((token, ""));
            self.tokens.insert(key.into(), value.into());
            match key {
                "CASEMAPPING" => {
                    self.case_mapping = match value {
                        "ascii" => CaseMapping::Ascii,
                        "strict-rfc1459" => CaseMapping::StrictRfc1459,
                        _ => CaseMapping::Rfc1459,
                    }
                }
                "CHANTYPES" => self.chantypes = value.into(),
                "STATUSMSG" => self.statusmsg = value.into(),
                "PREFIX" => {
                    if let Some((modes, symbols)) = value.strip_prefix('(').and_then(|s| s.split_once(')'))
                        && modes.chars().count() == symbols.chars().count()
                    {
                        self.prefix_modes = modes.into();
                        self.prefix_symbols = symbols.into();
                    }
                }
                "CHANMODES" => {
                    let parts: Vec<_> = value.split(',').collect();
                    if parts.len() == 4 {
                        self.chanmodes = std::array::from_fn(|i| parts[i].into());
                    }
                }
                "CHATHISTORY" => {
                    if let Ok(limit) = value.parse::<usize>() {
                        self.history_limit = if limit == 0 { 100 } else { limit.min(1000) };
                    }
                }
                "MSGREFTYPES" => self.reference_types = value.split(',').map(str::to_owned).collect(),
                "LINELEN" => {
                    if let Ok(limit) = value.parse::<usize>() {
                        self.line_limit = limit.saturating_sub(2).min(510);
                    }
                }
                "soju.im/FILEHOST" => self.filehost = Some(value.into()),
                "BOUNCER_NETID" => self.bound_network = Some(value.into()),
                "NETWORK" => self.network_name = Some(value.into()),
                _ => {}
            }
        }
    }

    pub fn capabilities(&self, caps: &std::collections::BTreeSet<String>, tls: bool) -> Capabilities {
        Capabilities {
            // soju keeps batch/time tags even when an upstream lacks message-tags.
            // Such networks can page history by timestamp without message IDs.
            history: ["draft/chathistory", "batch", "server-time"]
                .iter()
                .all(|s| caps.contains(*s)),
            read_sync: caps.contains("draft/read-marker") && caps.contains("server-time"),
            echo: caps.contains("echo-message"),
            upload: self
                .filehost
                .as_ref()
                .is_some_and(|url| url.starts_with("https://") || (!tls && url.starts_with("http://"))),
            ..Capabilities::default()
        }
    }

    pub fn rank(&self, prefixes: &str) -> usize {
        self.prefix_symbols
            .chars()
            .position(|c| prefixes.contains(c))
            .unwrap_or(self.prefix_symbols.len())
    }
}
