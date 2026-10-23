//! What a message carries and between whom: unwraps carbon copies (XEP-0280), names
//! group senders, and tells OMEMO from plain text.

use omemo_xml::xml;
use omtalk_xmpp::BareJid;
use xmpp_parsers::carbons::{Received, Sent};
use xmpp_parsers::message::{Message, MessageType};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the peer to us.
    Incoming,
    /// Sent by another device of the own account; we see it as a carbon copy.
    Outgoing,
}

pub(crate) enum Content {
    Omemo(xml::Encrypted),
    /// Sent without encryption. Shown, marked as such, whatever the chat's setting.
    Plain(String),
}

pub(crate) struct Envelope {
    /// The other side of the conversation: the peer, or the room.
    pub peer: BareJid,
    /// The account that sent it. In a group chat whose sender the room does not reveal
    /// (plain text in an anonymous room), the room itself.
    pub sender: BareJid,
    /// The sender's nick, in a group chat.
    pub nick: Option<String>,
    pub direction: Direction,
    pub content: Content,
    pub meta: Meta,
}

/// What a message says about itself, besides its content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// The sender's id for it: origin-id (XEP-0359), which rooms keep, else the stanza's.
    pub id: Option<String>,
    /// The sender would like to know when it is read (XEP-0333).
    pub markable: bool,
    /// The id of the message this one answers (XEP-0461).
    pub reply_to: Option<String>,
    /// The body is the link out of band data names (XEP-0066): a file shared, as
    /// Conversations, Gajim and Monal mark their uploads.
    pub shared_file: bool,
}

pub(crate) fn open(own: &BareJid, mut message: Message) -> Option<Envelope> {
    let outer_from = message.from.as_ref()?.to_bare();

    // A carbon is only genuine if the own account's server sent it; anybody else could
    // wrap a forged message in one.
    if outer_from == *own {
        if let Ok(Some(sent)) = message.extract_payload::<Sent>() {
            let inner = sent.forwarded.message;
            let peer = inner.to.as_ref()?.to_bare();
            return content_of(inner).map(|(content, meta)| Envelope {
                peer,
                sender: own.clone(),
                nick: None,
                direction: Direction::Outgoing,
                content,
                meta,
            });
        }
        if let Ok(Some(received)) = message.extract_payload::<Received>() {
            message = received.forwarded.message;
        }
    }

    let sender = message.from.as_ref()?.to_bare();
    content_of(message).map(|(content, meta)| Envelope {
        peer: sender.clone(),
        sender,
        nick: None,
        direction: Direction::Incoming,
        content,
        meta,
    })
}

/// Opens a group message, whose sender is known by nick only: `resolve` maps it to the
/// real account. An OMEMO message whose sender cannot be resolved is an error, not a
/// silent drop: its keys belong to an account we do not know.
pub(crate) fn open_group(
    own: &BareJid,
    message: Message,
    resolve: impl Fn(&BareJid, &str) -> Option<BareJid>,
) -> Result<Option<Envelope>, crate::Error> {
    let Some(from) = message.from.as_ref().and_then(|f| f.try_as_full().ok()).cloned() else {
        return Ok(None);
    };
    let room = from.to_bare();
    let nick = from.resource().to_string();
    let real = resolve(&room, &nick);
    room_envelope(own, &room, nick, real, message)
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
    content_of(message).map(|(content, meta)| Envelope {
        peer,
        sender,
        nick: None,
        direction,
        content,
        meta,
    })
}

/// Opens a message from a room's archive. The archive names the sender's real account
/// in it, so a sender who has left since is still known.
pub(crate) fn open_room_archived(
    own: &BareJid,
    room: &BareJid,
    message: Message,
) -> Result<Option<Envelope>, crate::Error> {
    let nick = message
        .from
        .as_ref()
        .and_then(|from| from.resource().map(|r| r.to_string()))
        .unwrap_or_default();
    // Read by hand: xmpp-parsers wants a full JID here, as presences carry, but the
    // archive stores the bare one, and the whole element would fail to parse.
    let real = message
        .payloads
        .iter()
        .filter(|p| p.is("x", xmpp_parsers::ns::MUC_USER))
        .flat_map(|x| x.children())
        .find(|item| item.is("item", xmpp_parsers::ns::MUC_USER))
        .and_then(|item| item.attr(xmpp_parsers::minidom::rxml::xml_ncname!("jid")))
        .and_then(|jid| jid.parse::<omtalk_xmpp::Jid>().ok())
        .map(|jid| jid.to_bare());
    room_envelope(own, room, nick, real, message)
}

fn room_envelope(
    own: &BareJid,
    room: &BareJid,
    nick: String,
    real: Option<BareJid>,
    message: Message,
) -> Result<Option<Envelope>, crate::Error> {
    let Some((content, meta)) = content_of(message) else {
        return Ok(None);
    };
    let sender = match (real, &content) {
        (Some(real), _) => real,
        (None, Content::Plain(_)) => room.clone(),
        (None, Content::Omemo(_)) => {
            return Err(crate::Error::UnknownOccupant {
                room: room.clone(),
                nick,
            });
        }
    };
    let direction = if &sender == own {
        Direction::Outgoing
    } else {
        Direction::Incoming
    };
    Ok(Some(Envelope {
        peer: room.clone(),
        sender,
        nick: Some(nick),
        direction,
        content,
        meta,
    }))
}

/// OMEMO if the message has it (its body is then only the fallback text), else the
/// plain body. Errors and messages without either carry nothing to show.
fn content_of(mut message: Message) -> Option<(Content, Meta)> {
    if message.type_ == MessageType::Error {
        return None;
    }
    let origin = message.payloads.iter().find_map(|p| {
        p.is("origin-id", xmpp_parsers::ns::SID)
            .then(|| {
                p.attr(xmpp_parsers::minidom::rxml::xml_ncname!("id"))
                    .map(str::to_owned)
            })
            .flatten()
    });
    let meta = Meta {
        id: origin.or_else(|| message.id.as_ref().map(|id| id.0.clone())),
        markable: message
            .payloads
            .iter()
            .any(|p| p.is("markable", xmpp_parsers::ns::DISPLAYED_MARKERS)),
        reply_to: omtalk_xmpp::replies::reply_of(&message).map(|r| r.id),
        shared_file: false,
    };
    let body = message.get_best_body_cloned(vec![]).map(|(_, b)| b);
    let mut meta = meta;
    meta.shared_file = body.as_deref().is_some_and(|body| {
        message.payloads.iter().any(|p| {
            p.is("x", "jabber:x:oob")
                && p.get_child("url", "jabber:x:oob")
                    .is_some_and(|url| url.text().trim() == body.trim())
        })
    });
    if let Ok(Some(encrypted)) = message.extract_payload::<xml::Encrypted>() {
        return Some((Content::Omemo(encrypted), meta));
    }
    message
        .get_best_body_cloned(vec![])
        .map(|(_, body)| (Content::Plain(body), meta))
}

/// The id `archive` gave this live message (XEP-0359), if it gave one. `archive` is the
/// own account for chats, the room for group chats.
pub(crate) fn archive_id(archive: &BareJid, message: &Message) -> Option<String> {
    let by_own = |payloads: &[xmpp_parsers::minidom::Element]| {
        payloads
            .iter()
            .filter_map(|p| xmpp_parsers::stanza_id::StanzaId::try_from(p.clone()).ok())
            .find(|id| id.by.resource().is_none() && id.by.to_bare() == *archive)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_a_shared_file_only_where_out_of_band_data_names_it() {
        let message = |payload: &str| -> Message {
            format!(
                r#"<message xmlns="jabber:client" type="chat" from="bob@example.org/pc" to="me@example.org"><body>https://up.example.org/a/notes.txt</body>{payload}</message>"#
            )
            .parse::<xmpp_parsers::minidom::Element>()
            .unwrap()
            .try_into()
            .unwrap()
        };
        let shared = |m: Message| open(&own(), m).unwrap().meta.shared_file;
        assert!(shared(message(
            r#"<x xmlns="jabber:x:oob"><url>https://up.example.org/a/notes.txt</url></x>"#
        )));
        assert!(!shared(message("")), "a link written by hand");
        assert!(
            !shared(message(
                r#"<x xmlns="jabber:x:oob"><url>https://elsewhere.example/x.txt</url></x>"#
            )),
            "out of band data about another link"
        );
    }
    use omemo::{OwnKeys, Session};
    use omtalk_xmpp::Element;
    use xmpp_parsers::message::Lang;

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

    fn own() -> BareJid {
        "me@example.org".parse().unwrap()
    }

    #[test]
    fn genuine_sent_carbon_is_our_outgoing_message() {
        let envelope = open(&own(), sent_carbon("me@example.org")).unwrap();
        assert_eq!(envelope.direction, Direction::Outgoing);
        assert_eq!(envelope.peer.as_str(), "peer@example.org");
        assert!(matches!(envelope.content, Content::Omemo(_)));
    }

    #[test]
    fn carbon_from_someone_else_is_ignored() {
        // Mallory wraps a message in a carbon to make it look like one we sent.
        assert!(open(&own(), sent_carbon("mallory@evil.example")).is_none());
    }

    #[test]
    fn plain_text_is_kept_and_omemo_fallback_text_is_not() {
        let mut plain = Message::chat(Some(own().into())).with_body(Lang::default(), "hi".into());
        plain.from = Some("peer@example.org/pc".parse().unwrap());
        assert!(matches!(open(&own(), plain).unwrap().content, Content::Plain(ref t) if t == "hi"));

        // An OMEMO message's body is the "cannot decrypt" fallback: never shown as text.
        let mut omemo = Message::chat(Some(own().into()))
            .with_body(Lang::default(), "This message is OMEMO encrypted".into())
            .with_payload(encrypted());
        omemo.from = Some("peer@example.org/pc".parse().unwrap());
        assert!(matches!(open(&own(), omemo).unwrap().content, Content::Omemo(_)));

        let mut error = Message::error(Some(own().into())).with_body(Lang::default(), "bounce".into());
        error.from = Some("peer@example.org/pc".parse().unwrap());
        assert!(open(&own(), error).is_none(), "an error is not a message to show");
    }

    #[test]
    fn group_plain_text_from_unknown_nick_still_shows_but_omemo_does_not() {
        let room: BareJid = "room@muc.example.org".parse().unwrap();
        let from = |m: Message| {
            let mut m = m;
            m.from = Some("room@muc.example.org/stranger".parse().unwrap());
            m
        };
        let unknown = |_: &BareJid, _: &str| None;
        let plain =
            from(Message::groupchat(Some(room.clone().into())).with_body(Lang::default(), "hi".into()));
        let envelope = open_group(&own(), plain, unknown).unwrap().unwrap();
        assert_eq!(
            (&envelope.sender, envelope.nick.as_deref()),
            (&room, Some("stranger"))
        );

        let omemo = from(Message::groupchat(Some(room.clone().into())).with_payload(encrypted()));
        assert!(matches!(
            open_group(&own(), omemo, unknown),
            Err(crate::Error::UnknownOccupant { .. })
        ));
    }
}
