// Copyright (c) 2025 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use core::error::Error;
use core::fmt;
use core::future::Future;
use core::ops::ControlFlow;
use core::pin::Pin;
use core::task::{ready, Context, Poll};
use std::io;
use std::sync::Mutex;
use xmpp_parsers::jid::BareJid;

use futures::Stream;
use tokio::sync::oneshot;

use xmpp_parsers::{iq::Iq, stanza_error::StanzaError};

use crate::{
    event::make_id,
    jid::Jid,
    minidom::Element,
    stanzastream::{StanzaState, StanzaToken},
};

/// An IQ request payload
#[derive(Debug)]
pub enum IqRequest {
    /// Payload for a `type="get"` request
    Get(Element),

    /// Payload for a `type="set"` request
    Set(Element),
}

impl IqRequest {
    fn into_iq(self, from: Option<Jid>, to: Option<Jid>, id: String) -> Iq {
        match self {
            Self::Get(payload) => Iq::Get {
                from,
                to,
                id,
                payload,
            },
            Self::Set(payload) => Iq::Set {
                from,
                to,
                id,
                payload,
            },
        }
    }
}

/// An IQ response payload
#[derive(Debug)]
pub enum IqResponse {
    /// Payload for a `type="result"` response.
    Result(Option<Element>),

    /// Payload for a `type="error"` response.
    Error(StanzaError),
}

impl IqResponse {
    fn into_iq(self, from: Option<Jid>, to: Option<Jid>, id: String) -> Iq {
        match self {
            Self::Error(error) => Iq::Error {
                from,
                to,
                id,
                error,
                payload: None,
            },
            Self::Result(payload) => Iq::Result {
                from,
                to,
                id,
                payload,
            },
        }
    }
}

/// Error enumeration for Iq sending failures
#[derive(Debug)]
pub enum IqFailure {
    /// Internal error inside tokio_xmpp which caused the stream worker to
    /// drop the token before the response was received.
    ///
    /// Most likely, this means that the stream has died with a panic.
    LostWorker,

    /// The IQ failed to send because of an I/O or serialisation error.
    SendError(io::Error),
}

impl fmt::Display for IqFailure {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::LostWorker => {
                f.write_str("disconnected from internal connection worker while sending IQ")
            }
            Self::SendError(e) => write!(f, "send error: {e}"),
        }
    }
}

impl Error for IqFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SendError(ref e) => Some(e),
            Self::LostWorker => None,
        }
    }
}

type IqKey = (Option<Jid>, String);
type IqMap = BTreeMap<IqKey, IqResponseSink>;

#[derive(Debug)]
struct IqMapEntryHandle {
    key: IqKey,
    map: Weak<Mutex<IqMap>>,
}

impl Drop for IqMapEntryHandle {
    fn drop(&mut self) {
        let Some(map) = self.map.upgrade() else {
            return;
        };
        let Some(mut map) = map.lock().ok() else {
            return;
        };
        map.remove(&self.key);
    }
}

pin_project_lite::pin_project! {
    /// Handle for awaiting an IQ response.
    ///
    /// The `IqResponseToken` can be awaited and will generate a result once
    /// the Iq response has been received. Note that an `Ok(_)` result does
    /// **not** imply a successful execution of the remote command: It may
    /// contain a [`IqResponse::Error`] variant.
    ///
    /// Note that there are no internal timeouts for Iq responses: If a reply
    /// never arrives, the [`IqResponseToken`] future will never complete.
    /// Most of the time, you should combine that token with something like
    /// [`tokio::time::timeout`].
    ///
    /// Dropping (cancelling) an `IqResponseToken` removes the internal
    /// bookkeeping required for tracking the response.
    #[derive(Debug)]
    pub struct IqResponseToken {
        entry: Option<IqMapEntryHandle>,
        #[pin]
        stanza_token: Option<tokio_stream::wrappers::WatchStream<StanzaState>>,
        #[pin]
        inner: oneshot::Receiver<Result<IqResponse, IqFailure>>,
    }
}

impl IqResponseToken {
    /// Tie a stanza token to this IQ response token.
    ///
    /// The stanza token should point at the IQ **request**, the response of
    /// which this response token awaits.
    ///
    /// Awaiting the response token will then handle error states in the
    /// stanza token and return IqFailure as appropriate.
    pub(crate) fn set_stanza_token(&mut self, token: StanzaToken) {
        assert!(self.stanza_token.is_none());
        self.stanza_token = Some(token.into_stream());
    }
}

impl Future for IqResponseToken {
    type Output = Result<IqResponse, IqFailure>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        match this.inner.poll(cx) {
            Poll::Ready(Ok(v)) => {
                // Drop the map entry handle to release some memory.
                this.entry.take();
                return Poll::Ready(v);
            }
            Poll::Ready(Err(_)) => {
                log::warn!("IqResponseToken oneshot::Receiver returned receive error!");
                // Drop the map entry handle to release some memory.
                this.entry.take();
                return Poll::Ready(Err(IqFailure::LostWorker));
            }
            Poll::Pending => (),
        };

        loop {
            match this.stanza_token.as_mut().as_pin_mut() {
                // We have a stanza token to look at, so we check its state.
                Some(stream) => match ready!(stream.poll_next(cx)) {
                    // Still in the queue.
                    Some(StanzaState::Queued) => (),

                    Some(StanzaState::Dropped) | None => {
                        log::warn!("StanzaToken associated with IqResponseToken signalled that the Stanza was dropped before transmission.");
                        // Drop the map entry handle to release some memory.
                        this.entry.take();
                        // Lost stanza stream: cannot ever get a reply.
                        return Poll::Ready(Err(IqFailure::LostWorker));
                    }

                    Some(StanzaState::Failed { error }) => {
                        // Drop the map entry handle to release some memory.
                        this.entry.take();
                        // Send error: cannot ever get a reply.
                        return Poll::Ready(Err(IqFailure::SendError(error.into_io_error())));
                    }

                    Some(StanzaState::Sent { .. }) | Some(StanzaState::Acked { .. }) => {
                        // Sent successfully, stop polling the stream: We do
                        // not care what happens after successful sending,
                        // the next step we expect is that this.inner
                        // completes.
                        *this.stanza_token = None;
                        return Poll::Pending;
                    }
                },

                // No StanzaToken to poll, so we return Poll::Pending and hope
                // that we will get a response through this.inner eventually..
                None => return Poll::Pending,
            }
        }
    }
}

#[derive(Debug)]
struct IqResponseSink {
    inner: oneshot::Sender<Result<IqResponse, IqFailure>>,
}

impl IqResponseSink {
    fn complete(self, resp: IqResponse) {
        let _: Result<_, _> = self.inner.send(Ok(resp));
    }
}

/// Utility struct to track IQ responses.
#[derive(Clone, Debug)]
pub struct IqResponseTracker {
    map: Arc<Mutex<IqMap>>,
    account_jid: Arc<Mutex<Option<BareJid>>>,
}

impl IqResponseTracker {
    /// Create a new empty response tracker.
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(IqMap::new())),
            account_jid: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the local JID the `IqResponseTracker` is handling IQs on behalf of.
    pub fn set_account_jid(&self, jid: BareJid) {
        let mut guard = self.account_jid.lock().unwrap();
        *guard = Some(jid);
    }

    /// Attempt to handle an IQ stanza as IQ response.
    ///
    /// Returns the IQ stanza unharmed if it is not an IQ response matching
    /// any request which is still being tracked.
    pub fn handle_iq(&self, iq: Iq) -> ControlFlow<(), Iq> {
        let (mut from, to, id, payload) = match iq {
            Iq::Error {
                from,
                to,
                id,
                error,
                payload: _,
            } => (from, to, id, IqResponse::Error(error)),
            Iq::Result {
                from,
                to,
                id,
                payload,
            } => (from, to, id, IqResponse::Result(payload)),
            _ => return ControlFlow::Continue(iq),
        };

        if from.is_none() {
            // Implicitly setting None to the JID the tracker is active for in case the server
            // doesn't. This ensures that the IQ can be matched in the map again.
            let account_jid = self.account_jid.lock().unwrap();
            from = account_jid.clone().map(Jid::from);
        }

        let key = (from, id);
        let mut map = self.map.lock().unwrap();
        match map.remove(&key) {
            None => {
                log::debug!("not handling IQ response from {:?} with id {:?}: no active tracker for this tuple", key.0, key.1);
                log::trace!("active trackers: {map:?}");
                ControlFlow::Continue(payload.into_iq(key.0, to, key.1))
            }
            Some(sink) => {
                log::trace!("completing IQ {:?}", key.0);
                sink.complete(payload);
                ControlFlow::Break(())
            }
        }
    }

    /// Allocate a new IQ response tracking handle.
    ///
    /// This modifies the IQ to assign a unique ID.
    pub fn allocate_iq_handle(
        &self,
        from: Option<Jid>,
        mut to: Option<Jid>,
        req: IqRequest,
    ) -> (Iq, IqResponseToken) {
        if to.is_none() {
            // Implicitly setting None to the JID the tracker is active for, which the server
            // should do as well. This ensures that the IQ can be matched in the map again.
            let account_jid = self.account_jid.lock().unwrap();
            to = account_jid.clone().map(Jid::from);
        }

        let key = (to, make_id());
        let mut map = self.map.lock().unwrap();
        let (tx, rx) = oneshot::channel();
        let sink = IqResponseSink { inner: tx };
        assert!(map.get(&key).is_none());
        let token = IqResponseToken {
            entry: Some(IqMapEntryHandle {
                key: key.clone(),
                map: Arc::downgrade(&self.map),
            }),
            stanza_token: None,
            inner: rx,
        };
        map.insert(key.clone(), sink);
        (req.into_iq(from, key.0, key.1), token)
    }
}
