use omtalk_core::{AccountId, Network, NetworkId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Soju,
    Direct,
}

#[derive(Clone)]
pub struct Sasl {
    pub username: String,
    pub password: String,
}
impl std::fmt::Debug for Sasl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sasl")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct Config {
    pub id: String,
    pub name: String,
    pub mode: Mode,
    pub server: String,
    pub port: u16,
    pub tls: bool,
    /// Optional additional trust anchor, primarily for private/local servers.
    pub ca_certificate: Option<std::path::PathBuf>,
    pub username: String,
    pub nick: String,
    pub realname: String,
    pub sasl: Option<Sasl>,
    pub server_password: Option<String>,
    /// Explicit soju fallback: username/network, without discovery or implicit switches.
    pub selected_network: Option<String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("id", &self.id)
            .field("server", &self.server)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .finish_non_exhaustive()
    }
}

impl Config {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            return Err("IRC profile id must contain only ASCII letters, digits, _ or -".into());
        }
        for (key, text) in [
            ("server", &self.server),
            ("username", &self.username),
            ("nick", &self.nick),
        ] {
            if !super::wire::atom(text) {
                return Err(format!("IRC: invalid {key}"));
            }
        }
        if self.realname.chars().any(|c| c.is_control()) {
            return Err("IRC: invalid realname".into());
        }
        if let Some(password) = &self.server_password
            && password.chars().any(|c| c.is_control())
        {
            return Err("IRC: invalid server password".into());
        }
        if let Some(sasl) = &self.sasl {
            if !self.tls {
                return Err("IRC: SASL PLAIN requires TLS".into());
            }
            if sasl.username.contains('\0') || sasl.password.contains('\0') {
                return Err("IRC: invalid SASL credentials".into());
            }
        }
        if self.mode == Mode::Soju && self.sasl.is_none() {
            return Err("soju requires SASL credentials".into());
        }
        if let Some(network) = &self.selected_network
            && (!super::wire::atom(network) || self.mode != Mode::Soju)
        {
            return Err("IRC: selected_network is only valid in an explicit soju profile".into());
        }
        Ok(())
    }

    pub fn network(&self) -> NetworkId {
        NetworkId {
            account: AccountId::irc(&self.id),
            network: match self.mode {
                Mode::Direct => Network::Direct,
                Mode::Soju if self.selected_network.is_some() => Network::Selected,
                Mode::Soju => Network::Bouncer,
            },
        }
    }

    pub fn login_username(&self) -> String {
        let username = self.sasl.as_ref().map_or(self.username.as_str(), |s| &s.username);
        match &self.selected_network {
            Some(network) => format!("{username}/{network}"),
            None => username.to_owned(),
        }
    }
}
