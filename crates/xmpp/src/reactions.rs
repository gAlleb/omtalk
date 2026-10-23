//! Reactions (XEP-0444): a message names an earlier one and carries all its sender's
//! reactions to it, each a short text (an emoji); a later one replaces them all, none
//! takes them back. In a room the message is named by the room's id for it
//! (XEP-0359), in a chat by the sender's.

use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::ns;

pub const NS: &str = "urn:xmpp:reactions:0";
const HINTS: &str = "urn:xmpp:hints";

/// How many reactions of one sender to one message are taken: more is noise.
const MOST: usize = 10;

/// Reactions come in: in `chat`, to the message `id`, by `by` (a nick in a room, the
/// account otherwise), all of theirs to it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reactions {
    pub chat: BareJid,
    pub room: bool,
    pub id: String,
    pub by: String,
    pub reactions: Vec<String>,
}

/// A message to `to` with `reactions` to the message `id`: all of one's reactions to it,
/// none to take them back. Kept by the archive (a hint), as a reaction is history.
pub fn message(to: &BareJid, room: bool, id: &str, reactions: &[String]) -> Message {
    let kind = if room {
        MessageType::Groupchat
    } else {
        MessageType::Chat
    };
    let mut message = Message::new_with_type(kind, Some(to.clone().into()));
    let mut element = Element::builder("reactions", NS).attr(xml_ncname!("id").into(), id);
    for reaction in reactions {
        element = element.append(Element::builder("reaction", NS).append(reaction.as_str()).build());
    }
    message.payloads.push(element.build());
    message.payloads.push(Element::builder("store", HINTS).build());
    message
}

fn of(payloads: &[Element]) -> Option<(String, Vec<String>)> {
    let element = payloads.iter().find(|p| p.is("reactions", NS))?;
    let id = element.attr(xml_ncname!("id")).filter(|id| !id.is_empty())?;
    let mut reactions: Vec<String> = element
        .children()
        .filter(|c| c.is("reaction", NS))
        .map(|c| c.text().trim().to_owned())
        .filter(|r| !r.is_empty() && r.chars().count() <= 8)
        .collect();
    reactions.dedup();
    reactions.truncate(MOST);
    Some((id.to_owned(), reactions))
}

/// The reactions a message carries, and whose they are. One's own from another device
/// come as a carbon copy, taken only from the own account (the server).
pub fn reactions(own: &BareJid, message: &Message) -> Option<Reactions> {
    let from = message.from.as_ref()?;
    if message.type_ == MessageType::Groupchat {
        let nick = from.resource()?.to_string();
        let (id, reactions) = of(&message.payloads)?;
        return Some(Reactions {
            chat: from.to_bare(),
            room: true,
            id,
            by: nick,
            reactions,
        });
    }
    let sender = from.to_bare();
    if let Some((id, reactions)) = of(&message.payloads) {
        return Some(Reactions {
            chat: sender.clone(),
            room: false,
            id,
            by: sender.to_string(),
            reactions,
        });
    }
    if sender != *own {
        return None;
    }
    for p in &message.payloads {
        let sent = p.is("sent", ns::CARBONS);
        if !(sent || p.is("received", ns::CARBONS)) {
            continue;
        }
        for inner in p
            .children()
            .flat_map(|f| f.children())
            .filter(|m| m.is("message", ns::DEFAULT_NS))
        {
            let party = inner.attr(if sent {
                xml_ncname!("to")
            } else {
                xml_ncname!("from")
            });
            let Some(chat) = party.and_then(|j| j.parse::<Jid>().ok()).map(|j| j.to_bare()) else {
                continue;
            };
            let payloads: Vec<Element> = inner.children().cloned().collect();
            if let Some((id, reactions)) = of(&payloads) {
                let by = if sent { own.to_string() } else { chat.to_string() };
                return Some(Reactions {
                    chat,
                    room: false,
                    id,
                    by,
                    reactions,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_are_read_as_sent_and_said_whose() {
        let me: BareJid = "me@example.org".parse().unwrap();
        let bob: BareJid = "bob@example.org".parse().unwrap();
        let mut message = message(&me, false, "m-1", &["👍".into(), "👍".into(), "❤".into()]);
        message.from = Some("bob@example.org/phone".parse().unwrap());
        assert_eq!(
            reactions(&me, &message),
            Some(Reactions {
                chat: bob.clone(),
                room: false,
                id: "m-1".into(),
                by: "bob@example.org".into(),
                reactions: vec!["👍".into(), "❤".into()],
            })
        );

        // In a room, by the nick, and the room is the chat.
        let room: BareJid = "team@muc.example.org".parse().unwrap();
        let mut in_room = super::message(&room, true, "s-9", &[]);
        in_room.from = Some("team@muc.example.org/Алиса".parse().unwrap());
        let got = reactions(&me, &in_room).unwrap();
        assert_eq!(
            (got.chat, got.room, got.by.as_str(), got.reactions.len()),
            (room, true, "Алиса", 0)
        );

        // Nothing of the kind: none.
        let mut plain = Message::chat(Some(me.clone().into()));
        plain.from = Some(bob.into());
        assert_eq!(reactions(&me, &plain), None);
    }
}
