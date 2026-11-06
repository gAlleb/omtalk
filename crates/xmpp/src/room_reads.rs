//! Individual displayed positions in a room (XEP-0333). IDs are opaque and belong
//! to the room; the caller must verify XEP-0359 support before using them.

use crate::BareJid;
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::ns;

pub const OCCUPANT_ID: &str = "urn:xmpp:occupant-id:0";

pub fn occupant_id(payloads: &[xmpp_parsers::minidom::Element]) -> Option<String> {
    let mut ids = payloads.iter().filter(|p| p.is("occupant-id", OCCUPANT_ID));
    let id = ids.next()?;
    if ids.next().is_some() {
        return None;
    }
    id.attr("id").filter(|id| !id.is_empty()).map(str::to_owned)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomRead {
    pub room: BareJid,
    pub nick: String,
    pub message: String,
    pub occupant_id: Option<String>,
    /// Only trusted for messages returned by the room's archive. Live messages
    /// use the identity from the room's presence instead.
    pub real_jid: Option<BareJid>,
    /// Time of the marker, never the time of the message it points to.
    pub at: Option<i64>,
}

pub fn parse(message: &Message) -> Option<RoomRead> {
    if message.type_ != MessageType::Groupchat {
        return None;
    }
    let from = message.from.as_ref()?;
    let nick = from.resource()?.to_string();
    let id = message
        .payloads
        .iter()
        .find(|p| p.is("displayed", ns::DISPLAYED_MARKERS))?
        .attr("id")?
        .to_owned();
    if id.is_empty() {
        return None;
    }
    let occupant_id = occupant_id(&message.payloads);
    // As with archived message envelopes, two MUC items are ambiguous: one may
    // be supplied by the sender. Never choose whichever happens to come first.
    let items: Vec<_> = message
        .payloads
        .iter()
        .filter(|p| p.is("x", ns::MUC_USER))
        .flat_map(|p| p.children())
        .filter(|p| p.is("item", ns::MUC_USER) && p.attr("jid").is_some())
        .collect();
    let real_jid = match items.as_slice() {
        [item] => item.attr("jid"),
        _ => None,
    }
    .and_then(|jid| jid.parse::<crate::Jid>().ok())
    .map(|jid| jid.to_bare());
    let at = message
        .payloads
        .iter()
        .find_map(|p| xmpp_parsers::delay::Delay::try_from(p.clone()).ok())
        .map(|delay| delay.stamp.0.timestamp());
    Some(RoomRead {
        room: from.to_bare(),
        nick,
        message: id,
        occupant_id,
        real_jid,
        at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_marker_keeps_its_reader_and_its_own_timestamp() {
        let message: Message = "<message xmlns='jabber:client' type='groupchat' from='room@conference.example/Anya'><displayed xmlns='urn:xmpp:chat-markers:0' id='room-message'/><occupant-id xmlns='urn:xmpp:occupant-id:0' id='stable-anya'/><delay xmlns='urn:xmpp:delay' stamp='2026-11-06T10:32:00Z'/><x xmlns='http://jabber.org/protocol/muc#user'><item jid='anya@example/phone'/></x></message>".parse::<xmpp_parsers::minidom::Element>().unwrap().try_into().unwrap();
        let mark = parse(&message).unwrap();
        assert_eq!(mark.room.as_str(), "room@conference.example");
        assert_eq!(mark.nick, "Anya");
        assert_eq!(mark.message, "room-message");
        assert_eq!(mark.occupant_id.as_deref(), Some("stable-anya"));
        assert_eq!(mark.real_jid.unwrap().as_str(), "anya@example");
        assert!(mark.at.is_some());
        let mut invalid = message.clone();
        invalid.type_ = MessageType::Chat;
        assert!(parse(&invalid).is_none());
        invalid.type_ = MessageType::Groupchat;
        invalid.from = Some("room@conference.example".parse().unwrap());
        assert!(parse(&invalid).is_none());
    }

    #[test]
    fn ambiguous_identities_and_empty_targets_are_not_trusted() {
        let xml = "<message xmlns='jabber:client' type='groupchat' from='room@conference.example/Anya'><displayed xmlns='urn:xmpp:chat-markers:0' id='target'/><occupant-id xmlns='urn:xmpp:occupant-id:0' id='forged'/><occupant-id xmlns='urn:xmpp:occupant-id:0' id='real'/><x xmlns='http://jabber.org/protocol/muc#user'><item jid='forged@example'/></x><x xmlns='http://jabber.org/protocol/muc#user'><item jid='anya@example'/></x></message>";
        let decode =
            |s: &str| Message::try_from(s.parse::<xmpp_parsers::minidom::Element>().unwrap()).unwrap();
        let message = decode(xml);
        let mark = parse(&message).unwrap();
        assert_eq!(mark.real_jid, None);
        assert_eq!(mark.occupant_id, None);
        assert!(
            crate::receipts::marks(&"me@example".parse().unwrap(), &message).is_empty(),
            "room markers must not enter the direct-chat receipt path"
        );
        assert!(parse(&decode(&xml.replace("id='target'", "id=''"))).is_none());
    }
}
