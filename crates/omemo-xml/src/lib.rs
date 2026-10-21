//! OMEMO 0.3 on the wire: device lists, bundles and `<encrypted>` elements, between the
//! xmpp-parsers types and the `omemo` crate.
//!
//! Parsing here also checks what the XML schema cannot: key formats and lengths. What a
//! client does with the result — which devices to encrypt for, whom to trust — is not
//! decided here.

pub mod bundle;
pub mod device_list;
mod error;
pub mod message;

pub use error::Error;
pub use xmpp_parsers::legacy_omemo as xml;
pub use xmpp_parsers::minidom::Element;

/// PEP node holding an account's device list.
pub const DEVICE_LIST_NODE: &str = xmpp_parsers::ns::LEGACY_OMEMO_DEVICELIST;

/// PEP node holding one device's bundle.
pub fn bundle_node(device_id: omemo::DeviceId) -> String {
    format!("{}:{device_id}", xmpp_parsers::ns::LEGACY_OMEMO_BUNDLES)
}
