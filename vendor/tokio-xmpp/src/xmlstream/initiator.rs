// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::fmt;
use core::pin::Pin;
use std::borrow::Cow;
use std::io;

use futures::SinkExt;

use tokio::io::{AsyncBufRead, AsyncWrite};

use xmpp_parsers::{
    stream_error::{ReceivedStreamError, StreamError},
    stream_features::StreamFeatures,
};

use xso::FromXml;

use super::{
    common::{RawXmlStream, ReadXso, ReadXsoError, StreamHeader},
    XmlStream,
};

/// Type state for an initiator stream which has not yet sent its stream
/// header.
///
/// To continue stream setup, call [`send_header`][`Self::send_header`].
pub struct InitiatingStream<Io>(pub(super) RawXmlStream<Io>);

impl<Io: AsyncBufRead + AsyncWrite + Unpin> InitiatingStream<Io> {
    /// Send the stream header.
    pub async fn send_header(
        self,
        header: StreamHeader<'_>,
    ) -> io::Result<PendingFeaturesRecv<Io>> {
        let Self(mut stream) = self;

        header.send(Pin::new(&mut stream)).await?;
        stream.flush().await?;
        let header = StreamHeader::recv(Pin::new(&mut stream)).await?;
        Ok(PendingFeaturesRecv { stream, header })
    }
}

#[derive(xso::FromXml)]
#[xml()]
enum StreamFeaturesPayload {
    #[xml(transparent)]
    Features(StreamFeatures),
    #[xml(transparent)]
    Error(StreamError),
}

/// Error conditions when receiving stream features
#[derive(Debug)]
pub enum RecvFeaturesError {
    /// I/o error while receiving stream features
    Io(io::Error),

    /// Received a stream error instead of stream features
    StreamError(ReceivedStreamError),
}

impl fmt::Display for RecvFeaturesError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "i/o error: {e}"),
            Self::StreamError(e) => fmt::Display::fmt(&e, f),
        }
    }
}

impl core::error::Error for RecvFeaturesError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::StreamError(e) => Some(e),
        }
    }
}

impl From<io::Error> for RecvFeaturesError {
    fn from(other: io::Error) -> Self {
        Self::Io(other)
    }
}

/// Type state for an initiator stream which has sent and received the stream
/// header.
///
/// To continue stream setup, call [`recv_features`][`Self::recv_features`].
pub struct PendingFeaturesRecv<Io> {
    pub(super) stream: RawXmlStream<Io>,
    pub(super) header: StreamHeader<'static>,
}

impl<Io> PendingFeaturesRecv<Io> {
    /// The stream header contents as sent by the peer.
    pub fn header(&self) -> StreamHeader<'_> {
        StreamHeader {
            from: self.header.from.as_deref().map(Cow::Borrowed),
            to: self.header.to.as_deref().map(Cow::Borrowed),
            id: self.header.id.as_deref().map(Cow::Borrowed),
        }
    }

    /// Extract the stream header contents as sent by the peer.
    pub fn take_header(&mut self) -> StreamHeader<'static> {
        self.header.take()
    }
}

impl<Io: AsyncBufRead + AsyncWrite + Unpin> PendingFeaturesRecv<Io> {
    /// Receive the responder's stream features.
    ///
    /// After the stream features have been received, the stream can be used
    /// for exchanging stream-level elements (stanzas or "nonzas"). The Rust
    /// type for these elements must be given as type parameter `T`.
    ///
    /// If the peer sends a stream error instead of features, the error is
    /// returned as [`RecvFeaturesError::StreamError`].
    ///
    /// If the peer sends any payload which is neither stream features nor
    /// a stream error, an [`io::Error`][`std::io::Error`] with
    /// [`InvalidData`][`io::ErrorKind::InvalidData`] kind is returned.
    pub async fn recv_features<T: FromXml>(
        self,
    ) -> Result<(StreamFeatures, XmlStream<Io, T>), RecvFeaturesError> {
        let Self {
            mut stream,
            header: _,
        } = self;
        let features = loop {
            match ReadXso::read_from(Pin::new(&mut stream)).await {
                Ok(StreamFeaturesPayload::Features(v)) => break v,
                Ok(StreamFeaturesPayload::Error(v)) => {
                    return Err(RecvFeaturesError::StreamError(ReceivedStreamError(v)))
                }
                Err(ReadXsoError::SoftTimeout) => (),
                Err(ReadXsoError::Hard(e)) => return Err(RecvFeaturesError::Io(e)),
                Err(ReadXsoError::Parse(e)) => {
                    return Err(RecvFeaturesError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        e,
                    )))
                }
                Err(ReadXsoError::Footer) => {
                    return Err(RecvFeaturesError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected stream footer",
                    )))
                }
            }
        };
        Ok((features, XmlStream::wrap(stream)))
    }

    /// Skip receiving the responder's stream features.
    ///
    /// The stream can be used for exchanging stream-level elements (stanzas
    /// or "nonzas"). The Rust type for these elements must be given as type
    /// parameter `T`.
    ///
    /// **Note:** Using this on RFC 6120 compliant streams where stream
    /// features **are** sent after the stream header will cause a parse error
    /// down the road (because the feature stream element cannot be handled).
    /// The only place where this is useful is in
    /// [XEP-0114](https://xmpp.org/extensions/xep-0114.html) connections.
    pub fn skip_features<T: FromXml>(self) -> XmlStream<Io, T> {
        XmlStream::wrap(self.stream)
    }
}
