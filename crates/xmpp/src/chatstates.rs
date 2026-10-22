//! Whether someone is typing (XEP-0085): a client says it is composing, has paused, or is
//! simply in the chat. Sent apart from the message, unencrypted, as Gajim, Monal and
//! Conversations do.

pub use xmpp_parsers::chatstates::ChatState;
use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::minidom::Element;

/// The state a message carries, and who says it: the sender's account in a chat, the
/// nick in a room. Carbons (one's own other clients) are not looked into.
pub fn state_of(message: &Message) -> Option<(BareJid, Option<String>, ChatState)> {
    let state = message
        .payloads
        .iter()
        .find_map(|p| ChatState::try_from(p.clone()).ok())?;
    let from = message.from.as_ref()?;
    let nick = match message.type_ {
        MessageType::Groupchat => Some(from.resource()?.to_string()),
        MessageType::Chat | MessageType::Normal => None,
        _ => return None,
    };
    Some((from.to_bare(), nick, state))
}

/// A message with nothing but a state, to a chat or a room; never stored.
pub fn notice(to: &BareJid, room: bool, state: ChatState) -> Message {
    let type_ = if room {
        MessageType::Groupchat
    } else {
        MessageType::Chat
    };
    let mut message = Message::new_with_type(type_, Some(Jid::from(to.clone())));
    message.payloads.push(Element::from(state));
    message
        .payloads
        .push(Element::builder("no-store", "urn:xmpp:hints").build());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_read_from_chats_and_rooms_and_written_back() {
        let composing = notice(&"bob@example.org".parse().unwrap(), false, ChatState::Composing);
        let mut back = composing.clone();
        back.from = Some("bob@example.org/gajim".parse().unwrap());
        assert_eq!(
            state_of(&back),
            Some(("bob@example.org".parse().unwrap(), None, ChatState::Composing))
        );

        let mut room = notice(&"team@muc.example.org".parse().unwrap(), true, ChatState::Paused);
        room.from = Some("team@muc.example.org/Алиса".parse().unwrap());
        assert_eq!(
            state_of(&room),
            Some((
                "team@muc.example.org".parse().unwrap(),
                Some("Алиса".into()),
                ChatState::Paused
            ))
        );

        let plain = Message::chat(Some("bob@example.org".parse().unwrap()));
        assert_eq!(state_of(&plain), None);
    }
}
