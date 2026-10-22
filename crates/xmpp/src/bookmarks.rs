//! The account's room bookmarks (XEP-0402), shared by its clients: Gajim, Conversations,
//! Monal and Dino keep the rooms one is in, and the nick used there, here. Entering a
//! room under the same nick as the other clients makes one occupant with several
//! connections; a different nick makes two.

use xmpp_parsers::bookmarks2::Conference;
use xmpp_parsers::jid::BareJid;
use xmpp_parsers::minidom::Element;

use crate::pep::Access;
use crate::{Account, Error};

pub const NODE: &str = "urn:xmpp:bookmarks:1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub room: BareJid,
    pub name: Option<String>,
    pub nick: Option<String>,
    pub autojoin: bool,
}

impl Account {
    /// The bookmarks, or none if the account has never had any.
    pub async fn bookmarks(&self) -> Result<Vec<Bookmark>, Error> {
        let own = self.jid().clone();
        let items = match self.items(&own, NODE).await {
            Ok(items) => items,
            Err(Error::Stanza(e))
                if e.defined_condition == xmpp_parsers::stanza_error::DefinedCondition::ItemNotFound =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        Ok(items
            .into_iter()
            .filter_map(|item| {
                let room = item.id?.parse().ok()?;
                let conference = Conference::try_from(item.payload).ok()?;
                Some(Bookmark {
                    room,
                    name: conference.name,
                    nick: conference.nick.map(|n| n.to_string()),
                    autojoin: conference.autojoin,
                })
            })
            .collect())
    }

    /// Adds or replaces the bookmark for `bookmark.room`, visible to this account only.
    pub async fn bookmark(&self, bookmark: &Bookmark) -> Result<(), Error> {
        let mut conference = Element::builder("conference", NODE);
        if bookmark.autojoin {
            conference = conference.attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("autojoin").into(),
                "true",
            );
        }
        if let Some(name) = &bookmark.name {
            conference = conference.attr(
                xmpp_parsers::minidom::rxml::xml_ncname!("name").into(),
                name.as_str(),
            );
        }
        if let Some(nick) = &bookmark.nick {
            conference = conference.append(Element::builder("nick", NODE).append(nick.as_str()).build());
        }
        self.publish(
            NODE,
            Some(bookmark.room.as_str()),
            conference.build(),
            Access::Owner,
        )
        .await
    }
}
