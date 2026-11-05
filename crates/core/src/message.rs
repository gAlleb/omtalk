use crate::ConversationId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MessageId(pub String);

impl MessageId {
    pub fn new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageKind {
    Text,
    Action,
    Notice,
}

/// Intent is decided before removing input escapes. Literal `/me` remains Text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub conversation: ConversationId,
    pub id: MessageId,
    pub kind: MessageKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub url: String,
    pub name: String,
    pub content_type: Option<String>,
    pub size: Option<u64>,
    pub local_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub history: bool,
    pub read_sync: bool,
    pub pin_sync: bool,
    pub echo: bool,
    pub peer_receipts: bool,
    pub e2ee: bool,
    pub upload: bool,
    pub edit: bool,
    pub reactions: bool,
    pub replies: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub conversation: ConversationId,
    pub id: MessageId,
    /// Opaque, case-sensitive and scoped to the network, never ordered or folded.
    pub server_id: Option<String>,
    /// Exact original server-time for subsequent protocol cursors/read markers.
    pub server_time: Option<String>,
    pub at: chrono::DateTime<chrono::Utc>,
    pub sender: String,
    pub outgoing: bool,
    pub kind: MessageKind,
    pub text: String,
    pub attachment: Option<Attachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource {
    Live,
    CatchUp,
    Older,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Reconnecting(String),
    Failed(String),
    Removed,
}

/// Terminal escape sequences and C0/C1 controls are data, never terminal commands.
pub fn display_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}
