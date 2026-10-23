// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::pin::Pin;
use std::borrow::Cow;
use std::io;

use futures::SinkExt;

use tokio::io::{AsyncBufRead, AsyncWrite};

use xmpp_parsers::{stream_error::StreamError, stream_features::StreamFeatures};

use xso::FromXml;

use super::{
    common::{RawXmlStream, StreamHeader},
    XmlStream,
};

/// Type state for a responder stream which has received a stream header
///
/// To continue stream setup, call [`send_header`][`Self::send_header`].
pub struct AcceptedStream<Io> {
    pub(super) stream: RawXmlStream<Io>,
    pub(super) header: StreamHeader<'static>,
}

impl<Io> AcceptedStream<Io> {
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

impl<Io: AsyncBufRead + AsyncWrite + Unpin> AcceptedStream<Io> {
    /// Send a stream header.
    ///
    /// Sends the given stream header to the initiator. Returns a new object
    /// which is prepared to send the stream features.
    pub async fn send_header(
        self,
        header: StreamHeader<'_>,
    ) -> io::Result<PendingFeaturesSend<Io>> {
        let Self {
            mut stream,
            header: _,
        } = self;

        header.send(Pin::new(&mut stream)).await?;
        Ok(PendingFeaturesSend { stream })
    }
}

/// Type state for a responder stream which has received and sent the stream
/// header.
///
/// To continue stream setup, call [`send_features`][`Self::send_features`].
pub struct PendingFeaturesSend<Io> {
    pub(super) stream: RawXmlStream<Io>,
}

impl<Io: AsyncBufRead + AsyncWrite + Unpin> PendingFeaturesSend<Io> {
    /// Send the responder's stream features.
    ///
    /// After the stream features have been sent, the stream can be used for
    /// exchanging stream-level elements (stanzas or "nonzas"). The Rust type
    /// for these elements must be given as type parameter `T`.
    pub async fn send_features<T: FromXml>(
        self,
        features: &'_ StreamFeatures,
    ) -> io::Result<XmlStream<Io, T>> {
        let Self { mut stream } = self;
        Pin::new(&mut stream).start_send_xso(features)?;
        stream.flush().await?;

        Ok(XmlStream::wrap(stream))
    }

    /// Send a stream error and shut the stream down.
    ///
    /// Sends the given stream error to the peer and cleanly closes the stream
    /// by sending a stream footer.
    pub async fn send_error(self, error: &'_ StreamError) -> io::Result<()> {
        let Self { mut stream } = self;
        Pin::new(&mut stream).start_send_xso(error)?;
        stream.send(xso::Item::ElementFoot).await?;
        stream.close().await?;

        Ok(())
    }
}
