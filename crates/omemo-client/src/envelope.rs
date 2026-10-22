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

/// Opens a group message, whose sender is known by nick only: `resolve` maps it to the
/// real account. A sender that cannot be resolved is an error, not a silent drop.
pub(crate) fn open_group(
    own: &BareJid,
    message: Message,
    resolve: impl Fn(&BareJid, &str) -> Option<BareJid>,
) -> Result<Option<Envelope>, crate::Error> {
    let Some(from) = message.from.as_ref().and_then(|f| f.try_as_full().ok()).cloned() else {
        return Ok(None);
    };
    let Some(encrypted) = encrypted_in(message) else {
        return Ok(None);
    };
    let room = from.to_bare();
    let nick = from.resource().to_string();
    let sender = resolve(&room, &nick).ok_or(crate::Error::UnknownOccupant {
        room: room.clone(),
        nick,
    })?;
    let direction = if &sender == own {
        Direction::Outgoing
    } else {
        Direction::Incoming
    };
    Ok(Some(Envelope {
        peer: room,
        sender,
        direction,
        encrypted,
    }))
}

/// Opens a message from the own archive. It is stored as sent, not as a carbon: from
/// the sender to the recipient, the own account being one of the two.
pub(crate) fn open_archived(own: &BareJid, message: Message) -> Option<Envelope> {
    let sender = message.from.as_ref()?.to_bare();
    let (peer, direction) = if &sender == own {
        (message.to.as_ref()?.to_bare(), Direction::Outgoing)
    } else {
        (sender.clone(), Direction::Incoming)
    };
    encrypted_in(message).map(|encrypted| Envelope {
        peer,
        sender,
        direction,
        encrypted,
    })
}

/// The id the own archive gave this live message (XEP-0359), if it gave one.
pub(crate) fn archive_id(own: &BareJid, message: &Message) -> Option<String> {
    let by_own = |payloads: &[xmpp_parsers::minidom::Element]| {
        payloads
            .iter()
            .filter_map(|p| xmpp_parsers::stanza_id::StanzaId::try_from(p.clone()).ok())
            .find(|id| id.by.resource().is_none() && id.by.to_bare() == *own)
            .map(|id| id.id)
    };
    by_own(&message.payloads).or_else(|| {
        // A carbon carries the id on the message inside.
        message.payloads.iter().find_map(|p| {
            let inner = Received::try_from(p.clone())
                .map(|r| r.forwarded.message)
                .or_else(|_| Sent::try_from(p.clone()).map(|s| s.forwarded.message))
                .ok()?;
            by_own(&inner.payloads)
        })
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
