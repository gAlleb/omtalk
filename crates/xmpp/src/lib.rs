//! The XMPP side of omtalk: connections for any number of accounts, the replies a
//! client owes the server, and PEP.
//!
//! Each account runs in its own task, which owns the connection; [`Account`] is a cheap
//! handle to it. Events of all accounts arrive on one channel, tagged with the account.

mod account;
mod accounts;
pub mod avatar;
pub mod bookmarks;
pub mod chatstates;
mod config;
mod disco;
mod error;
mod event;
mod login;
pub mod mam;
pub mod muc;
pub mod pep;
pub mod presence;
pub mod receipts;
pub mod replies;
pub mod roster;
pub mod services;
pub mod upload;

pub use pep::Access;
pub use roster::Contact;

pub use account::Account;
pub use accounts::Accounts;
pub use config::{AccountConfig, Server, device_resource};
pub use error::Error;
pub use event::{AccountEvent, Event, PepItem};
pub use xmpp_parsers::jid::{self, BareJid, FullJid, Jid};
pub use xmpp_parsers::minidom::Element;
pub use xmpp_parsers::muc::user::{Affiliation, Role};
pub use xmpp_parsers::stanza_error;
