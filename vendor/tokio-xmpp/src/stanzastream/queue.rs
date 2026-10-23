// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::cmp::Ordering;
use core::fmt;
use core::task::{Context, Poll};
use std::collections::VecDeque;
use std::io;

use futures::ready;

use tokio::sync::{mpsc, watch};

use crate::Stanza;

#[derive(Debug, Clone)]
pub struct OpaqueIoError {
    kind: io::ErrorKind,
    message: String,
}

impl OpaqueIoError {
    pub fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    pub fn into_io_error(self) -> io::Error {
        io::Error::new(self.kind, self.message)
    }

    pub fn to_io_error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}

impl From<io::Error> for OpaqueIoError {
    fn from(other: io::Error) -> Self {
        <Self as From<&io::Error>>::from(&other)
    }
}

impl From<&io::Error> for OpaqueIoError {
    fn from(other: &io::Error) -> Self {
        Self {
            kind: other.kind(),
            message: other.to_string(),
        }
    }
}

impl fmt::Display for OpaqueIoError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl core::error::Error for OpaqueIoError {}

/// The five stages of stanza transmission.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum StanzaStage {
    /// The stanza is in the transmit queue, but has not been serialised or
    /// sent to the stream yet.
    Queued,

    /// The stanza was successfully serialised and put into the transmit
    /// buffers.
    Sent,

    /// The stanza has been acked by the peer using XEP-0198 or comparable
    /// means.
    ///
    /// **Note:** This state is only ever reached on streams where XEP-0198
    /// was successfully negotiated.
    Acked,

    /// Stanza transmission or serialisation failed.
    Failed,

    /// The stanza was dropped from the transmit queue before it could be
    /// sent.
    ///
    /// This may happen if the stream breaks in a fatal, panick-y way.
    Dropped,
}

impl From<&StanzaState> for StanzaStage {
    fn from(other: &StanzaState) -> Self {
        match other {
            StanzaState::Queued => Self::Queued,
            StanzaState::Sent { .. } => Self::Sent,
            StanzaState::Acked { .. } => Self::Acked,
            StanzaState::Failed { .. } => Self::Failed,
            StanzaState::Dropped => Self::Dropped,
        }
    }
}

impl PartialEq<StanzaStage> for StanzaState {
    fn eq(&self, other: &StanzaStage) -> bool {
        StanzaStage::from(self).eq(other)
    }
}

impl PartialEq<StanzaState> for StanzaStage {
    fn eq(&self, other: &StanzaState) -> bool {
        self.eq(&Self::from(other))
    }
}

impl PartialOrd<StanzaStage> for StanzaState {
    fn partial_cmp(&self, other: &StanzaStage) -> Option<Ordering> {
        StanzaStage::from(self).partial_cmp(other)
    }
}

impl PartialOrd<StanzaState> for StanzaStage {
    fn partial_cmp(&self, other: &StanzaState) -> Option<Ordering> {
        self.partial_cmp(&Self::from(other))
    }
}

/// State of a stanza in transit to the peer.
#[derive(Debug, Clone)]
pub enum StanzaState {
    /// The stanza has been enqueued in the local queue but not sent yet.
    Queued,

    /// The stanza has been sent to the server, but there is no proof that it
    /// has been received by the server yet.
    Sent {
        /*
        /// The time from when the stanza was enqueued until the time it was
        /// sent on the stream.
        queue_delay: Duration,
        */
    },

    /// Confirmation that the stanza has been seen by the server has been
    /// received.
    Acked {
        /*
        /// The time from when the stanza was enqueued until the time it was
        /// sent on the stream.
        queue_delay: Duration,

        /// The time between sending the stanza on the stream and receiving
        /// confirmation from the server.
        ack_delay: Duration,
        */
    },

    /// Sending the stanza has failed in a non-recoverable manner.
    Failed {
        /// The error which caused the sending to fail.
        error: OpaqueIoError,
    },

    /// The stanza was dropped out of the queue for unspecified reasons,
    /// such as the stream breaking in a fatal, panick-y way.
    Dropped,
}

/// Track stanza transmission through the
/// [`StanzaStream`][`super::StanzaStream`] up to the peer.
#[derive(Clone)]
pub struct StanzaToken {
    inner: watch::Receiver<StanzaState>,
}

impl StanzaToken {
    /// Wait for the stanza transmission to reach the given state.
    ///
    /// If the stanza is removed from tracking before that state is reached,
    /// `None` is returned.
    pub async fn wait_for(&mut self, state: StanzaStage) -> Option<StanzaState> {
        self.inner
            .wait_for(|st| *st >= state)
            .await
            .map(|x| x.clone())
            .ok()
    }

    pub(crate) fn into_stream(self) -> tokio_stream::wrappers::WatchStream<StanzaState> {
        tokio_stream::wrappers::WatchStream::new(self.inner)
    }

    /// Read the current transmission state.
    pub fn state(&self) -> StanzaState {
        self.inner.borrow().clone()
    }
}

pub(super) struct QueueEntry {
    pub stanza: Box<Stanza>,
    pub token: watch::Sender<StanzaState>,
}

impl QueueEntry {
    pub fn untracked(st: Box<Stanza>) -> Self {
        Self::tracked(st).0
    }

    pub fn tracked(st: Box<Stanza>) -> (Self, StanzaToken) {
        let (tx, rx) = watch::channel(StanzaState::Queued);
        let token = StanzaToken { inner: rx };
        (
            QueueEntry {
                stanza: st,
                token: tx,
            },
            token,
        )
    }
}

/// Reference to a transmit queue entry.
///
/// On drop, the entry is returned to the queue.
pub(super) struct TransmitQueueRef<'x, T> {
    q: &'x mut VecDeque<T>,
}

impl<T> TransmitQueueRef<'_, T> {
    /// Take the item out of the queue.
    pub fn take(self) -> T {
        // Unwrap: when this type is created, a check is made that the queue
        // actually has a front item and because we borrow, that also cannot
        // change.
        self.q.pop_front().unwrap()
    }
}

/// A transmit queue coupled to an [`mpsc::Receiver`].
///
/// The transmit queue will by default only allow one element to reside in the
/// queue outside the inner `Receiver`: the main queueing happens inside the
/// receiver and is governed by its queue depth and associated backpressure.
///
/// However, the queue does allow prepending elements to the front, which is
/// useful for retransmitting items.
pub(super) struct TransmitQueue<T: Unpin> {
    inner: mpsc::Receiver<T>,
    peek: VecDeque<T>,
}

impl<T: Unpin> TransmitQueue<T> {
    /// Create a new transmission queue around an existing mpsc receiver.
    pub fn wrap(ch: mpsc::Receiver<T>) -> Self {
        Self {
            inner: ch,
            peek: VecDeque::with_capacity(1),
        }
    }

    /// Create a new mpsc channel and wrap the receiving side in a
    /// transmission queue
    pub fn channel(depth: usize) -> (mpsc::Sender<T>, Self) {
        let (tx, rx) = mpsc::channel(depth);
        (tx, Self::wrap(rx))
    }

    /// Poll the queue for the next item to transmit.
    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<Option<TransmitQueueRef<'_, T>>> {
        if !self.peek.is_empty() {
            // Cannot use `if let Some(.) = .` here because of a borrowchecker
            // restriction. If the reference is created before the branch is
            // entered, it will think it needs to be borrowed until the end
            // of the function (and that will conflict with the mutable
            // borrow we do for `self.peek.push_back` below).
            // See also https://github.com/rust-lang/rust/issues/54663.
            return Poll::Ready(Some(TransmitQueueRef { q: &mut self.peek }));
        } else {
            // The target size for the queue is 1, effectively acting as an
            // Option<T>. In some cases, we need more than one, but that is
            // always only a temporary burst (e.g. SM resumption
            // retransmissions), so we release the memory as soon as possible
            // after that.
            // Even though the target size is 1, we don't want to be pedantic
            // about this and we don't want to reallocate often. Some short
            // bursts are ok, and given that the stanzas inside QueueEntry
            // elements (the main use case for this type) are boxed anyway,
            // the size of the elements is rather small.
            if self.peek.capacity() > 32 {
                // We do not use shrink_to here, because we are *certain* that
                // we won't need a larger capacity any time soon, and
                // allocators may avoid moving data around.
                let mut new = VecDeque::new();
                core::mem::swap(&mut self.peek, &mut new);
            }
        }
        match ready!(self.inner.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(v) => {
                self.peek.push_back(v);
                Poll::Ready(Some(TransmitQueueRef { q: &mut self.peek }))
            }
        }
    }

    /// Requeue a sequence of items to the front of the queue.
    ///
    /// This function preserves ordering of the elements in `iter`, meaning
    /// that the first item from `iter` is going to be the next item yielded
    /// by `poll_take` or `poll_peek`.
    pub fn requeue_all<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        let iter = iter.into_iter();
        let to_reserve = iter.size_hint().1.unwrap_or(iter.size_hint().0);
        self.peek.reserve(to_reserve);
        let mut n = 0;
        for item in iter {
            self.peek.push_front(item);
            n += 1;
        }
        // Now we need to revert the order: we pushed the elements to the
        // front, so if we now read back from the front via poll_peek or
        // poll_take, that will cause them to be read in reverse order. The
        // following loop fixes that.
        for i in 0..(n / 2) {
            let j = n - (i + 1);
            self.peek.swap(i, j);
        }
    }

    /// Enqueues an item to be sent after all items in the *local* queue, but
    /// *before* all items which are still inside the inner `mpsc` channel.
    pub fn enqueue(&mut self, item: T) {
        self.peek.push_back(item);
    }

    /// Return true if the sender side of the queue is closed.
    ///
    /// Note that there may still be items which can be retrieved from the
    /// queue even though it has been closed.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl TransmitQueue<QueueEntry> {
    /// Fail all currently queued items with the given error.
    ///
    /// Future items will not be affected.
    pub fn fail(&mut self, error: &OpaqueIoError) {
        for item in self.peek.drain(..) {
            item.token.send_replace(StanzaState::Failed {
                error: error.clone(),
            });
        }
        while let Ok(item) = self.inner.try_recv() {
            item.token.send_replace(StanzaState::Failed {
                error: error.clone(),
            });
        }
        self.peek.shrink_to(1);
    }
}
