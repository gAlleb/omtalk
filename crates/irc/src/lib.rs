//! Native IRC/soju sessions. IRC parsing/encoding belongs to irc-proto; registration,
//! negotiated capabilities, bouncer binding and history belong to this adapter.
mod config;
mod manager;
mod session;
mod state;
mod support;
mod wire;

pub use config::{Config, Mode, Sasl};
pub use manager::Client;
pub use session::{Command, Handle};
pub use state::{Event, NetworkInfo, Participant, Snapshot};
pub use state::{HistoryKind, HistoryRequest};
pub use support::Support;
pub use wire::{is_sensitive, split_message};

#[cfg(test)]
mod tests;
