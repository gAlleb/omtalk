//! Group chats (XEP-0045): what OMEMO in a room needs. A room must be members-only and
//! show real JIDs to everyone, so each member knows whose devices to encrypt for.

use xmpp_parsers::data_forms::{DataForm, DataFormType, Field};
use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::muc::Muc;
use xmpp_parsers::muc::muc::History;
use xmpp_parsers::muc::user::{Affiliation, MucUser, Role, Status};
use xmpp_parsers::ns;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

use crate::{Account, Error};

const MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const ROOM_CONFIG: &str = "http://jabber.org/protocol/muc#roomconfig";

/// Someone in a room, as their presence describes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Occupant {
    pub room: BareJid,
    pub nick: String,
    /// Known in non-anonymous rooms only.
    pub real_jid: Option<BareJid>,
    pub affiliation: Affiliation,
    pub role: Role,
    /// Whether the presence is about one of our own sessions in the room.
    pub is_self: bool,
    /// `false` when they left.
    pub present: bool,
}

/// Reads a presence from a room occupant; `None` for any other presence.
pub fn occupant(presence: &Presence) -> Option<Occupant> {
    let from = presence.from.as_ref()?.try_as_full().ok()?;
    let user = presence
        .payloads
        .iter()
        .find(|p| p.is("x", ns::MUC_USER))
        .and_then(|p| MucUser::try_from(p.clone()).ok())?;
    let item = user.items.first()?;
    Some(Occupant {
        room: from.to_bare(),
        nick: from.resource().to_string(),
        real_jid: item.jid.as_ref().map(|jid| jid.to_bare()),
        affiliation: item.affiliation.clone(),
        role: item.role.clone(),
        is_self: user.status.contains(&Status::SelfPresence),
        present: presence.type_ != PresenceType::Unavailable,
    })
}

/// Whether a presence from a room answers our entering it: our own presence back (status
/// 110) lets us in, an error presence refuses.
pub(crate) fn join_outcome(presence: &Presence) -> Option<(BareJid, Result<(), Error>)> {
    let room = presence.from.as_ref()?.to_bare();
    if presence.type_ == PresenceType::Error {
        let error = presence
            .payloads
            .iter()
            .find_map(|p| xmpp_parsers::stanza_error::StanzaError::try_from(p.clone()).ok())?;
        return Some((room, Err(Error::Stanza(Box::new(error)))));
    }
    let entered = occupant(presence).is_some_and(|o| o.is_self && o.present);
    entered.then_some((room, Ok(())))
}

impl Account {
    /// Enters a room under `nick`, without the room's history (the archive has it), and
    /// returns once the room has let us in. A refusal comes back as `Error::Stanza`:
    /// `registration-required` for a members-only room without an invitation,
    /// `conflict` for a nick in use, `forbidden` for a ban. Creates the room if it does
    /// not exist yet; configure it with [`Account::make_private`] right after.
    pub async fn join(&self, room: &BareJid, nick: &str) -> Result<(), Error> {
        let to = room
            .with_resource_str(nick)
            .map_err(|e| Error::Unexpected(e.to_string()))?;
        let muc = Muc::new().with_history(History::new().with_maxstanzas(0));
        self.join_room(room.clone(), Presence::available().with_to(to).with_payload(muc))
            .await
    }

    pub async fn leave(&self, room: &BareJid, nick: &str) -> Result<(), Error> {
        let to = room
            .with_resource_str(nick)
            .map_err(|e| Error::Unexpected(e.to_string()))?;
        self.send(Presence::unavailable().with_to(to)).await
    }

    /// Configures a room we own for OMEMO: members-only, real JIDs visible to all,
    /// persistent.
    pub async fn make_private(&self, room: &BareJid) -> Result<(), Error> {
        let form = DataForm::new(
            DataFormType::Submit,
            ROOM_CONFIG,
            vec![
                Field::text_single("muc#roomconfig_membersonly", "1"),
                Field::text_single("muc#roomconfig_whois", "anyone"),
                Field::text_single("muc#roomconfig_persistentroom", "1"),
            ],
        );
        let query = Element::builder("query", MUC_OWNER)
            .append(Element::from(form))
            .build();
        self.set(Some(room.clone().into()), query).await.map(drop)
    }

    pub async fn set_affiliation(
        &self,
        room: &BareJid,
        jid: &BareJid,
        affiliation: Affiliation,
    ) -> Result<(), Error> {
        let item = Element::builder("item", MUC_ADMIN)
            .attr(xml_ncname!("jid").into(), jid.as_str())
            .attr(xml_ncname!("affiliation").into(), affiliation_name(&affiliation))
            .build();
        let query = Element::builder("query", MUC_ADMIN).append(item).build();
        self.set(Some(room.clone().into()), query).await.map(drop)
    }

    /// The accounts holding `affiliation` in the room, whether present or not.
    pub async fn affiliated(&self, room: &BareJid, affiliation: Affiliation) -> Result<Vec<BareJid>, Error> {
        let item = Element::builder("item", MUC_ADMIN)
            .attr(xml_ncname!("affiliation").into(), affiliation_name(&affiliation))
            .build();
        let query = Element::builder("query", MUC_ADMIN).append(item).build();
        let answer = self
            .get(Some(Jid::from(room.clone())), query)
            .await?
            .ok_or_else(|| Error::Unexpected("empty affiliation list answer".into()))?;
        Ok(answer
            .children()
            .filter(|item| item.is("item", MUC_ADMIN))
            .filter_map(|item| item.attr(xml_ncname!("jid"))?.parse::<Jid>().ok())
            .map(|jid| jid.to_bare())
            .collect())
    }
}

fn affiliation_name(affiliation: &Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}
