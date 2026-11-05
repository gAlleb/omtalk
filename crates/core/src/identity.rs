use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Xmpp,
    Irc,
}

/// A configured identity, never a mutable nickname, hostname or display name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AccountId {
    pub protocol: Protocol,
    pub id: String,
}

impl AccountId {
    pub fn irc(id: impl Into<String>) -> Self {
        Self {
            protocol: Protocol::Irc,
            id: id.into(),
        }
    }

    pub fn xmpp(bare_jid: impl Into<String>) -> Self {
        Self {
            protocol: Protocol::Xmpp,
            id: bare_jid.into(),
        }
    }
}

/// A soju netid is only unique inside its configured bouncer account.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetworkId {
    pub account: AccountId,
    pub network: Network,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Network {
    Direct,
    Bouncer,
    Soju(String),
    /// Explicit fallback for a bouncer without network discovery. The profile ID
    /// remains the storage identity when the configured username/hostname changes.
    Selected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConversationKind {
    Direct,
    Channel,
    Room,
    Server,
    Control,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ConversationId {
    Xmpp {
        account: AccountId,
        jid: String,
    },
    Irc {
        network: NetworkId,
        kind: ConversationKind,
        target: String,
    },
}

impl ConversationId {
    pub fn network(&self) -> Option<&NetworkId> {
        match self {
            Self::Irc { network, .. } => Some(network),
            Self::Xmpp { .. } => None,
        }
    }
    pub fn account(&self) -> &AccountId {
        match self {
            Self::Xmpp { account, .. } => account,
            Self::Irc { network, .. } => &network.account,
        }
    }

    pub fn irc(network: NetworkId, kind: ConversationKind, target: &str, mapping: CaseMapping) -> Self {
        Self::Irc {
            network,
            kind,
            target: mapping.fold(target),
        }
    }
}

/// IRC comparison deliberately folds ASCII only. Unicode case conversion would
/// merge distinct nicknames on servers which do not consider them equal.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseMapping {
    Ascii,
    StrictRfc1459,
    #[default]
    Rfc1459,
}

impl CaseMapping {
    pub fn fold(self, text: &str) -> String {
        text.chars()
            .map(|c| match c {
                'A'..='Z' => c.to_ascii_lowercase(),
                '[' if self != Self::Ascii => '{',
                ']' if self != Self::Ascii => '}',
                '\\' if self != Self::Ascii => '|',
                '^' if self == Self::Rfc1459 => '~',
                _ => c,
            })
            .collect()
    }

    pub fn equivalent(self, left: &str, right: &str) -> bool {
        self.fold(left) == self.fold(right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn irc_names_follow_network_mapping_not_unicode_lowercase() {
        assert!(CaseMapping::Rfc1459.equivalent("Nick[\\^", "nick{|~"));
        assert!(!CaseMapping::StrictRfc1459.equivalent("^", "~"));
        assert!(!CaseMapping::Ascii.equivalent("[", "{"));
        assert!(!CaseMapping::Rfc1459.equivalent("Ä", "ä"));
    }

    #[test]
    fn same_target_and_netid_on_two_bouncers_never_collide() {
        let target = |account| {
            ConversationId::irc(
                NetworkId {
                    account: AccountId::irc(account),
                    network: Network::Soju("1".into()),
                },
                ConversationKind::Channel,
                "#om",
                CaseMapping::Rfc1459,
            )
        };
        assert_ne!(target("home"), target("work"));
    }
}
