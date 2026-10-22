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
    /// Leaving under this nick to come back under another (status 303): the new one.
    pub new_nick: Option<String>,
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
        new_nick: user
            .status
            .contains(&Status::NewNick)
            .then(|| item.nick.clone())
            .flatten(),
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
    /// The account's server's room service (muc.example.org and the like), if it has one.
    pub async fn rooms_service(&self) -> Result<Option<BareJid>, Error> {
        let server: Jid = self.jid().domain().as_str().parse().expect("a domain is a JID");
        for item in self.disco_items(&server, None).await? {
            let Ok(info) = self.disco_info(&item.jid, None).await else {
                continue;
            };
            if info
                .identities
                .iter()
                .any(|i| i.category == "conference" && i.kind == "text")
            {
                return Ok(Some(item.jid.to_bare()));
            }
        }
        Ok(None)
    }

    /// Invites `jid` to the room directly (XEP-0249), as Gajim and Conversations do: the
    /// invitation goes to them, not through the room (Prosody refuses that kind with
    /// service-unavailable). Membership is the room's business: see
    /// [`Account::set_affiliation`].
    pub async fn invite(&self, room: &BareJid, jid: &BareJid) -> Result<(), Error> {
        let invite = Element::builder("x", "jabber:x:conference")
            .attr(xml_ncname!("jid").into(), room.as_str())
            .build();
        let mut message = xmpp_parsers::message::Message::new(Some(jid.clone().into()));
        message.payloads.push(invite);
        self.send(message).await
    }

    /// The room's own name (its disco#info identity), if it has one.
    pub async fn room_name(&self, room: &BareJid) -> Result<Option<String>, Error> {
        let info = self.disco_info(&room.clone().into(), None).await?;
        Ok(info
            .identities
            .into_iter()
            .find(|i| i.category == "conference")
            .and_then(|i| i.name)
            .filter(|n| !n.trim().is_empty()))
    }

    /// Names the room, as its owner (muc#roomconfig_roomname).
    pub async fn set_room_name(&self, room: &BareJid, name: &str) -> Result<(), Error> {
        let form = DataForm::new(
            DataFormType::Submit,
            ROOM_CONFIG,
            vec![Field::text_single("muc#roomconfig_roomname", name)],
        );
        let query = Element::builder("query", MUC_OWNER)
            .append(Element::from(form))
            .build();
        self.set(Some(room.clone().into()), query).await.map(drop)
    }

    /// Sets the room's subject (its topic); the room tells everyone, and refuses if one
    /// may not.
    pub async fn set_subject(&self, room: &BareJid, subject: &str) -> Result<(), Error> {
        let mut message = xmpp_parsers::message::Message::groupchat(Some(room.clone().into()));
        message
            .subjects
            .insert(xmpp_parsers::message::Lang::default(), subject.to_owned());
        self.send(message).await
    }

    pub async fn make_private(&self, room: &BareJid) -> Result<(), Error> {
        self.make_private_named(room, None).await
    }

    /// [`Account::make_private`], naming the room too. A room just made stays locked
    /// until this is done.
    pub async fn make_private_named(&self, room: &BareJid, name: Option<&str>) -> Result<(), Error> {
        let mut fields = vec![
            Field::text_single("muc#roomconfig_membersonly", "1"),
            Field::text_single("muc#roomconfig_whois", "anyone"),
            Field::text_single("muc#roomconfig_persistentroom", "1"),
        ];
        if let Some(name) = name {
            fields.push(Field::text_single("muc#roomconfig_roomname", name));
        }
        let form = DataForm::new(DataFormType::Submit, ROOM_CONFIG, fields);
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

/// A room's subject, if `message` sets it: the room, who set it (a nick; none when the
/// room tells it on entering), and the subject (empty: none).
pub fn subject_of(message: &xmpp_parsers::message::Message) -> Option<(BareJid, Option<String>, String)> {
    if message.type_ != xmpp_parsers::message::MessageType::Groupchat || !message.bodies.is_empty() {
        return None;
    }
    let subject = message.subjects.values().next()?.clone();
    let from = message.from.as_ref()?;
    let nick = from.resource().map(|r| r.to_string());
    Some((from.to_bare(), nick, subject))
}

/// Whether a room says its configuration changed (status 104): its name may be new.
pub fn config_changed(message: &xmpp_parsers::message::Message) -> Option<BareJid> {
    let changed = message
        .payloads
        .iter()
        .filter_map(|p| MucUser::try_from(p.clone()).ok())
        .any(|user| user.status.contains(&Status::ConfigNonPrivacyRelated));
    changed
        .then(|| message.from.as_ref().map(|f| f.to_bare()))
        .flatten()
}

/// An invitation to a room, either kind: through the room (XEP-0045, the room sends it
/// on someone's behalf) or direct (XEP-0249, as Conversations and Gajim send). The room,
/// and who invites, if said.
pub fn invitation_of(message: &xmpp_parsers::message::Message) -> Option<(BareJid, Option<BareJid>)> {
    for payload in &message.payloads {
        if payload.is("x", "jabber:x:conference") {
            let room = payload.attr(xml_ncname!("jid"))?.parse().ok()?;
            return Some((room, message.from.as_ref().map(|f| f.to_bare())));
        }
        if payload.is("x", ns::MUC_USER)
            && let Some(invite) = payload.get_child("invite", ns::MUC_USER)
        {
            let room = message.from.as_ref()?.to_bare();
            let from = invite
                .attr(xml_ncname!("from"))
                .and_then(|f| f.parse::<Jid>().ok())
                .map(|j| j.to_bare());
            return Some((room, from));
        }
    }
    None
}

#[cfg(test)]
mod invitation_tests {
    use super::*;

    #[test]
    fn both_kinds_of_invitation_are_read() {
        let direct: Element =
            r#"<message xmlns="jabber:client" from="alice@example.org/gajim" to="me@example.org">
            <x xmlns="jabber:x:conference" jid="team@muc.example.org"/></message>"#
                .parse()
                .unwrap();
        let direct = xmpp_parsers::message::Message::try_from(direct).unwrap();
        assert_eq!(
            invitation_of(&direct),
            Some((
                "team@muc.example.org".parse().unwrap(),
                Some("alice@example.org".parse().unwrap())
            ))
        );
        let mediated: Element = r#"<message xmlns="jabber:client" from="team@muc.example.org" to="me@example.org">
            <x xmlns="http://jabber.org/protocol/muc#user"><invite from="alice@example.org/omtalk"/></x></message>"#
            .parse()
            .unwrap();
        let mediated = xmpp_parsers::message::Message::try_from(mediated).unwrap();
        assert_eq!(
            invitation_of(&mediated),
            Some((
                "team@muc.example.org".parse().unwrap(),
                Some("alice@example.org".parse().unwrap())
            ))
        );
        assert_eq!(invitation_of(&xmpp_parsers::message::Message::chat(None)), None);
    }
}
