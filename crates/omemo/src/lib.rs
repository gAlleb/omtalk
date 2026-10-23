//! OMEMO 0.3 (`eu.siacs.conversations.axolotl`) — the version Gajim, Conversations,
//! Dino and Monal interoperate on.
//!
//! The crate covers the cryptography and the wire format of the key material: X3DH,
//! the Double Ratchet, the libsignal-compatible message encoding, and the AES-GCM
//! payload. XML and XMPP live elsewhere. Every byte layout here is checked against
//! python-omemo (`tests/oracle`), which is what "compatible" means for this crate.

mod crypto;
mod error;
pub mod keys;
pub mod payload;
mod proto;
mod ratchet;
pub mod session;

pub use error::Error;
pub use keys::{Bundle, DeviceId, IdentityKeyPair, OwnKeys};
pub use payload::{EncryptedPayload, decrypt_payload, encrypt_payload};
pub use session::{EncryptedKey, Received, Session, receive, receive_from};

/// XML namespace of this OMEMO version.
pub const NAMESPACE: &str = "eu.siacs.conversations.axolotl";
