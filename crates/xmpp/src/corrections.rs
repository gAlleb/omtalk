//! Corrections (XEP-0308): a message that replaces an earlier one names it, and its body
//! is the text as it should have been. The id named is always the first message's, also
//! for a correction of a correction.

use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;

pub const NS: &str = "urn:xmpp:message-correct:0";

/// The element that makes a message a correction of the one with `id`.
pub fn element(id: &str) -> Element {
    Element::builder("replace", NS)
        .attr(xml_ncname!("id").into(), id)
        .build()
}

/// The id of the message this one corrects, if it is a correction.
pub fn replaces(message: &Message) -> Option<String> {
    message
        .payloads
        .iter()
        .find(|p| p.is("replace", NS))?
        .attr(xml_ncname!("id"))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_correction_names_the_message_it_replaces() {
        let mut message = Message::chat(Some("bob@example.org".parse().unwrap()));
        assert_eq!(replaces(&message), None);
        message.payloads.push(element("m-17"));
        assert_eq!(replaces(&message).as_deref(), Some("m-17"));
    }
}
