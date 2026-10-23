//! What omtalk tells others about itself (XEP-0030, XEP-0115), and the IQ requests a
//! client must answer.

use std::collections::BTreeSet;

use xmpp_parsers::caps::{Caps, compute_disco, hash_caps};
use xmpp_parsers::disco::{DiscoInfoQuery, DiscoInfoResult, Identity};
use xmpp_parsers::hashes::Algo;
use xmpp_parsers::iq::{Iq, IqPayload};
use xmpp_parsers::ns;
use xmpp_parsers::presence::Presence;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const CAPS_NODE: &str = "https://github.com/gAlleb/omtalk";

/// Features this client implements. A `+notify` feature asks the server to push
/// changes of that PEP node: without the device list one, OMEMO would miss new devices.
const FEATURES: &[&str] = &[
    ns::DISCO_INFO,
    ns::CAPS,
    ns::PING,
    ns::LEGACY_OMEMO_DEVICELIST,
    "eu.siacs.conversations.axolotl.devicelist+notify",
    // Receipts and read markers, also shared between one's own clients (XEP-0490).
    ns::RECEIPTS,
    ns::DISPLAYED_MARKERS,
    "urn:xmpp:mds:displayed:0+notify",
    ns::CHATSTATES,
    // Direct invitations to rooms (XEP-0249) and replies (XEP-0461) are understood.
    "jabber:x:conference",
    "urn:xmpp:reply:0",
    "urn:xmpp:message-correct:0",
    "urn:xmpp:reactions:0",
    // Message styling: *strong*, _emphasis_ and the like are shown (XEP-0393).
    "urn:xmpp:styling:0",
    // Avatars: a change is pushed (XEP-0084).
    "urn:xmpp:avatar:metadata+notify",
    // The names people give themselves (XEP-0172).
    "http://jabber.org/protocol/nick+notify",
];

pub fn info() -> DiscoInfoResult {
    DiscoInfoResult {
        node: None,
        identities: vec![Identity::new("client", "console", "en", "omtalk")],
        features: FEATURES.iter().map(|f| f.to_string()).collect::<BTreeSet<_>>(),
        extensions: Vec::new(),
    }
}

fn caps() -> Caps {
    let hash = hash_caps(&compute_disco(&info()), Algo::Sha_1).expect("SHA-1 is supported");
    Caps::new(CAPS_NODE, hash)
}

/// The initial presence, carrying the caps that tell the server which notifications we want.
pub fn presence() -> Presence {
    Presence::available().with_payload(caps())
}

/// The answer to an incoming IQ, or `None` if the IQ is itself an answer.
pub fn answer(iq: Iq) -> Option<Iq> {
    let from = iq.from().cloned();
    let id = iq.id().to_owned();
    let unsupported = || {
        Iq::from_error(
            id.clone(),
            StanzaError::new(ErrorType::Cancel, DefinedCondition::ServiceUnavailable, "en", ""),
        )
    };
    let reply = match iq.into_payload() {
        IqPayload::Get(payload) if payload.is("query", ns::DISCO_INFO) => {
            match DiscoInfoQuery::try_from(payload) {
                Ok(query) => {
                    let mut info = info();
                    // Asked by caps node: answer for that node, as XEP-0115 requires.
                    info.node = query.node;
                    Iq::from_result(id, Some(info))
                }
                Err(_) => unsupported(),
            }
        }
        IqPayload::Get(payload) if payload.is("ping", ns::PING) => {
            Iq::from_result(id, None::<DiscoInfoResult>)
        }
        IqPayload::Get(_) | IqPayload::Set(_) => unsupported(),
        IqPayload::Result(_) | IqPayload::Error(_) => return None,
    };
    Some(match from {
        Some(from) => reply.with_to(from),
        None => reply,
    })
}
