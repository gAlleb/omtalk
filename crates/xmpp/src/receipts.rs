//! Whether a message arrived and was read: delivery receipts (XEP-0184) and read markers
//! (XEP-0333), which Gajim, Conversations, Monal and Dino all speak. Rooms have neither;
//! there a message is known to have arrived when the room sends it back.

use xmpp_parsers::jid::Jid;
use xmpp_parsers::message::{Id, Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::ns;

/// Where one's clients keep, per chat, the last message read (XEP-0490): Gajim, Monal
/// and Conversations clear their unread counts from it.
pub const MDS: &str = "urn:xmpp:mds:displayed:0";

impl crate::Account {
    /// Tells the account's other clients that `chat` was read up to the message the
    /// archive `by` (the own account for chats, the room for rooms) calls `archive_id`.
    pub async fn read_up_to(
        &self,
        chat: &xmpp_parsers::jid::BareJid,
        by: &xmpp_parsers::jid::BareJid,
        archive_id: &str,
    ) -> Result<(), crate::Error> {
        let stanza_id = Element::builder("stanza-id", ns::SID)
            .attr(xml_ncname!("id").into(), archive_id)
            .attr(xml_ncname!("by").into(), by.as_str())
            .build();
        let displayed = Element::builder("displayed", MDS).append(stanza_id).build();
        self.publish(MDS, Some(chat.as_str()), displayed, crate::Access::Owner)
            .await
    }
}

/// The chats a notification of [`MDS`] says were read on another of one's clients.
pub fn read_elsewhere(items: &[crate::PepItem]) -> Vec<xmpp_parsers::jid::BareJid> {
    items
        .iter()
        .filter(|item| item.payload.is("displayed", MDS))
        .filter_map(|item| item.id.as_ref()?.parse().ok())
        .collect()
}

/// A fresh id for a message one sends.
pub fn new_id() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("system randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Names an outgoing message `id`, twice: as the stanza's id and as origin-id
/// (XEP-0359), which rooms keep. In a chat, asks for a receipt and a read marker.
pub fn mark_outgoing(message: &mut Message, id: &str) {
    message.id = Some(Id(id.to_owned()));
    message.payloads.push(
        Element::builder("origin-id", ns::SID)
            .attr(xml_ncname!("id").into(), id)
            .build(),
    );
    if message.type_ == MessageType::Chat {
        message
            .payloads
            .push(Element::builder("request", ns::RECEIPTS).build());
        message
            .payloads
            .push(Element::builder("markable", ns::DISPLAYED_MARKERS).build());
    }
}

/// What a message says about an earlier one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    /// It reached a client of the recipient.
    Received(String),
    /// The recipient has seen it, and all before it.
    Displayed(String),
}

/// The receipts and markers a message carries, also inside a carbon copy: one's own
/// other device that read a message marks it read here too.
pub fn marks(message: &Message) -> Vec<Mark> {
    let mut found = Vec::new();
    let mut look = |payloads: &[Element]| {
        for p in payloads {
            let id = p.attr(xml_ncname!("id")).map(str::to_owned);
            match id {
                Some(id) if p.is("received", ns::RECEIPTS) => found.push(Mark::Received(id)),
                Some(id) if p.is("received", ns::DISPLAYED_MARKERS) => found.push(Mark::Received(id)),
                Some(id) if p.is("displayed", ns::DISPLAYED_MARKERS) => found.push(Mark::Displayed(id)),
                _ => {}
            }
        }
    };
    look(&message.payloads);
    for p in &message.payloads {
        if p.is("sent", ns::CARBONS) || p.is("received", ns::CARBONS) {
            for inner in p
                .children()
                .flat_map(|f| f.children())
                .filter(|m| m.is("message", ns::DEFAULT_NS))
            {
                look(&inner.children().cloned().collect::<Vec<_>>());
            }
        }
    }
    found
}

/// A message of one's that came back as an error: its id, and why, in words. The
/// reason the server gives (for example "server does not accept other servers") follows
/// what the condition means.
pub fn bounce(message: &Message) -> Option<(String, String)> {
    use xmpp_parsers::stanza_error::{DefinedCondition as C, StanzaError};
    if message.type_ != MessageType::Error {
        return None;
    }
    let id = message.id.as_ref()?.0.clone();
    let error = message
        .payloads
        .iter()
        .find_map(|p| StanzaError::try_from(p.clone()).ok())?;
    let what = match error.defined_condition {
        C::PolicyViolation => "сервер получателя не принимает такие сообщения (policy-violation)",
        C::RemoteServerNotFound => "сервер получателя не найден",
        C::RemoteServerTimeout => "сервер получателя не отвечает",
        C::ServiceUnavailable => "получатель не принимает сообщения",
        C::ItemNotFound => "такого адреса нет",
        C::Forbidden | C::NotAllowed | C::NotAuthorized => "отказано",
        C::NotAcceptable => "сообщение не принято",
        C::ResourceConstraint => "у получателя не хватает места",
        C::RecipientUnavailable => "получатель недоступен",
        C::JidMalformed => "неверный адрес",
        _ => "ошибка",
    };
    let reason = match error.texts.values().next() {
        Some(text) if !text.trim().is_empty() => format!("{what}: {}", text.trim()),
        _ => what.to_owned(),
    };
    Some((id, reason))
}

/// Whether the sender asked for a receipt (only chats; never for an error).
pub fn wants_receipt(message: &Message) -> bool {
    message.type_ == MessageType::Chat && message.payloads.iter().any(|p| p.is("request", ns::RECEIPTS))
}

/// The receipt owed for `message`, if any: someone else (not `own` account) asked for it
/// on a message with something in it (text, or OMEMO), and named it.
pub fn answer(own: &xmpp_parsers::jid::BareJid, message: &Message) -> Option<Message> {
    let from = message.from.clone()?;
    let id = message.id.as_ref()?;
    let has_content = !message.bodies.is_empty()
        || message
            .payloads
            .iter()
            .any(|p| p.is("encrypted", "eu.siacs.conversations.axolotl"));
    (wants_receipt(message) && has_content && from.to_bare() != *own).then(|| receipt(from, &id.0))
}

/// The receipt for message `id`, to the very client that sent it.
pub fn receipt(to: Jid, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(to));
    message.payloads.push(
        Element::builder("received", ns::RECEIPTS)
            .attr(xml_ncname!("id").into(), id)
            .build(),
    );
    message
        .payloads
        .push(Element::builder("store", "urn:xmpp:hints").build());
    message
}

/// Says that message `id` (and all before it) was read, to the sender's account: its
/// other clients learn it too.
pub fn displayed(to: Jid, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(to));
    message.payloads.push(
        Element::builder("displayed", ns::DISPLAYED_MARKERS)
            .attr(xml_ncname!("id").into(), id)
            .build(),
    );
    message
        .payloads
        .push(Element::builder("store", "urn:xmpp:hints").build());
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outgoing_chats_ask_for_receipts_and_marks_are_read_back() {
        let mut chat = Message::new_with_type(MessageType::Chat, Some("bob@example.org".parse().unwrap()));
        mark_outgoing(&mut chat, "m1");
        assert!(wants_receipt(&chat));
        assert_eq!(chat.id, Some(Id("m1".into())));
        let mut room = Message::new_with_type(
            MessageType::Groupchat,
            Some("room@muc.example.org".parse().unwrap()),
        );
        mark_outgoing(&mut room, "m2");
        assert!(!wants_receipt(&room), "rooms take no receipts");
        assert!(room.payloads.iter().any(|p| p.is("origin-id", ns::SID)));

        let back = receipt("me@example.org/omtalk".parse().unwrap(), "m1");
        assert_eq!(marks(&back), [Mark::Received("m1".into())]);
        let read = displayed("me@example.org".parse().unwrap(), "m1");
        assert_eq!(marks(&read), [Mark::Displayed("m1".into())]);

        // The same inside a carbon: one's other device read it.
        let carbon: Element = r#"<message xmlns="jabber:client" from="me@example.org" to="me@example.org/omtalk">
            <sent xmlns="urn:xmpp:carbons:2"><forwarded xmlns="urn:xmpp:forward:0">
              <message xmlns="jabber:client" to="bob@example.org"><displayed xmlns="urn:xmpp:chat-markers:0" id="b7"/></message>
            </forwarded></sent></message>"#
            .parse()
            .unwrap();
        assert_eq!(
            marks(&Message::try_from(carbon).unwrap()),
            [Mark::Displayed("b7".into())]
        );

        // A receipt is owed to others for real messages that ask, never to oneself.
        let own: xmpp_parsers::jid::BareJid = "me@example.org".parse().unwrap();
        let mut asked =
            Message::chat(Some("me@example.org".parse().unwrap())).with_body(Default::default(), "hi".into());
        mark_outgoing(&mut asked, "q1");
        asked.from = Some("bob@example.org/gajim".parse().unwrap());
        let answer_ = answer(&own, &asked).expect("owed");
        assert_eq!(answer_.to, Some("bob@example.org/gajim".parse().unwrap()));
        assert_eq!(marks(&answer_), [Mark::Received("q1".into())]);
        asked.from = Some("me@example.org/phone".parse().unwrap());
        assert!(answer(&own, &asked).is_none(), "not to one's own devices");
        let mut empty = Message::chat(Some("me@example.org".parse().unwrap()));
        mark_outgoing(&mut empty, "q2");
        empty.from = Some("bob@example.org/gajim".parse().unwrap());
        assert!(
            answer(&own, &empty).is_none(),
            "nothing in it, nothing to confirm"
        );

        // An error back for one's message names it and says why, the server's words too.
        let error: Element = r#"<message xmlns="jabber:client" type="error" id="q1" from="bob@far.example">
            <error type="cancel"><policy-violation xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/>
              <text xmlns="urn:ietf:params:xml:ns:xmpp-stanzas">Server does not accept other servers</text></error></message>"#
            .parse()
            .unwrap();
        let (id, reason) = bounce(&Message::try_from(error).unwrap()).unwrap();
        assert_eq!(id, "q1");
        assert!(
            reason.contains("не принимает") && reason.contains("does not accept"),
            "{reason}"
        );
        assert!(bounce(&asked).is_none());
    }
}
