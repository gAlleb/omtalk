// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xmpp_parsers::{jid::Jid, message::Id, stanza::Stanza, stream_features::StreamFeatures};

use crate::xmlstream::XmppStreamElement;
use crate::Error;

pub(crate) fn make_id() -> String {
    let id: u64 = rand::random();
    format!("{}", id)
}

/// Assign a random ID to the stanza, if no ID has been assigned yet.
pub fn ensure_stanza_id(stanza: &mut Stanza) -> &str {
    match stanza {
        Stanza::Iq(iq) => {
            let id = iq.id_mut();
            if id.is_empty() {
                *id = make_id();
            }
            id
        }
        Stanza::Message(message) => message.id.get_or_insert_with(|| Id(make_id())).0.as_ref(),
        Stanza::Presence(presence) => presence.id.get_or_insert_with(make_id),
    }
}

impl From<Stanza> for XmppStreamElement {
    fn from(other: Stanza) -> Self {
        Self::Stanza(other)
    }
}

/// High-level event on the Stream implemented by Client and Component
#[derive(Debug)]
pub enum Event {
    /// Stream is connected and initialized
    Online {
        /// Server-set Jabber-Id for your session
        ///
        /// This may turn out to be a different JID resource than
        /// expected, so use this one instead of the JID with which
        /// the connection was setup.
        bound_jid: Jid,
        /// Features supported by the server
        features: StreamFeatures,
        /// Was this session resumed?
        ///
        /// Not yet implemented for the Client
        resumed: bool,
    },
    /// Stream end
    Disconnected(Error),
    /// Received stanza/nonza
    Stanza(Stanza),
}

impl Event {
    /// `Online` event?
    pub fn is_online(&self) -> bool {
        matches!(&self, Event::Online { .. })
    }

    /// Get the server-assigned JID for the `Online` event
    pub fn get_jid(&self) -> Option<&Jid> {
        match *self {
            Event::Online { ref bound_jid, .. } => Some(bound_jid),
            _ => None,
        }
    }

    /// If this is a `Stanza` event, get its data
    pub fn as_stanza(&self) -> Option<&Stanza> {
        match *self {
            Event::Stanza(ref stanza) => Some(stanza),
            _ => None,
        }
    }

    /// If this is a `Stanza` event, unwrap into its data
    pub fn into_stanza(self) -> Option<Stanza> {
        match self {
            Event::Stanza(stanza) => Some(stanza),
            _ => None,
        }
    }
}
