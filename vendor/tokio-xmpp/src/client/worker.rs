// Copyright (c) 2025 xmpp-rs contributors.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::client::iq;
use crate::stanzastream::StanzaReceiver;
use crate::stanzastream::{Event as StanzaStreamEvent, StreamEvent};
use crate::{Event, Stanza};
use core::ops::ControlFlow;
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use xmpp_parsers::jid::FullJid;
use xmpp_parsers::stream_features::StreamFeatures;

/// Worker to drive the [`crate::stanzastream`] of a client in the background and continue to
/// acknowledge IQs, even when the client is not polled.
pub struct ClientWorker {
    // Receiver from the StanzaStream
    stream_rx: StanzaReceiver,
    // Sender to the client (worker-to-frontend)
    stanza_w2f_tx: mpsc::Sender<Event>,
    // Shutdown signal receiver from frontend
    shutdown_rx: oneshot::Receiver<()>,
    // JID of the logged-in client
    bound_jid: Option<FullJid>,
    // Stream features of the currently connected stream
    features: Option<StreamFeatures>,
    // Response tracker for IQs
    iq_response_tracker: iq::IqResponseTracker,
}

impl ClientWorker {
    pub fn new(
        stream_rx: StanzaReceiver,
        iq_response_tracker: iq::IqResponseTracker,
        depth: usize,
    ) -> (Self, oneshot::Sender<()>, mpsc::Receiver<Event>) {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // worker-to-frontend connection
        let (stanza_w2f_tx, stanza_w2f_rx) = mpsc::channel(depth);

        let worker = Self {
            stream_rx,
            stanza_w2f_tx,
            iq_response_tracker,
            shutdown_rx,
            bound_jid: None,
            features: None,
        };

        (worker, shutdown_tx, stanza_w2f_rx)
    }

    pub async fn run(mut self) -> StanzaReceiver {
        loop {
            tokio::select! {
                _ = &mut self.shutdown_rx => {
                    return self.stream_rx;
                }
                Some(event) = self.stream_rx.next() => {
                    self.handle_event(event).await;
                }
            }
        }
    }

    async fn handle_event(&mut self, event: StanzaStreamEvent) {
        let send_event = match event {
            StanzaStreamEvent::Stanza(st) => match st {
                Stanza::Iq(iq) => match self.iq_response_tracker.handle_iq(iq) {
                    ControlFlow::Break(()) => return,
                    ControlFlow::Continue(iq) => Event::Stanza(Stanza::Iq(iq)),
                },
                other => Event::Stanza(other),
            },
            StanzaStreamEvent::Stream(StreamEvent::Reset {
                bound_jid,
                features,
            }) => {
                // This unwrap() will never fail, because the server always uses our own bound JID.
                self.bound_jid = Some(bound_jid.try_as_full().unwrap().clone());
                self.features = Some(features.clone());
                self.iq_response_tracker
                    .set_account_jid(bound_jid.to_bare());

                Event::Online {
                    bound_jid,
                    features,
                    resumed: false,
                }
            }
            StanzaStreamEvent::Stream(StreamEvent::Resumed) => Event::Online {
                bound_jid: self.bound_jid.as_ref().unwrap().clone().into(),
                features: self.features.as_ref().unwrap().clone(),
                resumed: true,
            },
            StanzaStreamEvent::Stream(StreamEvent::Suspended) => return,
        };

        let Ok(()) = self.stanza_w2f_tx.send(send_event).await else {
            panic!("All clients have been dropped.");
        };
    }
}
