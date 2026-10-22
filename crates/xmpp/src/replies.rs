//! Replies (XEP-0461): a message that answers an earlier one names it, and starts with
//! a quote of it as "> " lines, which a fallback marker (XEP-0428) shows to be only the
//! quote. Clients that know replies show the quote their own way; others show the
//! lines, which read well too.

use xmpp_parsers::jid::Jid;
use xmpp_parsers::message::Message;
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;

pub const NS: &str = "urn:xmpp:reply:0";
const FALLBACK: &str = "urn:xmpp:fallback:0";

/// The message answered: its id (a room's stanza-id in a room, the sender's id in a
/// chat) and, if known, who wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTo {
    pub id: String,
    pub to: Option<Jid>,
}

/// A reply's body: `quote` as "> " lines, then `text`; and how long the quote part is,
/// in characters, for the fallback marker.
pub fn body(quote: &str, text: &str) -> (String, usize) {
    let quoted: String = quote.lines().map(|l| format!("> {l}\n")).collect();
    let length = quoted.chars().count();
    (format!("{quoted}{text}"), length)
}

/// The elements that make a message a reply to `to`, whose body begins with a quote of
/// `quote_length` characters.
pub fn elements(to: &ReplyTo, quote_length: usize) -> Vec<Element> {
    let mut reply = Element::builder("reply", NS).attr(xml_ncname!("id").into(), to.id.as_str());
    if let Some(jid) = &to.to {
        reply = reply.attr(xml_ncname!("to").into(), jid.to_string());
    }
    let fallback = Element::builder("fallback", FALLBACK)
        .attr(xml_ncname!("for").into(), NS)
        .append(
            Element::builder("body", FALLBACK)
                .attr(xml_ncname!("start").into(), "0")
                .attr(xml_ncname!("end").into(), quote_length.to_string())
                .build(),
        )
        .build();
    vec![reply.build(), fallback]
}

/// The message a reply answers, if it is one.
pub fn reply_of(message: &Message) -> Option<ReplyTo> {
    let reply = message.payloads.iter().find(|p| p.is("reply", NS))?;
    Some(ReplyTo {
        id: reply.attr(xml_ncname!("id"))?.to_owned(),
        to: reply.attr(xml_ncname!("to")).and_then(|t| t.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_quotes_and_names_what_it_answers() {
        let (text, length) = body("первая\nвторая", "отвечаю");
        assert_eq!(text, "> первая\n> вторая\nотвечаю");
        assert_eq!(length, "> первая\n> вторая\n".chars().count());

        let to = ReplyTo {
            id: "m-17".into(),
            to: Some("room@muc.example.org/Алиса".parse().unwrap()),
        };
        let mut message = Message::chat(Some("room@muc.example.org".parse().unwrap()));
        message.payloads.extend(elements(&to, length));
        assert_eq!(reply_of(&message), Some(to));
        let fallback = message
            .payloads
            .iter()
            .find(|p| p.is("fallback", FALLBACK))
            .unwrap();
        assert_eq!(
            fallback
                .get_child("body", FALLBACK)
                .unwrap()
                .attr(xml_ncname!("end")),
            Some(length.to_string().as_str())
        );
        assert_eq!(reply_of(&Message::chat(None)), None);
    }
}
