//! PEP (XEP-0163): publishing to the account's own nodes and reading others'.

use xmpp_parsers::data_forms::{DataForm, DataFormType, Field};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::ns;
use xmpp_parsers::pubsub::event::{Event as PubSubEvent, Payload};
use xmpp_parsers::pubsub::owner::{Owner, Payload as OwnerPayload};
use xmpp_parsers::pubsub::pubsub::{Item, Items, PubSub, Publish, PublishOptions};
use xmpp_parsers::pubsub::{ItemId, NodeName};
use xmpp_parsers::stanza_error::{DefinedCondition, StanzaError};

use crate::{Error, PepItem};

/// FORM_TYPE of publish options (XEP-0060, 7.1.5).
const PUBLISH_OPTIONS_FORM: &str = "http://jabber.org/protocol/pubsub#publish-options";

/// Who may read a node. OMEMO device lists and bundles must be `Open`: a contact
/// without presence subscription, a group member, has to fetch them too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Open,
    Presence,
    /// The account itself only: bookmarks and other private data.
    Owner,
}

impl Access {
    fn model(self) -> &'static str {
        match self {
            Access::Open => "open",
            Access::Presence => "presence",
            Access::Owner => "whitelist",
        }
    }
}

/// Nodes of one item per thing (a bookmark per room, a read mark per chat): a node
/// keeps one item only unless told otherwise, and each new one would drop the others.
const MANY_ITEMS: [&str; 2] = [crate::bookmarks::NODE, crate::receipts::MDS];

/// How the node is to be: who reads it, and for one of many items, that all are kept
/// (XEP-0402, 5; XEP-0490).
fn node_options(node: &str, access: Access) -> Vec<Field> {
    let mut fields = vec![Field::text_single("pubsub#access_model", access.model())];
    if MANY_ITEMS.contains(&node) {
        fields.push(Field::text_single("pubsub#persist_items", "true"));
        fields.push(Field::text_single("pubsub#max_items", "max"));
    }
    if node == crate::receipts::MDS {
        fields.push(Field::text_single("pubsub#send_last_published_item", "never"));
    }
    fields
}

pub(crate) fn publish_request(node: &str, id: Option<&str>, payload: Element, access: Access) -> PubSub {
    PubSub::Publish {
        publish: Publish {
            node: NodeName(node.to_owned()),
            items: vec![Item {
                id: id.map(|id| ItemId(id.to_owned())),
                publisher: None,
                payload: Some(payload),
            }],
        },
        publish_options: Some(PublishOptions {
            form: Some(DataForm::new(
                DataFormType::Submit,
                PUBLISH_OPTIONS_FORM,
                node_options(node, access),
            )),
        }),
    }
}

/// Whether a publish failed because the node exists with other settings than the
/// publish options asked for (XEP-0060, 7.1.5): another client created it.
pub(crate) fn options_conflict(error: &StanzaError) -> bool {
    error.defined_condition == DefinedCondition::Conflict
        && error
            .other
            .as_ref()
            .is_some_and(|e| e.is("precondition-not-met", ns::PUBSUB_ERRORS))
}

/// Sets the node as it is to be, as its owner: one made with other settings (by another
/// client, or by omtalk before it kept all bookmarks) is put right.
pub(crate) fn configure_request(node: &str, access: Access) -> Owner {
    Owner {
        payload: OwnerPayload::Configure {
            node: Some(NodeName(node.to_owned())),
            form: Some(DataForm::new(
                DataFormType::Submit,
                ns::PUBSUB_CONFIGURE,
                node_options(node, access),
            )),
        },
    }
}

pub(crate) fn items_request(node: &str) -> PubSub {
    PubSub::Items(Items {
        max_items: None,
        node: NodeName(node.to_owned()),
        subid: None,
        items: Vec::new(),
    })
}

pub(crate) fn parse_items(answer: Option<Element>) -> Result<Vec<PepItem>, Error> {
    let answer = answer.ok_or_else(|| Error::Unexpected("empty answer to an items request".into()))?;
    match PubSub::try_from(answer) {
        Ok(PubSub::Items(items)) => Ok(items
            .items
            .into_iter()
            .filter_map(|item| {
                Some(PepItem {
                    id: item.id.map(|id| id.0),
                    payload: item.payload?,
                })
            })
            .collect()),
        Ok(other) => Err(Error::Unexpected(format!("{other:?}"))),
        Err(error) => Err(Error::Unexpected(error.to_string())),
    }
}

/// Published and retracted items of a PEP notification, if `payload` is one.
pub(crate) fn parse_event(payload: &Element) -> Option<(String, Vec<PepItem>, Vec<String>)> {
    match PubSubEvent::try_from(payload.clone()).ok()?.payload {
        Payload::Items {
            node,
            published,
            retracted,
        } => Some((
            node.0,
            published
                .into_iter()
                .filter_map(|item| {
                    Some(PepItem {
                        id: item.id.map(|id| id.0),
                        payload: item.payload?,
                    })
                })
                .collect(),
            retracted.into_iter().map(|id| id.0).collect(),
        )),
        _ => None,
    }
}
