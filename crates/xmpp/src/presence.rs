//! Who is online, and on what: a contact's (or one's own) connected clients, each with
//! its state and, from the caps it announces (XEP-0115), which program it is.

use xmpp_parsers::jid::BareJid;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::ns;
use xmpp_parsers::presence::{Presence, Show, Type};

/// One connected client of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Online {
    pub account: BareJid,
    pub resource: String,
    /// `None`: it went offline.
    pub state: Option<State>,
    /// The program, as far as it says: "Gajim", "Monal", "omtalk"…
    pub client: String,
    /// Its own words, if it gives any ("на встрече").
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Chat,
    Online,
    Away,
    Dnd,
    Xa,
}

/// What a presence says about a client, if it is about one: not a room's (see
/// [`crate::muc`]), not a subscription, not an error.
pub fn online(presence: &Presence) -> Option<Online> {
    let from = presence.from.as_ref()?.try_as_full().ok()?.clone();
    if presence.payloads.iter().any(|p| p.is("x", ns::MUC_USER)) {
        return None;
    }
    let state = match presence.type_ {
        Type::None => Some(match presence.show {
            None => State::Online,
            Some(Show::Chat) => State::Chat,
            Some(Show::Away) => State::Away,
            Some(Show::Dnd) => State::Dnd,
            Some(Show::Xa) => State::Xa,
        }),
        Type::Unavailable => None,
        _ => return None,
    };
    let node = presence
        .payloads
        .iter()
        .find(|p| p.is("c", ns::CAPS))
        .and_then(|c| c.attr(xml_ncname!("node")))
        .unwrap_or_default();
    let resource = from.resource().to_string();
    Some(Online {
        account: from.to_bare(),
        client: client_name(node, &resource),
        resource,
        state,
        status: presence
            .statuses
            .values()
            .next()
            .cloned()
            .filter(|s| !s.trim().is_empty()),
    })
}

/// The program behind a caps node (its home page, by custom), or behind the resource
/// some clients name themselves in; else what the resource says.
pub fn client_name(node: &str, resource: &str) -> String {
    let known = [
        ("gajim", "Gajim"),
        ("monal", "Monal"),
        ("conversations", "Conversations"),
        ("dino", "Dino"),
        ("omtalk", "omtalk"),
        ("profanity", "Profanity"),
        ("poez", "Poezio"),
        ("psi", "Psi"),
        ("pidgin", "Pidgin"),
        ("cheogram", "Cheogram"),
        ("siskin", "Siskin"),
        ("beagle", "Beagle"),
        ("movim", "Movim"),
        ("blabber", "blabber.im"),
        ("monocles", "monocles chat"),
        ("converse", "Converse.js"),
        ("kaidan", "Kaidan"),
    ];
    let node = node.to_lowercase();
    let resource_lower = resource.to_lowercase();
    for (mark, name) in known {
        if node.contains(mark) {
            return name.to_owned();
        }
    }
    for (mark, name) in known {
        if resource_lower.starts_with(mark) {
            return name.to_owned();
        }
    }
    // Nothing to tell it by: the interface names it in its own words.
    resource.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::minidom::Element;

    fn presence(xml: &str) -> Presence {
        Presence::try_from(xml.parse::<Element>().unwrap()).unwrap()
    }

    #[test]
    fn clients_are_named_by_caps_or_resource_and_states_read() {
        let gajim = presence(
            r#"<presence xmlns="jabber:client" from="bob@example.org/gajim.X1Y2"><show>away</show><status>обед</status>
               <c xmlns="http://jabber.org/protocol/caps" hash="sha-1" node="https://gajim.org" ver="abc="/></presence>"#,
        );
        let online = online(&gajim).unwrap();
        assert_eq!(
            (online.client.as_str(), online.state, online.status.as_deref()),
            ("Gajim", Some(State::Away), Some("обед"))
        );
        assert_eq!(online.resource, "gajim.X1Y2");

        let monal = presence(r#"<presence xmlns="jabber:client" from="bob@example.org/Monal-iOS.5d1e"/>"#);
        assert_eq!(
            super::online(&monal).unwrap().client,
            "Monal",
            "by resource when there are no caps"
        );

        let gone = presence(
            r#"<presence xmlns="jabber:client" type="unavailable" from="bob@example.org/gajim.X1Y2"/>"#,
        );
        assert_eq!(super::online(&gone).unwrap().state, None);

        let room = presence(
            r#"<presence xmlns="jabber:client" from="room@muc.example.org/bob">
            <x xmlns="http://jabber.org/protocol/muc#user"/></presence>"#,
        );
        assert!(super::online(&room).is_none(), "a room's occupant, not a client");
        let ask = presence(r#"<presence xmlns="jabber:client" type="subscribe" from="bob@example.org"/>"#);
        assert!(super::online(&ask).is_none());
        assert_eq!(client_name("", "laptop"), "laptop");
    }
}
