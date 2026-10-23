// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::ops::ControlFlow::{self, Break, Continue};
use core::pin::Pin;
use core::task::{Context, Poll};
use std::io;

use futures::{ready, Sink, Stream};

use xmpp_parsers::{
    bind::{BindQuery, BindResponse},
    iq::Iq,
    jid::{FullJid, Jid},
    sm,
    stream_error::{DefinedCondition, StreamError},
    stream_features::StreamFeatures,
};

use crate::xmlstream::{ReadError, XmppStreamElement};
use crate::Stanza;

use super::queue::{QueueEntry, TransmitQueue};
use super::stream_management::*;
use super::worker::{parse_error_to_stream_error, XmppStream};

static BIND_REQ_ID: &str = "resource-binding";

pub(super) enum NegotiationState {
    /// Send request to enable or resume stream management.
    SendSmRequest {
        /// Stream management state to use. If present, resumption will be
        /// attempted. Otherwise, a fresh session will be established.
        sm_state: Option<SmState>,

        /// If the stream has been freshly bound, we carry the bound JID along
        /// with us.
        bound_jid: Option<FullJid>,
    },

    /// Await the response to the SM enable/resume request.
    ReceiveSmResponse {
        /// State to use.
        sm_state: Option<SmState>,

        /// If the stream has been freshly bound, we carry the bound JID along
        /// with us.
        bound_jid: Option<FullJid>,
    },

    /// Send a new request to bind to a resource.
    SendBindRequest { sm_supported: bool },

    /// Receive the bind response.
    ReceiveBindResponse { sm_supported: bool },
}

/// The ultimate result of a stream negotiation.
pub(super) enum NegotiationResult {
    /// An unplanned disconnect happened or a stream error was received from
    /// the remote party.
    Disconnect {
        /// Stream management state for a later resumption attempt.
        sm_state: Option<SmState>,

        /// I/O error which came along the disconnect.
        error: io::Error,
    },

    /// The negotiation completed successfully, but the stream was reset (i.e.
    /// stream management and all session state was lost).
    StreamReset {
        /// Stream management state. This may still be non-None if the new
        /// stream has successfully negotiated stream management.
        sm_state: Option<SmState>,

        /// The JID to which the stream is now bound.
        bound_jid: Jid,
    },

    /// The negotiation completed successfully and a previous session was
    /// resumed.
    StreamResumed {
        /// Negotiated stream management state.
        sm_state: SmState,
    },

    /// The negotiation failed and we need to emit a stream error.
    ///
    /// **Note:** Stream errors *received* from the peer are signalled using
    /// [`Self::Disconnect`] instead, with an I/O error of kind `Other`.
    StreamError {
        /// Stream error to send to the remote party with details about the
        /// failure.
        error: StreamError,
    },
}

impl NegotiationState {
    pub fn new(features: &StreamFeatures, sm_state: Option<SmState>) -> io::Result<Self> {
        if let Some(sm_state) = sm_state {
            if features.stream_management.is_some() {
                return Ok(Self::SendSmRequest {
                    sm_state: Some(sm_state),
                    bound_jid: None,
                });
            } else {
                log::warn!("Peer is not offering stream management anymore. Dropping state.");
            }
        }

        if !features.can_bind() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Peer is not offering the bind feature. Cannot proceed with stream negotiation.",
            ));
        }

        Ok(Self::SendBindRequest {
            sm_supported: features.stream_management.is_some(),
        })
    }

    fn flush(stream: Pin<&mut XmppStream>, cx: &mut Context) -> ControlFlow<io::Error, ()> {
        match <XmppStream as Sink<&XmppStreamElement>>::poll_flush(stream, cx) {
            Poll::Pending | Poll::Ready(Ok(())) => Continue(()),
            Poll::Ready(Err(error)) => Break(error),
        }
    }

    pub fn advance(
        &mut self,
        mut stream: Pin<&mut XmppStream>,
        jid: &Jid,
        transmit_queue: &mut TransmitQueue<QueueEntry>,
        cx: &mut Context<'_>,
    ) -> Poll<ControlFlow<NegotiationResult, Option<Stanza>>> {
        // When sending requests, we need to wait for the stream to become
        // ready to send and then send the corresponding request.
        // Note that if this wasn't a fresh stream (which it always is!),
        // doing it in this kind of simplex fashion could lead to deadlocks
        // (because we are blockingly sending without attempting to receive: a
        // peer could stop receiving from our side if their tx buffer was too
        // full or smth). However, because this stream is fresh, we know that
        // our tx buffers are empty enough that this will succeed quickly, so
        // that we can proceed.
        // TODO: define a deadline for negotiation.
        match self {
            Self::SendBindRequest { sm_supported } => {
                match ready!(<XmppStream as Sink<&Stanza>>::poll_ready(
                    stream.as_mut(),
                    cx
                )) {
                    // We can send.
                    Ok(()) => (),

                    // Stream broke horribly.
                    Err(error) => {
                        return Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: None,
                            error,
                        }))
                    }
                };

                let resource = jid.resource().map(|x| x.as_str().to_owned());
                let stanza = Iq::from_set(BIND_REQ_ID, BindQuery::new(resource));
                match stream.start_send(&stanza) {
                    Ok(()) => (),
                    Err(e) => panic!("failed to serialize BindQuery: {}", e),
                };

                *self = Self::ReceiveBindResponse {
                    sm_supported: *sm_supported,
                };
                Poll::Ready(Continue(None))
            }

            Self::ReceiveBindResponse { sm_supported } => {
                match Self::flush(stream.as_mut(), cx) {
                    Break(error) => {
                        return Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: None,
                            error,
                        }))
                    }
                    Continue(()) => (),
                }

                let item = ready!(stream.poll_next(cx));
                let item = item.unwrap_or_else(|| {
                    Err(ReadError::HardError(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof before stream footer",
                    )))
                });
                let item = item.map(|v| v.into_read_error()).flatten();

                match item {
                    Ok(XmppStreamElement::Stanza(data)) => match data {
                        Stanza::Iq(iq) if iq.id() == BIND_REQ_ID => {
                            let error = match iq {
                                Iq::Result {
                                    payload: Some(payload),
                                    ..
                                } => match BindResponse::try_from(payload) {
                                    Ok(v) => {
                                        let bound_jid = v.into();
                                        if *sm_supported {
                                            *self = Self::SendSmRequest {
                                                sm_state: None,
                                                bound_jid: Some(bound_jid),
                                            };
                                            return Poll::Ready(Continue(None));
                                        } else {
                                            return Poll::Ready(Break(
                                                NegotiationResult::StreamReset {
                                                    sm_state: None,
                                                    bound_jid: Jid::from(bound_jid),
                                                },
                                            ));
                                        }
                                    }
                                    Err(e) => e.to_string(),
                                },
                                Iq::Result { payload: None, .. } => {
                                    "Bind response has no payload".to_owned()
                                }
                                _ => "Unexpected IQ type in response to bind request".to_owned(),
                            };
                            log::warn!("Received IQ matching the bind request, but parsing failed ({error})! Emitting stream error.");
                            Poll::Ready(Break(NegotiationResult::StreamError {
                                error: StreamError::new(
                                    DefinedCondition::UndefinedCondition,
                                    "en",
                                    error,
                                )
                                .with_application_specific(vec![super::error::ParseError.into()]),
                            }))
                        }
                        st => {
                            log::warn!("Received unexpected stanza before response to bind request: {st:?}. Dropping.");
                            Poll::Ready(Continue(None))
                        }
                    },

                    Ok(XmppStreamElement::StreamError(error)) => {
                        log::debug!("Received stream:error, failing stream and discarding any stream management state.");
                        let error = io::Error::other(error);
                        transmit_queue.fail(&(&error).into());
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            error,
                            sm_state: None,
                        }))
                    }

                    Ok(other) => {
                        log::warn!("Received unsupported stream element during bind: {other:?}. Emitting stream error.");
                        Poll::Ready(Break(NegotiationResult::StreamError {
                            error: StreamError::new(
                                DefinedCondition::UnsupportedStanzaType,
                                "en",
                                format!("Unsupported stream element during bind: {other:?}"),
                            ),
                        }))
                    }

                    // Soft timeouts during negotiation are a bad sign
                    // (because we already prompted the server to send
                    // something and are waiting for it), but also nothing
                    // to write home about.
                    Err(ReadError::SoftTimeout) => Poll::Ready(Continue(None)),

                    // Parse errors during negotiation cause an unconditional
                    // stream error.
                    Err(ReadError::ParseError(e)) => {
                        Poll::Ready(Break(NegotiationResult::StreamError {
                            error: parse_error_to_stream_error(e),
                        }))
                    }

                    // I/O errors cause the stream to be considered
                    // broken; we drop it and send a Disconnect event with
                    // the error embedded.
                    Err(ReadError::HardError(error)) => {
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: None,
                            error,
                        }))
                    }

                    // Stream footer during negotiation is really weird.
                    // We kill the stream immediately with an error
                    // (but allow preservation of the SM state).
                    Err(ReadError::StreamFooterReceived) => {
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: None,
                            error: io::Error::new(
                                io::ErrorKind::InvalidData,
                                "stream footer received during negotiation",
                            ),
                        }))
                    }
                }
            }

            Self::SendSmRequest {
                sm_state,
                bound_jid,
            } => {
                match ready!(<XmppStream as Sink<&XmppStreamElement>>::poll_ready(
                    stream.as_mut(),
                    cx
                )) {
                    // We can send.
                    Ok(()) => (),

                    // Stream broke horribly.
                    Err(error) => {
                        return Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: sm_state.take(),
                            error,
                        }))
                    }
                };

                let nonza = if let Some((id, inbound_ctr)) =
                    sm_state.as_ref().and_then(|x| x.resume_info())
                {
                    // Attempt resumption
                    sm::Nonza::Resume(sm::Resume {
                        h: inbound_ctr,
                        previd: sm::StreamId(id.to_owned()),
                    })
                } else {
                    // Attempt enabling
                    sm::Nonza::Enable(sm::Enable {
                        max: None,
                        resume: true,
                    })
                };
                match stream.start_send(&XmppStreamElement::SM(nonza)) {
                    Ok(()) => (),
                    Err(e) => {
                        // We panic here, instead of returning an
                        // error, because after we confirmed via
                        // poll_ready that the stream is ready to
                        // send, the only error returned by start_send
                        // can be caused by our data.
                        panic!("Failed to send SM nonza: {}", e);
                    }
                }

                *self = Self::ReceiveSmResponse {
                    sm_state: sm_state.take(),
                    bound_jid: bound_jid.take(),
                };
                // Ask caller to poll us again immediately in order to
                // start flushing the stream.
                Poll::Ready(Continue(None))
            }

            Self::ReceiveSmResponse {
                sm_state,
                bound_jid,
            } => {
                match Self::flush(stream.as_mut(), cx) {
                    Break(error) => {
                        return Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: sm_state.take(),
                            error,
                        }))
                    }
                    Continue(()) => (),
                }

                // So the difficulty here is that there's a possibility
                // that we receive non-SM data while the SM negotiation
                // is going on.

                let item = ready!(stream.poll_next(cx));
                let item = item.unwrap_or_else(|| {
                    Err(ReadError::HardError(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof before stream footer",
                    )))
                });
                let item = item.map(|v| v.into_read_error()).flatten();

                match item {
                    // Pre-SM data. Note that we mustn't count this while we
                    // are still in negotiating state: we transit to
                    // [`Self::Ready`] immediately after we got the
                    // `<resumed/>` or `<enabled/>`, and before we got that,
                    // counting inbound stanzas is definitely wrong (see e.g.
                    // aioxmpp commit 796aa32).
                    Ok(XmppStreamElement::Stanza(data)) => Poll::Ready(Continue(Some(data))),

                    Ok(XmppStreamElement::SM(sm::Nonza::Enabled(enabled))) => {
                        if sm_state.is_some() {
                            // Okay, the peer violated the stream management
                            // protocol here (or we have a bug).
                            log::warn!(
                                "Received <enabled/>, but we also have previous SM state. One of us has a bug here (us or the peer) and I'm not sure which it is. If you can reproduce this, please re-run with trace loglevel and provide the logs. Attempting to proceed with a fresh session.",
                            );
                        }
                        // We must emit Reset here because this is a
                        // fresh stream and we did not resume.
                        Poll::Ready(Break(NegotiationResult::StreamReset {
                            sm_state: Some(enabled.into()),
                            bound_jid: bound_jid.take().expect("State machine error: no bound_jid available in SM negotiation.").into(),
                        }))
                    }

                    Ok(XmppStreamElement::SM(sm::Nonza::Resumed(resumed))) => match sm_state.take()
                    {
                        Some(mut sm_state) => {
                            // Yay!
                            match sm_state.resume(resumed.h) {
                                Ok(to_retransmit) => transmit_queue.requeue_all(to_retransmit),
                                Err(e) => {
                                    // We kill the stream with an error
                                    log::error!("Resumption failed: {e}");
                                    return Poll::Ready(Break(NegotiationResult::StreamError {
                                        error: e.into(),
                                    }));
                                }
                            }
                            Poll::Ready(Break(NegotiationResult::StreamResumed { sm_state }))
                        }
                        None => {
                            // Okay, the peer violated the stream management
                            // protocol here (or we have a bug).
                            // Unlike the
                            // received-enabled-but-attempted-to-resume
                            // situation, we do not have enough information to
                            // proceed without having the stream break soon.
                            // (If we proceeded without a SM state, we would
                            // have the stream die as soon as the peer
                            // requests our counters).
                            // We thus terminate the stream with an error.
                            // We must emit Reset here because this is a fresh
                            // stream and we did not resume.
                            Poll::Ready(Break(NegotiationResult::Disconnect {
                                sm_state: None,
                                error: io::Error::new(io::ErrorKind::InvalidData, "Peer replied to <sm:enable/> request with <sm:resumed/> response"),
                            }))
                        }
                    },

                    Ok(XmppStreamElement::SM(sm::Nonza::Failed(failed))) => match sm_state {
                        Some(sm_state) => {
                            log::debug!("Received <sm:failed/> in response to resumption request. Discarding SM data and attempting to renegotiate.");
                            if let Some(h) = failed.h {
                                // This is only an optimization anyway, so
                                // we can also just ignore this.
                                let _: Result<_, _> = sm_state.remote_acked(h);
                            }
                            *self = Self::SendBindRequest { sm_supported: true };
                            Poll::Ready(Continue(None))
                        }
                        None => {
                            log::warn!("Received <sm:failed/> in response to enable request. Proceeding without stream management.");

                            // We must emit Reset here because this is a
                            // fresh stream and we did not resume.
                            Poll::Ready(Break(NegotiationResult::StreamReset {
                                bound_jid: bound_jid.take().expect("State machine error: no bound_jid available in SM negotiation.").into(),
                                sm_state: None,
                            }))
                        }
                    },

                    Ok(XmppStreamElement::StreamError(error)) => {
                        log::debug!("Received stream error, failing stream and discarding any stream management state.");
                        let error = io::Error::other(error);
                        transmit_queue.fail(&(&error).into());
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            error,
                            sm_state: None,
                        }))
                    }

                    Ok(other) => {
                        log::warn!("Received unsupported stream element during negotiation: {other:?}. Emitting stream error.");
                        Poll::Ready(Break(NegotiationResult::StreamError {
                            error: StreamError::new(
                                DefinedCondition::UnsupportedStanzaType,
                                "en",
                                format!("Unsupported stream element during negotiation: {other:?}"),
                            ),
                        }))
                    }

                    // Soft timeouts during negotiation are a bad sign
                    // (because we already prompted the server to send
                    // something and are waiting for it), but also nothing
                    // to write home about.
                    Err(ReadError::SoftTimeout) => Poll::Ready(Continue(None)),

                    // Parse errors during negotiation cause an unconditional
                    // stream error.
                    Err(ReadError::ParseError(e)) => {
                        Poll::Ready(Break(NegotiationResult::StreamError {
                            error: parse_error_to_stream_error(e),
                        }))
                    }

                    // I/O errors cause the stream to be considered
                    // broken; we drop it and send a Disconnect event with
                    // the error embedded.
                    Err(ReadError::HardError(error)) => {
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: sm_state.take(),
                            error,
                        }))
                    }

                    // Stream footer during negotiation is really weird.
                    // We kill the stream immediately with an error
                    // (but allow preservation of the SM state).
                    Err(ReadError::StreamFooterReceived) => {
                        Poll::Ready(Break(NegotiationResult::Disconnect {
                            sm_state: sm_state.take(),
                            error: io::Error::new(
                                io::ErrorKind::InvalidData,
                                "stream footer received during negotiation",
                            ),
                        }))
                    }
                }
            }
        }
    }
}
