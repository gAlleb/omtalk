//! The XMPP side of omtalk: connections for any number of accounts, the replies a
//! client owes the server, and PEP.
//!
//! Each account runs in its own task, which owns the connection; [`Account`] is a cheap
//! handle to it. Events of all accounts arrive on one channel, tagged with the account.

mod account;
mod accounts;
mod config;
mod disco;
mod error;
mod event;
pub mod pep;

pub use pep::Access;

pub use account::Account;
pub use accounts::Accounts;
pub use config::{AccountConfig, Server};
pub use error::Error;
pub use event::{AccountEvent, Event, PepItem};
pub use xmpp_parsers::jid::{BareJid, FullJid, Jid};
pub use xmpp_parsers::minidom::Element;
