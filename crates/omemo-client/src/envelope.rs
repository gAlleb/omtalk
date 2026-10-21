//! Finds the OMEMO element in a message, unwrapping carbon copies (XEP-0280).

use omemo_xml::xml;
use omtalk_xmpp::BareJid;
use xmpp_parsers::carbons::{Received, Sent};
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the peer to us.
    Incoming,
    /// Sent by another device of the own account; we see it as a carbon copy.
    Outgoing,
}

pub(crate) struct Envelope {
    /// The other side of the conversation.
    pub peer: BareJid,
    /// The account whose device encrypted the message.
    pub sender: BareJid,
    pub direction: Direction,
    pub encrypted: xml::Encrypted,
}

pub(crate) fn open(own: &BareJid, mut message: Message) -> Option<Envelope> {
    let outer_from = message.from.as_ref()?.to_bare();

    // A carbon is only genuine if the own account's server sent it; anybody else could
    // wrap a forged message in one.
    if outer_from == *own {
        if let Ok(Some(sent)) = message.extract_payload::<Sent>() {
            let inner = sent.forwarded.message;
            let peer = inner.to.as_ref()?.to_bare();
            return encrypted_in(inner).map(|encrypted| Envelope {
                peer,
                sender: own.clone(),
                direction: Direction::Outgoing,
                encrypted,
            });
        }
        if let Ok(Some(received)) = message.extract_payload::<Received>() {
            message = received.forwarded.message;
        }
    }

    let sender = message.from.as_ref()?.to_bare();
    encrypted_in(message).map(|encrypted| Envelope {
        peer: sender.clone(),
        sender,
        direction: Direction::Incoming,
        encrypted,
    })
}

fn encrypted_in(mut message: Message) -> Option<xml::Encrypted> {
    message.extract_payload::<xml::Encrypted>().ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omemo::{OwnKeys, Session};
    use omtalk_xmpp::Element;

    fn encrypted() -> xml::Encrypted {
        let alice = OwnKeys::generate(0).unwrap();
        let bob = OwnKeys::generate(0).unwrap();
        let mut session = Session::initiate(&alice, &bob.bundle()).unwrap();
        omemo_xml::message::encrypt(alice.device_id, b"x", [(bob.device_id, &mut session)]).unwrap()
    }

    fn sent_carbon(outer_from: &str) -> Message {
        let mut inner = Message::chat(Some("peer@example.org".parse().unwrap())).with_payload(encrypted());
        inner.from = Some("me@example.org/phone".parse().unwrap());
        let carbon: Element = Sent {
            forwarded: xmpp_parsers::forwarding::Forwarded {
                delay: None,
                message: inner,
            },
        }
        .into();
        let mut outer = Message::normal(Some("me@example.org/laptop".parse().unwrap()));
        outer.from = Some(outer_from.parse().unwrap());
        outer.payloads.push(carbon);
        outer
    }

    #[test]
    fn genuine_sent_carbon_is_our_outgoing_message() {
        let own: BareJid = "me@example.org".parse().unwrap();
        let envelope = open(&own, sent_carbon("me@example.org")).unwrap();
        assert_eq!(envelope.direction, Direction::Outgoing);
        assert_eq!(envelope.peer.as_str(), "peer@example.org");
    }

    #[test]
    fn carbon_from_someone_else_is_ignored() {
        // Mallory wraps a message in a carbon to make it look like one we sent.
        let own: BareJid = "me@example.org".parse().unwrap();
        assert!(open(&own, sent_carbon("mallory@evil.example")).is_none());
    }
}
