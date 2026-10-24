//! The name people give themselves (XEP-0172): published in their PEP, sent to contacts
//! whose clients ask for it ("+notify"). A name of one's own choosing in the roster
//! comes first; this one is for those who have none there.

use xmpp_parsers::minidom::Element;

use crate::{Access, Account, Error, PepItem};

pub const NODE: &str = "http://jabber.org/protocol/nick";

impl Account {
    /// Gives oneself a name, for those who have none for one in their roster; `None`
    /// takes it away (an empty one, as XEP-0172 says). Seen by those who see one's status.
    pub async fn set_nickname(&self, nick: Option<&str>) -> Result<(), Error> {
        let mut payload = Element::builder("nick", NODE);
        if let Some(nick) = nick {
            payload = payload.append(nick);
        }
        self.publish(NODE, Some("current"), payload.build(), Access::Presence)
            .await
    }
}

/// The name in a notification of the node: `Some(None)` when it was taken away (or is
/// empty), `None` when the items say nothing of it.
pub fn of(items: &[PepItem]) -> Option<Option<String>> {
    if items.is_empty() {
        return Some(None);
    }
    let nick = items.iter().map(|i| &i.payload).find(|p| p.is("nick", NODE))?;
    let text = nick.text();
    let text: String = text.trim().chars().filter(|c| !c.is_control()).take(64).collect();
    Some((!text.is_empty()).then_some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(payload: Element) -> PepItem {
        PepItem {
            id: Some("current".into()),
            payload,
        }
    }

    #[test]
    fn a_published_name_is_read_and_one_taken_away_is_none() {
        let nick = Element::builder("nick", NODE).append("  Мама ").build();
        assert_eq!(of(&[item(nick)]), Some(Some("Мама".into())));
        let empty = Element::builder("nick", NODE).build();
        assert_eq!(of(&[item(empty)]), Some(None));
        assert_eq!(of(&[]), Some(None), "retracted");
        let other = Element::builder("x", "urn:example").build();
        assert_eq!(of(&[item(other)]), None);
    }
}
