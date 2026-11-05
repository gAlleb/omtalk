//! UI row keys. Explicit protocol variants keep existing XMPP addresses intact;
//! network/conversation identities are never encoded as fabricated JIDs.
use omtalk_core::{ConversationId, NetworkId};
use omtalk_xmpp::BareJid;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Address {
    Xmpp(BareJid),
    Network(NetworkId),
    Conversation(ConversationId),
}

impl From<&Address> for Address {
    fn from(address: &Address) -> Self {
        address.clone()
    }
}
impl From<BareJid> for Address {
    fn from(jid: BareJid) -> Self {
        Self::Xmpp(jid)
    }
}
impl From<&BareJid> for Address {
    fn from(jid: &BareJid) -> Self {
        Self::Xmpp(jid.clone())
    }
}
impl From<NetworkId> for Address {
    fn from(id: NetworkId) -> Self {
        Self::Network(id)
    }
}
impl From<ConversationId> for Address {
    fn from(id: ConversationId) -> Self {
        Self::Conversation(id)
    }
}
impl PartialEq<BareJid> for Address {
    fn eq(&self, other: &BareJid) -> bool {
        self.xmpp() == Some(other)
    }
}
impl PartialEq<Address> for BareJid {
    fn eq(&self, other: &Address) -> bool {
        other == self
    }
}

impl std::str::FromStr for Address {
    type Err = <BareJid as std::str::FromStr>::Err;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse::<BareJid>().map(Self::Xmpp)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Xmpp(jid) => jid.fmt(f),
            Self::Network(network) => match &network.network {
                omtalk_core::Network::Soju(id) => write!(f, "IRC {} / {id}", network.account.id),
                _ => write!(f, "IRC {}", network.account.id),
            },
            Self::Conversation(ConversationId::Irc { target, .. }) => target.fmt(f),
            Self::Conversation(ConversationId::Xmpp { jid, .. }) => jid.fmt(f),
        }
    }
}

impl Address {
    pub fn kept(&self) -> String {
        match self {
            Self::Xmpp(jid) => jid.to_string(),
            Self::Network(id) => format!("irc-network:{}", serde_json::to_string(id).unwrap()),
            Self::Conversation(id) => format!("conversation:{}", serde_json::to_string(id).unwrap()),
        }
    }
    pub fn from_kept(text: &str) -> Option<Self> {
        if let Some(id) = text.strip_prefix("irc-network:") {
            return serde_json::from_str(id).ok().map(Self::Network);
        }
        if let Some(id) = text.strip_prefix("conversation:") {
            return serde_json::from_str(id).ok().map(Self::Conversation);
        }
        text.parse().ok()
    }

    pub fn xmpp(&self) -> Option<&BareJid> {
        if let Self::Xmpp(jid) = self {
            Some(jid)
        } else {
            None
        }
    }
    pub fn is_irc(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::Conversation(ConversationId::Irc { .. })
        )
    }
    pub fn network(&self) -> Option<&NetworkId> {
        match self {
            Self::Network(id) => Some(id),
            Self::Conversation(ConversationId::Irc { network, .. }) => Some(network),
            _ => None,
        }
    }
    pub fn conversation(&self) -> Option<&ConversationId> {
        if let Self::Conversation(id) = self {
            Some(id)
        } else {
            None
        }
    }
    pub fn node(&self) -> Option<&str> {
        self.xmpp().and_then(|j| j.node()).map(|n| n.as_str())
    }
    pub fn domain(&self) -> Option<&str> {
        self.xmpp().map(|j| j.domain().as_str())
    }
    pub fn as_str(&self) -> &str {
        match self {
            Self::Xmpp(jid) => jid.as_str(),
            Self::Network(id) => &id.account.id,
            Self::Conversation(ConversationId::Irc { target, .. }) => target,
            Self::Conversation(ConversationId::Xmpp { jid, .. }) => jid,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub account: Address,
    pub name: String,
    pub chats: Vec<Address>,
}
