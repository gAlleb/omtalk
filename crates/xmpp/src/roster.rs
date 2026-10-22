//! The contact list (RFC 6121): who is in it, and the presence subscriptions that decide
//! who sees whose status.

use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid::BareJid;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::roster::{Item, Roster, Subscription};

use crate::{Account, Error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub jid: BareJid,
    pub name: Option<String>,
    /// We see their status.
    pub we_see_them: bool,
    /// They see ours.
    pub they_see_us: bool,
    /// We asked to see their status and they have not answered yet.
    pub asked: bool,
    /// Gone from the roster (a push removing it).
    pub removed: bool,
}

impl From<Item> for Contact {
    fn from(item: Item) -> Self {
        let (we_see_them, they_see_us) = match item.subscription {
            Subscription::Both => (true, true),
            Subscription::To => (true, false),
            Subscription::From => (false, true),
            Subscription::None | Subscription::Remove => (false, false),
        };
        Self {
            jid: item.jid,
            name: item.name,
            we_see_them,
            they_see_us,
            asked: item.ask == xmpp_parsers::roster::Ask::Subscribe,
            removed: item.subscription == Subscription::Remove,
        }
    }
}

impl Account {
    pub async fn roster(&self) -> Result<Vec<Contact>, Error> {
        let query = Roster {
            ver: None,
            items: Vec::new(),
        };
        let answer = self.get(None, query).await?;
        let Some(answer) = answer else {
            return Ok(Vec::new());
        };
        let roster = Roster::try_from(answer).map_err(|e| Error::Unexpected(e.to_string()))?;
        Ok(roster.items.into_iter().map(Contact::from).collect())
    }

    /// Adds `jid` to the contact list and asks to see their status.
    pub async fn add_contact(&self, jid: &BareJid, name: Option<&str>) -> Result<(), Error> {
        let item = Item {
            jid: jid.clone(),
            name: name.map(Into::into),
            subscription: Subscription::None,
            ask: Default::default(),
            approved: None,
            groups: Vec::new(),
        };
        self.set(
            None,
            Roster {
                ver: None,
                items: vec![item],
            },
        )
        .await?;
        self.send(Presence::subscribe().with_to(jid.clone())).await
    }

    /// Lets `jid` see our status, and asks to see theirs, as clients do on accepting.
    pub async fn accept_contact(&self, jid: &BareJid) -> Result<(), Error> {
        self.send(Presence::subscribed().with_to(jid.clone())).await?;
        self.send(Presence::subscribe().with_to(jid.clone())).await
    }

    /// Refuses (or withdraws) letting `jid` see our status.
    pub async fn refuse_contact(&self, jid: &BareJid) -> Result<(), Error> {
        self.send(Presence::new(PresenceType::Unsubscribed).with_to(jid.clone()))
            .await
    }

    pub async fn remove_contact(&self, jid: &BareJid) -> Result<(), Error> {
        let item = Item {
            jid: jid.clone(),
            name: None,
            subscription: Subscription::Remove,
            ask: Default::default(),
            approved: None,
            groups: Vec::new(),
        };
        self.set(
            None,
            Roster {
                ver: None,
                items: vec![item],
            },
        )
        .await
        .map(drop)
    }
}

/// A roster push: the server telling of a change, which the client must acknowledge.
/// Only the own server may push; from anybody else it is not a push.
pub(crate) fn push(iq: &Iq, own: &BareJid) -> Option<Vec<Contact>> {
    let Iq::Set { from, payload, .. } = iq else {
        return None;
    };
    if from
        .as_ref()
        .is_some_and(|from| from.resource().is_some() || from.to_bare() != *own)
    {
        return None;
    }
    let roster = Roster::try_from(payload.clone()).ok()?;
    Some(roster.items.into_iter().map(Contact::from).collect())
}

/// Someone asking to see our status.
pub(crate) fn subscription_request(presence: &Presence) -> Option<BareJid> {
    (presence.type_ == PresenceType::Subscribe).then(|| presence.from.as_ref().map(|f| f.to_bare()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster_set(from: Option<&str>) -> Iq {
        let item = Item {
            jid: "friend@example.org".parse().unwrap(),
            name: None,
            subscription: Subscription::Both,
            ask: Default::default(),
            approved: None,
            groups: Vec::new(),
        };
        let mut iq = Iq::from_set(
            "push1",
            Roster {
                ver: None,
                items: vec![item],
            },
        );
        if let Some(from) = from {
            iq = iq.with_from(from.parse().unwrap());
        }
        iq
    }

    #[test]
    fn pushes_only_from_the_own_server() {
        let own: BareJid = "me@example.org".parse().unwrap();
        assert_eq!(
            push(&roster_set(None), &own).unwrap()[0].jid.as_str(),
            "friend@example.org"
        );
        assert!(push(&roster_set(Some("me@example.org")), &own).is_some());
        assert!(push(&roster_set(Some("mallory@evil.example")), &own).is_none());
        assert!(
            push(&roster_set(Some("me@example.org/other")), &own).is_none(),
            "a resource is not the server"
        );
    }
}
