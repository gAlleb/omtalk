use xmpp_parsers::jid::{BareJid, FullJid};
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::Presence;

#[derive(Debug)]
pub struct AccountEvent {
    pub account: BareJid,
    pub event: Event,
}

#[derive(Debug)]
pub enum Event {
    /// Connected and bound. After `resumed: false` the session is new: the server forgot
    /// presence and other per-session state, and the client re-announces it.
    Online {
        jid: FullJid,
        resumed: bool,
    },
    /// The server answers again after [`Event::Disconnected`] said it did not, on the
    /// same connection: online as before, nothing to redo.
    Reachable,
    /// The server refused the password (or the account). The account has stopped: fix
    /// the credentials and add it again.
    AuthFailed {
        reason: String,
    },
    /// The connection broke or could not be made; it is being re-established in the
    /// background.
    Disconnected {
        reason: String,
    },
    Message(Box<Message>),
    /// The contact list changed on the server (from this or another client).
    Roster(Vec<crate::roster::Contact>),
    /// Someone asks to see our status: answer with `accept_contact` or `refuse_contact`.
    SubscriptionRequest {
        from: BareJid,
    },
    Presence(Box<Presence>),
    /// A PEP node of `from` changed (published or retracted items).
    Pep {
        from: BareJid,
        node: String,
        items: Vec<PepItem>,
        retracted: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PepItem {
    pub id: Option<String>,
    pub payload: Element,
}
