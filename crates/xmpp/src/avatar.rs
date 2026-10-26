//! Avatars, as Gajim, Conversations, Monal and Dino show them: published in PEP
//! (XEP-0084), where a change is pushed to us; else in the vCard (XEP-0054), whose
//! hash rides on presence (XEP-0153). Rooms keep theirs in the room's vCard. An avatar
//! is named by the SHA-1 of its bytes, which is checked: a cache can trust its name.

use base64::Engine;
use sha1::{Digest, Sha1};
use xmpp_parsers::jid::{BareJid, Jid};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::minidom::rxml::xml_ncname;
use xmpp_parsers::presence::Presence;
use xmpp_parsers::pubsub::pubsub::{Item, Items, PubSub};
use xmpp_parsers::pubsub::{ItemId, NodeName};

use crate::{Access, Error, PepItem};

pub const METADATA: &str = "urn:xmpp:avatar:metadata";
pub const DATA: &str = "urn:xmpp:avatar:data";
const VCARD: &str = "vcard-temp";
const VCARD_UPDATE: &str = "vcard-temp:x:update";

/// The SHA-1 of `bytes`, in hex: an avatar's name.
pub fn hash(bytes: &[u8]) -> String {
    Sha1::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// What a metadata item says: `Some(id)` of the avatar now, `None` when it was taken
/// away (an empty `<metadata/>`). Not one: `None`.
pub fn metadata_of(items: &[PepItem]) -> Option<Option<String>> {
    let metadata = items
        .iter()
        .map(|item| &item.payload)
        .find(|p| p.is("metadata", METADATA))?;
    // Any picture will do; PNG first, which every client publishes.
    let infos: Vec<&Element> = metadata.children().filter(|c| c.is("info", METADATA)).collect();
    let png = infos
        .iter()
        .find(|i| i.attr(xml_ncname!("type")) == Some("image/png"))
        .or(infos.first());
    Some(png.and_then(|i| i.attr(xml_ncname!("id"))).map(str::to_lowercase))
}

/// The avatar hash a presence announces (XEP-0153): `Some(Some(hash))`, `Some(None)` for
/// "none", `None` when it says nothing about it.
pub fn photo_of(presence: &Presence) -> Option<Option<String>> {
    let update = presence.payloads.iter().find(|p| p.is("x", VCARD_UPDATE))?;
    let photo = update.get_child("photo", VCARD_UPDATE)?;
    let text = photo.text().trim().to_lowercase();
    Some((!text.is_empty()).then_some(text))
}

/// The picture in a vCard, if it has one.
pub fn vcard_photo(vcard: &Element) -> Option<Vec<u8>> {
    let binval = vcard.get_child("PHOTO", VCARD)?.get_child("BINVAL", VCARD)?;
    let text: String = binval.text().chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

impl crate::Account {
    /// Update only PHOTO, preserving the room's other vCard fields.
    pub async fn set_room_avatar(&self, room: &BareJid, png: Option<&[u8]>) -> Result<(), Error> {
        let mut card = self
            .get(
                Some(room.clone().into()),
                Element::builder("vCard", VCARD).build(),
            )
            .await?
            .unwrap_or_else(|| Element::builder("vCard", VCARD).build());
        while card.remove_child("PHOTO", VCARD).is_some() {}
        if let Some(png) = png {
            card.append_child(
                Element::builder("PHOTO", VCARD)
                    .append(Element::builder("TYPE", VCARD).append("image/png").build())
                    .append(
                        Element::builder("BINVAL", VCARD)
                            .append(base64::engine::general_purpose::STANDARD.encode(png))
                            .build(),
                    )
                    .build(),
            );
        }
        self.set(Some(room.clone().into()), card).await.map(drop)
    }

    /// The avatar `id` of `owner`, from PEP, checked against its name.
    pub async fn avatar(&self, owner: &BareJid, id: &str) -> Result<Vec<u8>, Error> {
        let to = (owner != self.jid()).then(|| owner.clone().into());
        let request = PubSub::Items(Items {
            max_items: None,
            node: NodeName(DATA.to_owned()),
            subid: None,
            items: vec![Item {
                id: Some(ItemId(id.to_owned())),
                publisher: None,
                payload: None,
            }],
        });
        let items = crate::pep::parse_items(self.get(to, request).await?)?;
        let data = items
            .iter()
            .filter(|item| item.id.as_deref().is_none_or(|i| i.eq_ignore_ascii_case(id)))
            .find_map(|item| xmpp_parsers::avatar::Data::try_from(item.payload.clone()).ok())
            .ok_or_else(|| Error::Unexpected("no avatar data".into()))?;
        if hash(&data.data) != id.to_lowercase() {
            return Err(Error::Unexpected("avatar data does not match its id".into()));
        }
        Ok(data.data)
    }

    /// The avatar id `owner` publishes now, if any: for when no notification brought it
    /// (someone without a subscription, a room member).
    pub async fn avatar_id(&self, owner: &BareJid) -> Result<Option<String>, Error> {
        let items = self.items(owner, METADATA).await?;
        Ok(metadata_of(&items).flatten())
    }

    /// The picture in the vCard of `of` (a person, or a room), if any.
    pub async fn vcard_avatar(&self, of: &BareJid) -> Result<Option<Vec<u8>>, Error> {
        let to: Option<Jid> = (of != self.jid()).then(|| of.clone().into());
        let answer = self.get(to, Element::builder("vCard", VCARD).build()).await?;
        Ok(answer.as_ref().and_then(vcard_photo))
    }

    /// Publishes `bytes` (a PNG) as the account's avatar, readable by anyone.
    pub async fn publish_avatar(&self, bytes: &[u8], width: u16, height: u16) -> Result<(), Error> {
        let id = hash(bytes);
        let data = Element::builder("data", DATA)
            .append(base64::engine::general_purpose::STANDARD.encode(bytes))
            .build();
        self.publish(DATA, Some(&id), data, Access::Open).await?;
        let info = Element::builder("info", METADATA)
            .attr(xml_ncname!("id").into(), id.as_str())
            .attr(xml_ncname!("bytes").into(), bytes.len().to_string())
            .attr(xml_ncname!("type").into(), "image/png")
            .attr(xml_ncname!("width").into(), width.to_string())
            .attr(xml_ncname!("height").into(), height.to_string())
            .build();
        let metadata = Element::builder("metadata", METADATA).append(info).build();
        self.publish(METADATA, Some(&id), metadata, Access::Open).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avatars_are_named_by_their_hash_and_read_from_three_places() {
        assert_eq!(hash(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");

        let metadata: Element = r#"<metadata xmlns="urn:xmpp:avatar:metadata">
            <info id="AAA" bytes="1" type="image/webp"/>
            <info id="BBB" bytes="1" type="image/png"/></metadata>"#
            .parse()
            .unwrap();
        let item = |payload| PepItem {
            id: Some("x".into()),
            payload,
        };
        assert_eq!(
            metadata_of(&[item(metadata)]),
            Some(Some("bbb".into())),
            "PNG first"
        );
        let empty: Element = r#"<metadata xmlns="urn:xmpp:avatar:metadata"/>"#.parse().unwrap();
        assert_eq!(metadata_of(&[item(empty)]), Some(None), "taken away");

        let presence: Element = r#"<presence xmlns="jabber:client" from="a@b/c">
            <x xmlns="vcard-temp:x:update"><photo>ABC</photo></x></presence>"#
            .parse()
            .unwrap();
        assert_eq!(
            photo_of(&Presence::try_from(presence).unwrap()),
            Some(Some("abc".into()))
        );
        let plain: Element = r#"<presence xmlns="jabber:client" from="a@b/c"/>"#.parse().unwrap();
        assert_eq!(photo_of(&Presence::try_from(plain).unwrap()), None);

        let vcard: Element = r#"<vCard xmlns="vcard-temp"><PHOTO><BINVAL>
            YWJj
            </BINVAL></PHOTO></vCard>"#
            .parse()
            .unwrap();
        assert_eq!(
            vcard_photo(&vcard),
            Some(b"abc".to_vec()),
            "without TYPE, with line breaks"
        );
    }
}
