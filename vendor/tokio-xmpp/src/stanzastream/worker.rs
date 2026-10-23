// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;
use std::io;

use futures::{ready, SinkExt, StreamExt};

use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use xmpp_parsers::{
    iq,
    jid::Jid,
    ping,
    stream_error::{DefinedCondition, StreamError},
    stream_features::StreamFeatures,
};

use crate::connect::AsyncReadAndWrite;
use crate::xmlstream::{FallibleStreamElement, ReadError};
use crate::Stanza;

use super::connected::{ConnectedEvent, ConnectedState};
use super::negotiation::NegotiationState;
use super::queue::{QueueEntry, TransmitQueue};
use super::stream_management::SmState;
use super::{Event, StreamEvent};

/// Convenience alias for [`XmlStreams`][`crate::xmlstream::XmlStream`] which
/// may be used with [`StanzaStream`][`super::StanzaStream`].
pub type XmppStream =
    crate::xmlstream::XmlStream<Box<dyn AsyncReadAndWrite + Send + 'static>, FallibleStreamElement>;

/// Underlying connection for a [`StanzaStream`][`super::StanzaStream`].
pub struct Connection {
    /// The stream to use to send and receive XMPP data.
    pub stream: XmppStream,

    /// The stream features offered by the peer.
    pub features: StreamFeatures,

    /// The identity to which this stream belongs.
    ///
    /// Note that connectors must not return bound streams. However, the Jid
    /// may still be a full jid in order to request a specific resource at
    /// bind time. If `identity` is a bare JID, the peer will assign the
    /// resource.
    pub identity: Jid,
}

// Allow for up to 10s for local shutdown.
// TODO: make this configurable maybe?
pub(super) static LOCAL_SHUTDOWN_TIMEOUT: Duration = Duration::new(10, 0);
pub(super) static REMOTE_SHUTDOWN_TIMEOUT: Duration = Duration::new(5, 0);
pub(super) static PING_PROBE_ID_PREFIX: &str = "xmpp-rs-stanzastream-liveness-probe";

pub(super) enum Never {}

pub(super) enum WorkerEvent {
    /// The stream was reset and can now be used for rx/tx.
    Reset {
        bound_jid: Jid,
        features: StreamFeatures,
    },

    /// The stream has been resumed successfully.
    Resumed,

    /// Data received successfully.
    Stanza(Stanza),

    /// Failed to parse pieces from the stream.
    ParseError(xso::error::Error),

    /// Soft timeout noted by the underlying XmppStream.
    SoftTimeout,

    /// Stream disconnected.
    Disconnected {
        /// Slot for a new connection.
        slot: oneshot::Sender<Connection>,

        /// Set to None if the stream was cleanly closed by the remote side.
        error: Option<io::Error>,
    },

    /// The reconnection backend dropped the connection channel.
    ReconnectAborted,
}

enum WorkerStream {
    /// Pending connection.
    Connecting {
        /// Optional contents of an [`WorkerEvent::Disconnect`] to emit.
        notify: Option<(oneshot::Sender<Connection>, Option<io::Error>)>,

        /// Receiver slot for the next connection.
        slot: oneshot::Receiver<Connection>,

        /// Stream management state from a previous connection.
        sm_state: Option<SmState>,
    },

    /// Connection available.
    Connected {
        stream: XmppStream,
        substate: ConnectedState,
        features: StreamFeatures,
        identity: Jid,
    },

    /// Disconnected permanently by local choice.
    Terminated,
}

impl WorkerStream {
    fn disconnect(&mut self, sm_state: Option<SmState>, error: Option<io::Error>) -> WorkerEvent {
        let (tx, rx) = oneshot::channel();
        *self = Self::Connecting {
            notify: None,
            slot: rx,
            sm_state,
        };
        WorkerEvent::Disconnected { slot: tx, error }
    }

    fn poll_duplex(
        self: Pin<&mut Self>,
        transmit_queue: &mut TransmitQueue<QueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<WorkerEvent>> {
        let this = self.get_mut();
        loop {
            match this {
                // Disconnected cleanly (terminal state), signal end of
                // stream.
                Self::Terminated => return Poll::Ready(None),

                // In the progress of reconnecting, wait for reconnection to
                // complete and then switch states.
                Self::Connecting {
                    notify,
                    slot,
                    sm_state,
                } => {
                    if let Some((slot, error)) = notify.take() {
                        return Poll::Ready(Some(WorkerEvent::Disconnected { slot, error }));
                    }

                    match ready!(Pin::new(slot).poll(cx)) {
                        Ok(Connection {
                            stream,
                            features,
                            identity,
                        }) => {
                            let substate = ConnectedState::Negotiating {
                                // We panic here, but that is ok-ish, because
                                // that will "only" crash the worker and thus
                                // the stream, and that is kind of exactly
                                // what we want.
                                substate: NegotiationState::new(&features, sm_state.take())
                                    .expect("Non-negotiable stream"),
                            };
                            *this = Self::Connected {
                                substate,
                                stream,
                                features,
                                identity,
                            };
                        }
                        Err(_) => {
                            // The sender was dropped. This is fatal.
                            *this = Self::Terminated;
                            return Poll::Ready(Some(WorkerEvent::ReconnectAborted));
                        }
                    }
                }

                Self::Connected {
                    stream,
                    identity,
                    substate,
                    features,
                } => {
                    match ready!(substate.poll(
                        Pin::new(stream),
                        identity,
                        features,
                        transmit_queue,
                        cx
                    )) {
                        // continue looping if the substate did not produce a result.
                        None => (),

                        // produced an event to emit.
                        Some(ConnectedEvent::Worker(v)) => {
                            // Capture the JID from a stream reset to update our state.
                            if let WorkerEvent::Reset { ref bound_jid, .. } = v {
                                *identity = bound_jid.clone();
                            }
                            return Poll::Ready(Some(v));
                        }

                        // stream broke or closed somehow.
                        Some(ConnectedEvent::Disconnect { sm_state, error }) => {
                            return Poll::Ready(Some(this.disconnect(sm_state, error)));
                        }

                        Some(ConnectedEvent::RemoteShutdown { sm_state }) => {
                            let error = io::Error::new(
                                io::ErrorKind::ConnectionAborted,
                                "peer closed the XML stream",
                            );
                            let (tx, rx) = oneshot::channel();
                            let mut new_state = Self::Connecting {
                                notify: None,
                                slot: rx,
                                sm_state,
                            };
                            core::mem::swap(this, &mut new_state);
                            match new_state {
                                Self::Connected { stream, .. } => {
                                    tokio::spawn(shutdown_stream_by_remote_choice(
                                        stream,
                                        REMOTE_SHUTDOWN_TIMEOUT,
                                    ));
                                }
                                _ => unreachable!(),
                            }

                            return Poll::Ready(Some(WorkerEvent::Disconnected {
                                slot: tx,
                                error: Some(error),
                            }));
                        }

                        Some(ConnectedEvent::LocalShutdownRequested) => {
                            // We don't switch to "terminated" here, but we
                            // return "end of stream" nonetheless.
                            return Poll::Ready(None);
                        }
                    }
                }
            }
        }
    }

    /// Poll the stream write-only.
    ///
    /// This never completes, not even if the `transmit_queue` is empty and
    /// its sender has been dropped, unless a write error occurs.
    ///
    /// The use case behind this is to run his in parallel to a blocking
    /// operation which should only block the receive side, but not the
    /// transmit side of the stream.
    ///
    /// Calling this and `poll_duplex` from different tasks in parallel will
    /// cause havoc.
    ///
    /// Any errors are reported on the next call to `poll_duplex`.
    fn poll_writes(
        &mut self,
        transmit_queue: &mut TransmitQueue<QueueEntry>,
        cx: &mut Context,
    ) -> Poll<Never> {
        match self {
            Self::Terminated | Self::Connecting { .. } => Poll::Pending,
            Self::Connected {
                substate, stream, ..
            } => {
                ready!(substate.poll_writes(Pin::new(stream), transmit_queue, cx));
                Poll::Pending
            }
        }
    }

    fn start_send_stream_error(&mut self, error: StreamError) {
        match self {
            // If we are not connected or still connecting, we feign success
            // and enter the Terminated state.
            Self::Terminated | Self::Connecting { .. } => {
                *self = Self::Terminated;
            }

            Self::Connected { substate, .. } => substate.start_send_stream_error(error),
        }
    }

    fn poll_close(&mut self, cx: &mut Context) -> Poll<io::Result<()>> {
        match self {
            Self::Terminated => Poll::Ready(Ok(())),
            Self::Connecting { .. } => {
                *self = Self::Terminated;
                Poll::Ready(Ok(()))
            }
            Self::Connected {
                substate, stream, ..
            } => {
                let result = ready!(substate.poll_close(Pin::new(stream), cx));
                *self = Self::Terminated;
                Poll::Ready(result)
            }
        }
    }

    fn drive_duplex<'a>(
        &'a mut self,
        transmit_queue: &'a mut TransmitQueue<QueueEntry>,
    ) -> DriveDuplex<'a> {
        DriveDuplex {
            stream: Pin::new(self),
            queue: transmit_queue,
        }
    }

    fn drive_writes<'a>(
        &'a mut self,
        transmit_queue: &'a mut TransmitQueue<QueueEntry>,
    ) -> DriveWrites<'a> {
        DriveWrites {
            stream: Pin::new(self),
            queue: transmit_queue,
        }
    }

    fn close(&mut self) -> Close<'_> {
        Close {
            stream: Pin::new(self),
        }
    }

    /// Enqueue a `<sm:r/>`, if stream management is enabled.
    ///
    /// Multiple calls to `send_sm_request` may cause only a single `<sm:r/>`
    /// to be sent.
    ///
    /// Returns true if stream management is enabled and a request could be
    /// queued or deduplicated with a previous request.
    fn queue_sm_request(&mut self) -> bool {
        match self {
            Self::Terminated | Self::Connecting { .. } => false,
            Self::Connected { substate, .. } => substate.queue_sm_request(),
        }
    }
}

struct DriveDuplex<'x> {
    stream: Pin<&'x mut WorkerStream>,
    queue: &'x mut TransmitQueue<QueueEntry>,
}

impl Future for DriveDuplex<'_> {
    type Output = Option<WorkerEvent>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.stream.as_mut().poll_duplex(this.queue, cx)
    }
}

struct DriveWrites<'x> {
    stream: Pin<&'x mut WorkerStream>,
    queue: &'x mut TransmitQueue<QueueEntry>,
}

impl Future for DriveWrites<'_> {
    type Output = Never;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.stream.as_mut().poll_writes(this.queue, cx)
    }
}

struct Close<'x> {
    stream: Pin<&'x mut WorkerStream>,
}

impl Future for Close<'_> {
    type Output = io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.stream.as_mut().poll_close(cx)
    }
}

pub(super) fn parse_error_to_stream_error(e: xso::error::Error) -> StreamError {
    use xso::error::Error;
    let condition = match e {
        Error::XmlError(_) => DefinedCondition::NotWellFormed,
        Error::TextParseError(_) | Error::Other(_) => DefinedCondition::InvalidXml,
        Error::TypeMismatch => DefinedCondition::UnsupportedStanzaType,
    };
    StreamError::new(condition, "en", e.to_string())
}

/// Worker system for a [`StanzaStream`].
pub(super) struct StanzaStreamWorker {
    reconnector: Box<dyn FnMut(Option<String>, oneshot::Sender<Connection>) + Send + 'static>,
    frontend_tx: mpsc::Sender<Event>,
    stream: WorkerStream,
    transmit_queue: TransmitQueue<QueueEntry>,
}

macro_rules! send_or_break {
    ($value:expr => $permit:ident in $ch:expr, $txq:expr => $stream:expr$(,)?) => {
        if let Some(permit) = $permit.take() {
            log::trace!("stanza received, passing to frontend via permit");
            permit.send($value);
        } else {
            log::trace!("no permit for received stanza available, blocking on channel send while handling writes");
            tokio::select! {
                // drive_writes never completes: I/O errors are reported on
                // the next call to drive_duplex(), which makes it ideal for
                // use in parallel to $ch.send().
                result = $stream.drive_writes(&mut $txq) => { match result {} },
                result = $ch.send($value) => match result {
                    Err(_) => break,
                    Ok(()) => (),
                },
            }
        }
    };
}

impl StanzaStreamWorker {
    pub fn spawn(
        mut reconnector: Box<
            dyn FnMut(Option<String>, oneshot::Sender<Connection>) + Send + 'static,
        >,
        queue_depth: usize,
    ) -> (mpsc::Sender<QueueEntry>, mpsc::Receiver<Event>) {
        let (conn_tx, conn_rx) = oneshot::channel();
        reconnector(None, conn_tx);
        // c2f = core to frontend
        let (c2f_tx, c2f_rx) = mpsc::channel(queue_depth);
        // f2c = frontend to core
        let (f2c_tx, transmit_queue) = TransmitQueue::channel(queue_depth);
        let mut worker = StanzaStreamWorker {
            reconnector,
            frontend_tx: c2f_tx,
            stream: WorkerStream::Connecting {
                slot: conn_rx,
                sm_state: None,
                notify: None,
            },
            transmit_queue,
        };
        tokio::spawn(async move { worker.run().await });
        (f2c_tx, c2f_rx)
    }

    pub async fn run(&mut self) {
        // TODO: consider moving this into SmState somehow, i.e. run a kind
        // of fake stream management exploiting the sequentiality requirement
        // from RFC 6120.
        // NOTE: we use a random starting value here to avoid clashes with
        // other application code.
        let mut ping_probe_ctr: u64 = rand::random();

        // We use mpsc::Sender permits (check the docs on
        // [`tokio::sync::mpsc::Sender::reserve`]) as a way to avoid blocking
        // on the `frontend_tx` whenever possible.
        //
        // We always try to have a permit available. If we have a permit
        // available, any event we receive from the stream can be sent to
        // the frontend tx without blocking. If we do not have a permit
        // available, the code generated by the send_or_break macro will
        // use the normal Sender::send coroutine function, but will also
        // service stream writes in parallel (putting backpressure on the
        // sender while not blocking writes on our end).
        let mut permit = None;
        loop {
            tokio::select! {
                new_permit = self.frontend_tx.reserve(), if permit.is_none() && !self.frontend_tx.is_closed() => match new_permit {
                    Ok(new_permit) => permit = Some(new_permit),
                    // Receiver side dropped… That is stream closure, so we
                    // shut everything down and exit.
                    Err(_) => break,
                },
                ev = self.stream.drive_duplex(&mut self.transmit_queue) => {
                    let Some(ev) = ev else {
                        // Stream terminated by local choice. Exit.
                        break;
                    };
                    match ev {
                        WorkerEvent::Reset { bound_jid, features } => send_or_break!(
                            Event::Stream(StreamEvent::Reset { bound_jid, features }) => permit in self.frontend_tx,
                            self.transmit_queue => self.stream,
                        ),
                        WorkerEvent::Disconnected { slot, error } => {
                            send_or_break!(
                                Event::Stream(StreamEvent::Suspended) => permit in self.frontend_tx,
                                self.transmit_queue => self.stream,
                            );
                            if let Some(error) = error {
                                log::debug!("Backend stream got disconnected because of an I/O error: {error}. Attempting reconnect.");
                            } else {
                                log::debug!("Backend stream got disconnected for an unknown reason. Attempting reconnect.");
                            }
                            if self.frontend_tx.is_closed() || self.transmit_queue.is_closed() {
                                log::debug!("Immediately aborting reconnect because the frontend is gone.");
                                break;
                            }
                            (self.reconnector)(None, slot);
                        }
                        WorkerEvent::Resumed => send_or_break!(
                            Event::Stream(StreamEvent::Resumed) => permit in self.frontend_tx,
                            self.transmit_queue => self.stream,
                        ),
                        WorkerEvent::Stanza(stanza) => send_or_break!(
                            Event::Stanza(stanza) => permit in self.frontend_tx,
                            self.transmit_queue => self.stream,
                        ),
                        WorkerEvent::ParseError(e) => {
                            log::error!("Parse error on stream: {e}");
                            self.stream.start_send_stream_error(parse_error_to_stream_error(e));
                            // We are not break-ing here, because drive_duplex
                            // is sending the error.
                        }
                        WorkerEvent::SoftTimeout => {
                            if self.stream.queue_sm_request() {
                                log::debug!("SoftTimeout tripped: enqueued <sm:r/>");
                            } else {
                                log::debug!("SoftTimeout tripped. Stream Management is not enabled, enqueueing ping IQ");
                                ping_probe_ctr = ping_probe_ctr.wrapping_add(1);
                                // We can leave to/from blank because those
                                // are not needed to send a ping to the peer.
                                // (At least that holds true on c2s streams.
                                // On s2s, things are more complicated anyway
                                // due to how bidi works.)
                                self.transmit_queue.enqueue(QueueEntry::untracked(Box::new(iq::Iq::from_get(
                                    format!("{}-{}", PING_PROBE_ID_PREFIX, ping_probe_ctr),
                                    ping::Ping,
                                ).into())));
                            }
                        }
                        WorkerEvent::ReconnectAborted => {
                            panic!("Backend was unable to handle reconnect request.");
                        }
                    }
                },
            }
        }
        match self.stream.close().await {
            Ok(()) => log::debug!("Stream closed successfully"),
            Err(e) => log::debug!("Stream closure failed: {e}"),
        }
    }
}

async fn shutdown_stream_by_remote_choice(mut stream: XmppStream, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    match tokio::time::timeout_at(
        deadline,
        <XmppStream as SinkExt<&Stanza>>::close(&mut stream),
    )
    .await
    {
        // We don't really care about success or failure here.
        Ok(_) => (),
        // .. but if we run in a timeout, we exit here right away.
        Err(_) => {
            log::debug!("Giving up on clean stream shutdown after timeout elapsed.");
            return;
        }
    }
    let timeout = tokio::time::sleep_until(deadline);
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            _ = &mut timeout => {
                log::debug!("Giving up on clean stream shutdown after timeout elapsed.");
                break;
            }
            ev = stream.next() => match ev {
                None => break,
                Some(Ok(data)) => {
                    log::debug!("Ignoring data on stream during shutdown: {data:?}");
                    break;
                }
                Some(Err(ReadError::HardError(e))) => {
                    log::debug!("Ignoring stream I/O error during shutdown: {e}");
                    break;
                }
                Some(Err(ReadError::SoftTimeout)) => (),
                Some(Err(ReadError::ParseError(_))) => (),
                Some(Err(ReadError::StreamFooterReceived)) => (),
            }
        }
    }
}
