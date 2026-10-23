// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! # Resilient stanza stream
//!
//! This module provides the [`StanzaStream`], which is the next level up from
//! the low-level [`XmlStream`][`crate::xmlstream::XmlStream`].
//!
//! The stanza stream knows about XMPP and it most importantly knows how to
//! fix a broken connection with a reconnect and how to do this smoothly using
//! [XEP-0198 (Stream Management)](https://xmpp.org/extensions/xep-0198.html).
//! XEP-0198 is only used if the peer supports it. If the peer does not
//! support XEP-0198, automatic reconnects are still done, but with more
//! undetectable data loss.
//!
//! The main API entrypoint for the stanza stream is, unsurprisingly,
//! [`StanzaStream`].

use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

// TODO: ensure that IDs are always set on stanzas.

// TODO: figure out what to do with the mpsc::Sender<QueueEntry> on lossy
// stream reconnects. Keeping it may cause stanzas to be sent which weren't
// meant for that stream, replacing it is racy.

use futures::{SinkExt, Stream};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::{mpsc, oneshot};
use xmpp_parsers::{jid::Jid, stream_features::StreamFeatures};

use crate::connect::ServerConnector;
use crate::xmlstream::Timeouts;
use crate::Stanza;

mod connected;
mod error;
mod negotiation;
mod queue;
mod stream_management;
#[cfg(test)]
mod tests;
mod worker;

use self::queue::QueueEntry;
pub use self::queue::{StanzaStage, StanzaState, StanzaToken};
pub use self::worker::{Connection, XmppStream};
use self::worker::{StanzaStreamWorker, LOCAL_SHUTDOWN_TIMEOUT};

/// Event informing about the change of the [`StanzaStream`]'s status.
#[derive(Debug)]
pub enum StreamEvent {
    /// The stream was (re-)established **with** loss of state.
    Reset {
        /// The new JID to which the stream is bound.
        bound_jid: Jid,

        /// The features reported by the stream.
        features: StreamFeatures,
    },

    /// The stream is currently inactive because a connection was lost.
    ///
    /// Resumption without loss of state is still possible. This event is
    /// merely informative and may be used to prolong timeouts or inform the
    /// user that the connection is currently unstable.
    Suspended,

    /// The stream was reestablished **without** loss of state.
    ///
    /// This is merely informative. Potentially useful to prolong timeouts.
    Resumed,
}

/// Event emitted by the [`StanzaStream`].
///
/// Note that stream closure is not an explicit event, but the end of the
/// event stream itself.
#[derive(Debug)]
pub enum Event {
    /// The streams connectivity status has changed.
    Stream(StreamEvent),

    /// A stanza was received over the stream.
    Stanza(Stanza),
}

/// Frontend interface to a reliable, always-online stanza stream.
#[derive(Debug)]
pub struct StanzaStream {
    rx: mpsc::Receiver<Event>,
    tx: mpsc::Sender<QueueEntry>,
}

impl StanzaStream {
    /// Establish a new client-to-server stream using the given
    /// [`ServerConnector`].
    ///
    /// `jid` and `password` must be the user account's credentials. `jid` may
    /// either be a bare JID (to let the server choose a resource) or a full
    /// JID (to request a specific resource from the server, with no guarantee
    /// of success).
    ///
    /// `timeouts` controls the responsiveness to connection interruptions
    /// on the underlying transports. Please see the [`Timeouts`] struct's
    /// documentation for hints on how to correctly configure this.
    ///
    /// The `queue_depth` controls the sizes for the incoming and outgoing
    /// stanza queues. If the size is exceeded, the corresponding direction
    /// will block until the queues can be flushed. Note that the respective
    /// reverse direction is not affected (i.e. if your outgoing queue is
    /// full for example because of a slow server, you can still receive
    /// data).
    pub fn new_c2s<C: ServerConnector>(
        server: C,
        jid: Jid,
        password: String,
        timeouts: Timeouts,
        queue_depth: usize,
    ) -> Self {
        let reconnector = Box::new(
            move |_preferred_location: Option<String>, slot: oneshot::Sender<Connection>| {
                let jid = jid.clone();
                let server = server.clone();
                let password = password.clone();
                tokio::spawn(async move {
                    const MAX_DELAY: Duration = Duration::new(30, 0);
                    let mut delay = Duration::new(1, 0);
                    loop {
                        log::debug!("Starting new connection as {}", jid);
                        match crate::client::login::client_auth(
                            server.clone(),
                            jid.clone(),
                            password.clone(),
                            timeouts,
                        )
                        .await
                        {
                            Ok((features, stream)) => {
                                log::debug!("Connection as {} established", jid);
                                let stream = stream.box_stream();
                                let Err(mut conn) = slot.send(Connection {
                                    stream,
                                    features,
                                    identity: jid,
                                }) else {
                                    // Send succeeded, we're done here.
                                    return;
                                };

                                log::debug!("StanzaStream dropped, attempting graceful termination of fresh stream.");
                                // Send failed, i.e. the stanzastream is dead. Let's
                                // be polite and close this stream cleanly.
                                // We don't care whether that works, though, we
                                // just want to release the resources after a
                                // defined amount of time.
                                let _: Result<_, _> = tokio::time::timeout(
                                    LOCAL_SHUTDOWN_TIMEOUT,
                                    <XmppStream as SinkExt<&Stanza>>::close(&mut conn.stream),
                                )
                                .await;
                                return;
                            }
                            Err(e) => {
                                // TODO: auth errors should probably be fatal??
                                log::error!("Failed to connect: {}. Retrying in {:?}.", e, delay);
                                tokio::time::sleep(delay).await;
                                delay *= 2;
                                if delay > MAX_DELAY {
                                    delay = MAX_DELAY;
                                }
                            }
                        }
                    }
                });
            },
        );
        Self::new(reconnector, queue_depth)
    }

    /// Create a new stanza stream.
    ///
    /// Stanza streams operate using a `connector` which is responsible for
    /// producing a new stream whenever necessary. It is the connector's
    /// responsibility that:
    ///
    /// - It never fails to send to the channel it is given. If the connector
    ///   drops the channel, the `StanzaStream` will consider this fatal and
    ///   fail the stream.
    ///
    /// - All streams are authenticated and secured as necessary.
    ///
    /// - All streams are authenticated for the same entity. If the connector
    ///   were to provide streams for different identities, information leaks
    ///   could occur as queues from previous sessions are being flushed on
    ///   the new stream on a reconnect.
    ///
    /// Most notably, the `connector` is **not** responsible for performing
    /// resource binding: Resource binding is handled by the `StanzaStream`.
    ///
    /// `connector` will be called soon after `new()` was called to establish
    /// the first underlying stream for the `StanzaStream`.
    ///
    /// The `queue_depth` controls the sizes for the incoming and outgoing
    /// stanza queues. If the size is exceeded, the corresponding direction
    /// will block until the queues can be flushed. Note that the respective
    /// reverse direction is not affected (i.e. if your outgoing queue is
    /// full for example because of a slow server, you can still receive
    /// data).
    pub fn new(
        connector: Box<dyn FnMut(Option<String>, oneshot::Sender<Connection>) + Send + 'static>,
        queue_depth: usize,
    ) -> Self {
        // c2f = core to frontend, f2c = frontend to core
        let (f2c_tx, c2f_rx) = StanzaStreamWorker::spawn(connector, queue_depth);
        Self {
            tx: f2c_tx,
            rx: c2f_rx,
        }
    }

    async fn assert_send(&self, cmd: QueueEntry) {
        match self.tx.send(cmd).await {
            Ok(()) => (),
            Err(_) => panic!("Stream closed or the stream's background workers have crashed."),
        }
    }

    /// Close the stream.
    ///
    /// This will initiate a clean shutdown of the stream and will prevent and
    /// cancel any more reconnection attempts.
    pub async fn close(mut self) {
        drop(self.tx); // closes stream.
        while let Some(ev) = self.rx.recv().await {
            log::trace!("discarding event {:?} after stream closure", ev);
        }
    }

    /// Send a stanza via the stream.
    ///
    /// Note that completion of this function merely signals that the stanza
    /// has been enqueued successfully: it may be stuck in the transmission
    /// queue for quite a while if the stream is currently disconnected. The
    /// transmission progress can be observed via the returned
    /// [`StanzaToken`].
    ///
    /// # Panics
    ///
    /// If the stream has failed catastrophically (i.e. due to a software
    /// bug), this function may panic.
    pub async fn send(&self, stanza: Box<Stanza>) -> StanzaToken {
        let (queue_entry, token) = QueueEntry::tracked(stanza);
        self.assert_send(queue_entry).await;
        token
    }

    /// Split the stream into the [`StanzaSender`] and [`StanzaReceiver`].
    pub fn split(self) -> (StanzaSender, StanzaReceiver) {
        let stream = Arc::new(Mutex::new(self));

        let tx = StanzaSender(stream.clone());
        let rx = StanzaReceiver(stream);

        (tx, rx)
    }

    /// Reunite the [`StanzaSender`] and [`StanzaReceiver`] back into a single stream.
    ///
    /// # Panics
    ///
    /// This function panics if the `Sender` and `Receiver` don't come from
    /// the same [`Stream`].
    pub fn reunite(tx: StanzaSender, rx: StanzaReceiver) -> Self {
        assert!(
            Arc::ptr_eq(&tx.0, &rx.0),
            "Unrelated Sender and Receiver passed to reunite."
        );

        drop(tx);

        let inner = Arc::try_unwrap(rx.0).expect("Failed to unwrap Receiver Arc");
        inner.into_inner()
    }
}

impl Stream for StanzaStream {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// Send half of the [`StanzaStream`]
#[derive(Debug)]
pub struct StanzaSender(pub(super) Arc<Mutex<StanzaStream>>);

impl StanzaSender {
    /// Send a stanza via the stream.
    ///
    /// See the documentation of [`StanzaStream::send()`].
    pub async fn send(&self, stanza: Box<Stanza>) -> StanzaToken {
        self.0.lock().await.send(stanza).await
    }
}

/// Receive half of the [`StanzaStream`]
#[derive(Debug)]
pub struct StanzaReceiver(pub(super) Arc<Mutex<StanzaStream>>);

impl Stream for StanzaReceiver {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let Ok(mut stream) = self.0.try_lock() else {
            // omtalk: the sender holds the lock for a moment. Pending without a wakeup
            // left this half asleep with stanzas waiting (an IQ answer unseen until it
            // timed out); ask to be polled again instead.
            cx.waker().wake_by_ref();
            return Poll::Pending;
        };

        stream.rx.poll_recv(cx)
    }
}
