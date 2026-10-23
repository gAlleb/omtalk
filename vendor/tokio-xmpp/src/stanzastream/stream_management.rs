// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::fmt;
use std::collections::{vec_deque, VecDeque};

use xmpp_parsers::sm;

use super::queue::{QueueEntry, StanzaState};

#[derive(Debug)]
pub(super) enum SmResumeInfo {
    NotResumable,
    Resumable {
        /// XEP-0198 stream ID
        id: String,

        /// Preferred IP and port for resumption as indicated by the peer.
        // TODO: pass this to the reconnection logic.
        #[allow(dead_code)]
        location: Option<String>,
    },
}

/// State for stream management
pub(super) struct SmState {
    /// Last value seen from the remote stanza counter.
    outbound_base: u32,

    /// Counter for received stanzas
    inbound_ctr: u32,

    /// Number of `<sm:a/>` we still need to send.
    ///
    /// Acks cannot always be sent right away (if our tx buffer is full), and
    /// instead of cluttering our outbound queue or something with them, we
    /// just keep a counter of unsanswered `<sm:r/>`. The stream will process
    /// these in due time.
    pub(super) pending_acks: usize,

    /// Flag indicating that a `<sm:r/>` request should be sent.
    pub(super) pending_req: bool,

    /// Information about resumability of the stream
    resumption: SmResumeInfo,

    /// Unacked stanzas in the order they were sent
    // We use a VecDeque here because that has better performance
    // characteristics with the ringbuffer-type usage we're seeing here:
    // we push stuff to the back, and then drain it from the front. Vec would
    // have to move all the data around all the time, while VecDeque will just
    // move some pointers around.
    unacked_stanzas: VecDeque<QueueEntry>,
}

impl fmt::Debug for SmState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SmState")
            .field("outbound_base", &self.outbound_base)
            .field("inbound_ctr", &self.inbound_ctr)
            .field("resumption", &self.resumption)
            .field("len(unacked_stanzas)", &self.unacked_stanzas.len())
            .finish()
    }
}

#[derive(Debug)]
pub(super) enum SmError {
    RemoteAckedMoreStanzas {
        local_base: u32,
        queue_len: u32,
        remote_ctr: u32,
    },
    RemoteAckWentBackwards {
        local_base: u32,
        // NOTE: this is not needed to fully specify the error, but it's
        // needed to generate a `<handled-count-too-high/>` from Self.
        queue_len: u32,
        remote_ctr: u32,
    },
}

impl From<SmError> for xmpp_parsers::stream_error::StreamError {
    fn from(other: SmError) -> Self {
        let (h, send_count) = match other {
            SmError::RemoteAckedMoreStanzas {
                local_base,
                queue_len,
                remote_ctr,
            } => (remote_ctr, local_base.wrapping_add(queue_len)),
            SmError::RemoteAckWentBackwards {
                local_base,
                queue_len,
                remote_ctr,
            } => (remote_ctr, local_base.wrapping_add(queue_len)),
        };
        xmpp_parsers::sm::HandledCountTooHigh { h, send_count }.into()
    }
}

impl fmt::Display for SmError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::RemoteAckedMoreStanzas {
                local_base,
                queue_len,
                remote_ctr,
            } => {
                let local_tip = local_base.wrapping_add(*queue_len);
                write!(f, "remote acked more stanzas than we sent: remote counter = {}. queue covers range {}..<{}", remote_ctr, local_base, local_tip)
            }
            Self::RemoteAckWentBackwards {
                local_base,
                remote_ctr,
                ..
            } => {
                write!(f, "remote acked less stanzas than before: remote counter = {}, local queue starts at {}", remote_ctr, local_base)
            }
        }
    }
}

impl SmState {
    /// Mark a stanza as sent and keep it in the stream management queue.
    pub fn enqueue(&mut self, entry: QueueEntry) {
        // This may seem like an arbitrary limit, but there's some thought
        // in this.
        // First, the SM counters go up to u32 at most and then wrap around.
        // That means that any queue size larger than u32 would immediately
        // cause ambiguities when resuming.
        // Second, there's RFC 1982 "Serial Number Arithmetic". It is used for
        // example in DNS for the serial number and it has thoughts on how to
        // use counters which wrap around at some point. The document proposes
        // that if the (wrapped) difference between two numbers is larger than
        // half the number space, you should consider it as a negative
        // difference.
        //
        // Hence the ambiguity already starts at u32::MAX / 2, so we limit the
        // queue to one less than that.
        const MAX_QUEUE_SIZE: usize = (u32::MAX / 2 - 1) as usize;
        if self.unacked_stanzas.len() >= MAX_QUEUE_SIZE {
            // We don't bother with an error return here. u32::MAX / 2 stanzas
            // in the queue is fatal in any circumstance I can fathom (also,
            // we have no way to return this error to the
            // [`StanzaStream::send`] call anyway).
            panic!("Too many pending stanzas.");
        }

        self.unacked_stanzas.push_back(entry);
        log::trace!(
            "Stored stanza in SmState. We are now at {} unacked stanzas.",
            self.unacked_stanzas.len()
        );
    }

    /// Process resumption.
    ///
    /// Updates the internal state according to the received remote counter.
    /// Returns an iterator which yields the queue entries which need to be
    /// retransmitted.
    pub fn resume(&mut self, h: u32) -> Result<vec_deque::Drain<'_, QueueEntry>, SmError> {
        self.remote_acked(h)?;
        // Return the entire leftover queue. We cannot receive acks for them,
        // unless they are retransmitted, because the peer has not seen them
        // yet (they got lost in the previous unclean disconnect).
        Ok(self.unacked_stanzas.drain(..))
    }

    /// Process remote `<a/>`
    pub fn remote_acked(&mut self, h: u32) -> Result<(), SmError> {
        log::debug!("remote_acked: {self:?}::remote_acked({h})");
        // XEP-0198 specifies that counters are mod 2^32, which is handy when
        // you use u32 data types :-).
        let to_drop = h.wrapping_sub(self.outbound_base) as usize;
        if to_drop > 0 {
            log::trace!("remote_acked: need to drop {to_drop} stanzas");
            if to_drop > self.unacked_stanzas.len() {
                if to_drop as u32 > u32::MAX / 2 {
                    // If we look at the stanza counter values as RFC 1982
                    // values, a wrapping difference greater than half the
                    // number space indicates a negative difference, i.e.
                    // h went backwards.
                    return Err(SmError::RemoteAckWentBackwards {
                        local_base: self.outbound_base,
                        queue_len: self.unacked_stanzas.len() as u32,
                        remote_ctr: h,
                    });
                } else {
                    return Err(SmError::RemoteAckedMoreStanzas {
                        local_base: self.outbound_base,
                        queue_len: self.unacked_stanzas.len() as u32,
                        remote_ctr: h,
                    });
                }
            }
            for entry in self.unacked_stanzas.drain(..to_drop) {
                entry.token.send_replace(StanzaState::Acked {});
            }
            self.outbound_base = h;
            log::debug!("remote_acked: remote acked {to_drop} stanzas");
            Ok(())
        } else {
            log::trace!("remote_acked: no stanzas to drop");
            Ok(())
        }
    }

    /// Get the current inbound counter.
    #[inline(always)]
    pub fn inbound_ctr(&self) -> u32 {
        self.inbound_ctr
    }

    /// Increase the inbound counter.
    pub fn received(&mut self) {
        self.inbound_ctr = self.inbound_ctr.wrapping_add(1);
    }

    /// Get the info necessary for resumption.
    ///
    /// Returns the stream ID and the current inbound counter if resumption is
    /// available and None otherwise.
    pub fn resume_info(&self) -> Option<(&str, u32)> {
        match self.resumption {
            SmResumeInfo::Resumable { ref id, .. } => Some((id, self.inbound_ctr)),
            SmResumeInfo::NotResumable => None,
        }
    }
}

/// Initialize stream management state
impl From<sm::Enabled> for SmState {
    fn from(other: sm::Enabled) -> Self {
        let resumption = if other.resume {
            match other.id {
                Some(id) => SmResumeInfo::Resumable {
                    location: other.location,
                    id: id.0,
                },
                None => {
                    log::warn!("peer replied with <enable resume='true'/>, but without an ID! cannot make this stream resumable.");
                    SmResumeInfo::NotResumable
                }
            }
        } else {
            SmResumeInfo::NotResumable
        };

        Self {
            outbound_base: 0,
            inbound_ctr: 0,
            pending_acks: 0,
            pending_req: false,
            resumption,
            unacked_stanzas: VecDeque::new(),
        }
    }
}
