//! OMEMO for one XMPP account: keeps the own device announced, follows device lists,
//! and encrypts and decrypts chat messages for every device of both sides.
//!
//! An account is often open in several clients at once (a phone with Monal, two laptops
//! with Gajim). Each is a device with its own keys; a message is encrypted for all
//! devices of the peer and all other devices of the sender, and each device keeps its
//! own sessions.

mod client;
mod envelope;
mod error;
mod persist;
mod state;

pub use client::{CatchUp, DeviceInfo, Incoming, Omemo, SendReport, fingerprint};
pub use envelope::Direction;
pub use error::Error;
pub use persist::{Ephemeral, Persist, PersistError};
pub use state::State;
