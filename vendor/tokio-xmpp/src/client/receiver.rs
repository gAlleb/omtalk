// Copyright (c) 2025 xmpp-rs contributors.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::Client;
use crate::Event;
use core::{pin::Pin, task::Context};
use futures::StreamExt;
use futures::{task::Poll, Stream};
use std::sync::Arc;
use tokio::sync::Mutex;
use xmpp_parsers::{jid::FullJid, stream_features::StreamFeatures};

/// Read half of a [`Client`](crate::Client).
#[derive(Debug)]
pub struct ClientReceiver(pub(super) Arc<Mutex<Client>>);

impl ClientReceiver {
    /// Return the bound JID.
    ///
    /// See the documentation of [`Client::bound_jid`](crate::Client::bound_jid) for more
    /// information.
    pub async fn bound_jid(&self) -> Option<FullJid> {
        self.0.lock().await.bound_jid.clone()
    }

    /// Return the received stream features.
    ///
    /// See the documentation of [`Client::get_stream_features`](crate::Client::get_stream_features)
    /// for more information.
    pub async fn get_stream_features(&self) -> Option<StreamFeatures> {
        self.0.lock().await.features.clone()
    }
}

impl Stream for ClientReceiver {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let Ok(mut client) = self.0.try_lock() else {
            // omtalk: as in `StanzaReceiver`, Pending must not lose the wakeup.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        };

        client.poll_next_unpin(cx)
    }
}
