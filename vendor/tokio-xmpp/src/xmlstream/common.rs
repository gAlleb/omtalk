// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::borrow::Cow;
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use std::io;

use futures::{ready, Sink, SinkExt, Stream, StreamExt};

use bytes::{Buf, BytesMut};

use tokio::{
    io::{AsyncBufRead, AsyncWrite},
    time::Instant,
};

use xso::{
    exports::rxml::{
        self, writer::TrackNamespace, xml_lang::XmlLangStack, xml_ncname, Event, Namespace,
    },
    AsXml, FromEventsBuilder, FromXml, Item,
};

use crate::connect::AsyncReadAndWrite;

use super::capture::{log_enabled, log_recv, log_send, CaptureBufRead};

use xmpp_parsers::ns::STREAM as XML_STREAM_NS;

/// Configuration for timeouts on an XML stream.
///
/// The defaults are tuned toward common desktop/laptop use and may not hold
/// up to extreme conditions (arctic satellite link, mobile internet on a
/// train in Brandenburg, Germany, and similar) and may be inefficient in
/// other conditions (stable server link, localhost communication).
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// Maximum silence time before a
    /// [`ReadError::SoftTimeout`][`super::ReadError::SoftTimeout`] is
    /// returned.
    ///
    /// Soft timeouts are not fatal, but they must be handled by user code so
    /// that more data is read after at most [`Self::response_timeout`],
    /// starting from the moment the soft timeout is returned.
    pub read_timeout: Duration,

    /// Maximum silence after a soft timeout.
    ///
    /// If the stream is silent for longer than this time after a soft timeout
    /// has been emitted, a hard [`TimedOut`][`io::ErrorKind::TimedOut`]
    /// I/O error is returned and the stream is to be considered dead.
    pub response_timeout: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            read_timeout: Duration::new(300, 0),
            response_timeout: Duration::new(300, 0),
        }
    }
}

impl Timeouts {
    /// Tight timeouts suitable for communicating on a fast LAN or localhost.
    pub fn tight() -> Self {
        Self {
            read_timeout: Duration::new(60, 0),
            response_timeout: Duration::new(15, 0),
        }
    }

    fn data_to_soft(&self) -> Duration {
        self.read_timeout
    }

    fn soft_to_warn(&self) -> Duration {
        self.response_timeout / 2
    }

    fn warn_to_hard(&self) -> Duration {
        self.response_timeout / 2
    }
}

#[derive(Clone, Copy)]
enum TimeoutLevel {
    Soft,
    Warn,
    Hard,
}

#[derive(Debug)]
pub(super) enum RawError {
    Io(io::Error),
    SoftTimeout,
}

impl From<io::Error> for RawError {
    fn from(other: io::Error) -> Self {
        Self::Io(other)
    }
}

struct TimeoutState {
    /// Configuration for the timeouts.
    timeouts: Timeouts,

    /// Level of the next timeout which will trip.
    level: TimeoutLevel,

    /// Sleep timer used for read timeouts.
    // NOTE: even though we pretend we could deal with an !Unpin
    // RawXmlStream, we really can't: box_stream for example needs it,
    // but also all the typestate around the initial stream setup needs
    // to be able to move the stream around.
    deadline: Pin<Box<tokio::time::Sleep>>,
}

impl TimeoutState {
    fn new(timeouts: Timeouts) -> Self {
        Self {
            deadline: Box::pin(tokio::time::sleep(timeouts.data_to_soft())),
            level: TimeoutLevel::Soft,
            timeouts,
        }
    }

    fn poll(&mut self, cx: &mut Context) -> Poll<TimeoutLevel> {
        ready!(self.deadline.as_mut().poll(cx));
        // Deadline elapsed!
        let to_return = self.level;
        let (next_level, next_duration) = match self.level {
            TimeoutLevel::Soft => (TimeoutLevel::Warn, self.timeouts.soft_to_warn()),
            TimeoutLevel::Warn => (TimeoutLevel::Hard, self.timeouts.warn_to_hard()),
            // Something short-ish so that we fire this over and over until
            // someone finally kills the stream for good.
            TimeoutLevel::Hard => (TimeoutLevel::Hard, Duration::new(1, 0)),
        };
        self.level = next_level;
        self.deadline.as_mut().reset(Instant::now() + next_duration);
        Poll::Ready(to_return)
    }

    fn reset(&mut self) {
        self.level = TimeoutLevel::Soft;
        self.deadline
            .as_mut()
            .reset(Instant::now() + self.timeouts.data_to_soft());
    }
}

pin_project_lite::pin_project! {
    // NOTE: due to limitations of pin_project_lite, the field comments are
    // no doc comments. Luckily, this struct is only `pub(super)` anyway.
    #[project = RawXmlStreamProj]
    pub(super) struct RawXmlStream<Io> {
        // The parser used for deserialising data.
        #[pin]
        parser: rxml::AsyncReader<CaptureBufRead<Io>>,

        // Tracker for `xml:lang` data.
        lang_stack: XmlLangStack,

        // The writer used for serialising data.
        writer: rxml::writer::Encoder<rxml::writer::SimpleNamespaces>,

        timeouts: TimeoutState,

        // The default namespace to declare on the stream header.
        stream_ns: &'static str,

        // Buffer containing serialised data which will then be sent through
        // the inner `Io`. Sending that serialised data happens in
        // `poll_ready` and `poll_flush`, while appending serialised data
        // happens in `start_send`.
        tx_buffer: BytesMut,

        // Position inside tx_buffer up to which to-be-sent data has already
        // been logged.
        tx_buffer_logged: usize,

        // This signifies the limit at the point of which the Sink will
        // refuse to accept more data: if the `tx_buffer`'s size grows beyond
        // that high water mark, poll_ready will return Poll::Pending until
        // it has managed to flush enough data down the inner writer.
        //
        // Note that poll_ready will always attempt to progress the writes,
        // which further reduces the chance of hitting this limit unless
        // either the underlying writer gets stuck (e.g. TCP connection
        // breaking in a timeouty way) or a lot of data is written in bulk.
        // In both cases, the backpressure created by poll_ready returning
        // Pending is desirable.
        //
        // However, there is a catch: We don't assert this condition
        // in `start_send` at all. The reason is that we cannot suspend
        // serialisation of an XSO in the middle of writing it: it has to be
        // written in one batch or you have to start over later (this has to
        // do with the iterator state borrowing the data and futures getting
        // cancelled e.g. in tokio::select!). In order to facilitate
        // implementing a `Sink<T: AsXml>` on top of `RawXmlStream`, we
        // cannot be strict about what is going on in `start_send`:
        // `poll_ready` does not know what kind of data will be written (so
        // it could not make a size estimate, even if that was at all
        // possible with AsXml) and `start_send` is not a coroutine. So if
        // `Sink<T: AsXml>` wants to use `RawXmlStream`, it must be able to
        // submit an entire XSO's items in one batch to `RawXmlStream` after
        // it has reported to be ready once. That may easily make the buffer
        // reach its high water mark.
        //
        // So if we checked that condition in `start_send` (as opposed to
        // `poll_ready`), we would cause situations where submitting XSOs
        // failed randomly (with a panic or other errors) and would have to
        // be retried later.
        //
        // While failing with e.g. io::ErrorKind::WouldBlock is something
        // that could be investigated later, it would still require being
        // able to make an accurate estimate of the number of bytes needed to
        // serialise any given `AsXml`, because as pointed out earlier, once
        // we have started, there is no going back.
        //
        // Finally, none of that hurts much because `RawXmlStream` is only an
        // internal API. The high-level APIs will always call `poll_ready`
        // before sending an XSO, which means that we won't *grossly* go over
        // the TX buffer high water mark---unless you send a really large
        // XSO at once.
        tx_buffer_high_water_mark: usize,
    }
}

impl<Io: AsyncBufRead + AsyncWrite> RawXmlStream<Io> {
    fn new_writer(
        stream_ns: &'static str,
    ) -> rxml::writer::Encoder<rxml::writer::SimpleNamespaces> {
        let mut writer = rxml::writer::Encoder::new();
        writer
            .ns_tracker_mut()
            .declare_fixed(Some(xml_ncname!("stream")), XML_STREAM_NS.into());
        writer
            .ns_tracker_mut()
            .declare_fixed(None, stream_ns.into());
        writer
    }

    pub(super) fn new(io: Io, stream_ns: &'static str, timeouts: Timeouts) -> Self {
        let parser = rxml::Parser::default();
        let mut io = CaptureBufRead::wrap(io);
        if log_enabled() {
            io.enable_capture();
        }
        Self {
            parser: rxml::AsyncReader::wrap(io, parser),
            writer: Self::new_writer(stream_ns),
            lang_stack: XmlLangStack::new(),
            timeouts: TimeoutState::new(timeouts),
            tx_buffer_logged: 0,
            stream_ns,
            tx_buffer: BytesMut::new(),

            // This basically means: "if we already have 2 kiB in our send
            // buffer, do not accept more data".
            // Please see the extensive words at
            //`Self::tx_buffer_high_water_mark` for details.
            tx_buffer_high_water_mark: 2048,
        }
    }

    pub(super) fn reset_state(self: Pin<&mut Self>) {
        let this = self.project();
        *this.parser.parser_pinned() = rxml::Parser::default();
        *this.lang_stack = XmlLangStack::new();
        *this.writer = Self::new_writer(this.stream_ns);
    }

    pub(super) fn into_inner(self) -> Io {
        self.parser.into_inner().0.into_inner()
    }

    /// Box the underlying transport stream.
    ///
    /// This removes the specific type of the transport from the XML stream's
    /// type signature.
    pub(super) fn box_stream(self) -> RawXmlStream<Box<dyn AsyncReadAndWrite + Send + 'static>>
    where
        Io: AsyncReadAndWrite + Send + 'static,
    {
        let (io, p) = self.parser.into_inner();
        let mut io = CaptureBufRead::wrap(Box::new(io) as Box<_>);
        if log_enabled() {
            io.enable_capture();
        }
        let parser = rxml::AsyncReader::wrap(io, p);
        RawXmlStream {
            parser,
            lang_stack: self.lang_stack,
            timeouts: self.timeouts,
            writer: self.writer,
            tx_buffer: self.tx_buffer,
            tx_buffer_logged: self.tx_buffer_logged,
            tx_buffer_high_water_mark: self.tx_buffer_high_water_mark,
            stream_ns: self.stream_ns,
        }
    }
}

impl<Io: AsyncWrite> RawXmlStream<Io> {
    /// Start sending an entire XSO.
    ///
    /// Unlike the `Sink` implementation, this provides nice syntax
    /// highlighting for the serialised data in log outputs (if enabled) *and*
    /// is error safe: if the XSO fails to serialise completely, it will be as
    /// if it hadn't been attempted to serialise it at all.
    ///
    /// Note that, like with `start_send`, the caller is responsible for
    /// ensuring that the stream is ready by polling
    /// [`<Self as Sink>::poll_ready`] as needed.
    pub(super) fn start_send_xso<T: AsXml>(self: Pin<&mut Self>, xso: &T) -> io::Result<()> {
        let mut this = self.project();
        let prev_len = this.tx_buffer.len();
        match this.try_send_xso(xso) {
            Ok(()) => Ok(()),
            Err(e) => {
                let curr_len = this.tx_buffer.len();
                this.tx_buffer.truncate(prev_len);
                log::trace!(
                    "SEND failed: {}. Rewinding buffer by {} bytes.",
                    e,
                    curr_len - prev_len
                );
                Err(e)
            }
        }
    }
}

impl<Io> RawXmlStream<Io> {
    fn parser_pinned(self: Pin<&mut Self>) -> &mut rxml::Parser {
        self.project().parser.parser_pinned()
    }

    fn stream_pinned(self: Pin<&mut Self>) -> Pin<&mut CaptureBufRead<Io>> {
        self.project().parser.inner_pinned()
    }

    pub(super) fn get_stream(&self) -> &Io {
        self.parser.inner().inner()
    }
}

impl<Io: AsyncBufRead> Stream for RawXmlStream<Io> {
    type Item = Result<rxml::Event, RawError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.parser.as_mut().poll_read(cx) {
                Poll::Pending => (),
                Poll::Ready(v) => {
                    this.timeouts.reset();
                    match v.transpose() {
                        // Skip the XML declaration, nobody wants to hear about that.
                        Some(Ok(rxml::Event::XmlDeclaration(_, _))) => continue,
                        Some(Ok(event)) => {
                            this.lang_stack.handle_event(&event);
                            return Poll::Ready(Some(Ok(event)));
                        }
                        other => return Poll::Ready(other.map(|x| x.map_err(RawError::Io))),
                    }
                }
            };

            // poll_read returned pending... what do the timeouts have to say?
            match ready!(this.timeouts.poll(cx)) {
                TimeoutLevel::Soft => return Poll::Ready(Some(Err(RawError::SoftTimeout))),
                TimeoutLevel::Warn => (),
                TimeoutLevel::Hard => {
                    return Poll::Ready(Some(Err(RawError::Io(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "read and response timeouts elapsed",
                    )))))
                }
            }
        }
    }
}

impl<Io: AsyncWrite> RawXmlStreamProj<'_, Io> {
    fn flush_tx_log(&mut self) {
        let range = &self.tx_buffer[*self.tx_buffer_logged..];
        if range.is_empty() {
            return;
        }
        log_send(range);
        *self.tx_buffer_logged = self.tx_buffer.len();
    }

    fn start_send(&mut self, item: &xso::Item<'_>) -> io::Result<()> {
        self.writer
            .encode_into_bytes(item.as_rxml_item(), self.tx_buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    fn try_send_xso<T: AsXml>(&mut self, xso: &T) -> io::Result<()> {
        let iter = match xso.as_xml_iter() {
            Ok(v) => v,
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
        };
        for item in iter {
            let item = match item {
                Ok(v) => v,
                Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidInput, e)),
            };
            self.start_send(&item)?;
        }
        Ok(())
    }

    fn progress_write(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.flush_tx_log();
        while !self.tx_buffer.is_empty() {
            let written = match ready!(self
                .parser
                .as_mut()
                .inner_pinned()
                .poll_write(cx, self.tx_buffer))
            {
                Ok(v) => v,
                Err(e) => return Poll::Ready(Err(e)),
            };
            self.tx_buffer.advance(written);
            *self.tx_buffer_logged = self
                .tx_buffer_logged
                .checked_sub(written)
                .expect("Buffer arithmetic error");
        }
        Poll::Ready(Ok(()))
    }
}

impl<Io: AsyncWrite> RawXmlStream<Io> {
    /// Flush all buffered data and shut down the sender side of the
    /// underlying transport.
    ///
    /// Unlike `poll_close` (from the `Sink` impls), this will not close the
    /// receiving side of the underlying the transport. It is advisable to call
    /// `poll_close` eventually after `poll_shutdown` in order to gracefully
    /// handle situations where the remote side does not close the stream
    /// cleanly.
    pub fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.as_mut().poll_flush(cx))?;
        let this = self.project();
        this.parser.inner_pinned().poll_shutdown(cx)
    }
}

impl<'x, Io: AsyncWrite> Sink<xso::Item<'x>> for RawXmlStream<Io> {
    type Error = io::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        match this.progress_write(cx) {
            // No progress on write, but if we have enough space in the buffer
            // it's ok nonetheless.
            Poll::Pending => (),
            // Some progress and it went fine, move on.
            Poll::Ready(Ok(())) => (),
            // Something went wrong -> return the error.
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        }
        if this.tx_buffer.len() < *this.tx_buffer_high_water_mark {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        ready!(this.progress_write(cx))?;
        this.parser.as_mut().inner_pinned().poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        ready!(this.progress_write(cx))?;
        this.parser.as_mut().inner_pinned().poll_shutdown(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: xso::Item<'x>) -> Result<(), Self::Error> {
        let mut this = self.project();
        this.start_send(&item)
    }
}

/// Error returned by the [`ReadXso`] future and the [`ReadXsoState`] helper.
pub(super) enum ReadXsoError {
    /// The outer element was closed before a child element could be read.
    ///
    /// This is typically the stream footer in XML stream applications.
    Footer,

    /// A hard error occurred.
    ///
    /// This is either a real I/O error or an error from the XML parser.
    /// Neither are recoverable, because the nesting state is lost and
    /// in addition, XML errors are not recoverable because they indicate a
    /// not well-formed document.
    Hard(io::Error),

    /// The underlying stream signalled a soft read timeout before a child
    /// element could be read.
    ///
    /// Note that soft timeouts which are triggered in the middle of receiving
    /// an element are converted to hard timeouts (i.e. I/O errors).
    ///
    /// This masking is intentional, because:
    /// - Returning a [`Self::SoftTimeout`] from the middle of parsing is not
    ///   possible without complicating the API.
    /// - There is no reason why the remote side should interrupt sending data
    ///   in the middle of an element except if it or the transport has failed
    ///   fatally.
    SoftTimeout,

    /// A parse error occurred.
    ///
    /// The XML structure was well-formed, but the data contained did not
    /// match the XSO which was attempted to be parsed. This error is
    /// recoverable: when this error is emitted, the XML stream is at the same
    /// nesting level as it was before the XSO was attempted to be read; all
    /// XML structure which belonged to the XSO which failed to parse has
    /// been consumed. This allows to read more XSOs even if one fails to
    /// parse.
    Parse(xso::error::Error),
}

impl From<io::Error> for ReadXsoError {
    fn from(other: io::Error) -> Self {
        Self::Hard(other)
    }
}

impl From<xso::error::Error> for ReadXsoError {
    fn from(other: xso::error::Error) -> Self {
        Self::Parse(other)
    }
}

/// State for reading an XSO from a `Stream<Item = Result<rxml::Event, ...>>`.
///
/// Due to pinning, it is simpler to implement the statemachine in a dedicated
/// enum and let the actual (pinned) future pass the stream toward this enum's
/// function.
///
/// This is used by both [`ReadXso`] and the [`super::XmlStream`] itself.
#[derive(Default)]
pub(super) enum ReadXsoState<T: FromXml> {
    /// The [`rxml::Event::StartElement`] event was not seen yet.
    ///
    /// In this state, XML whitespace is ignored (as per RFC 6120 § 11.7), but
    /// other text data is rejected.
    #[default]
    PreData,

    /// The [`rxml::Event::StartElement`] event was received.
    ///
    /// The inner value is the builder for the "return type" of this enum and
    /// the implementation in the [`xso`] crate does all the heavy lifting:
    /// we'll only send events in its general direction.
    // We use the fallible parsing here so that we don't have to do the depth
    // accounting ourselves.
    Parsing(<Result<T, xso::error::Error> as FromXml>::Builder),

    /// The parsing has completed (successful or not).
    ///
    /// This is a final state and attempting to advance the state will panic.
    /// This is in accordance with [`core::future::Future::poll`]'s contract,
    /// for which this enum is primarily used.
    Done,
}

impl<T: FromXml> ReadXsoState<T> {
    /// Progress reading the XSO from the given source.
    ///
    /// This attempts to parse a single XSO from the underlying stream,
    /// while discarding any XML whitespace before the beginning of the XSO.
    ///
    /// If the XSO is parsed successfully, the method returns Ready with the
    /// parsed value. If parsing fails or an I/O error occurs, an appropriate
    /// error is returned.
    ///
    /// If parsing fails, the entire XML subtree belonging to the XSO is
    /// nonetheless processed. That makes parse errors recoverable: After
    /// `poll_advance` has returned Ready with either  an Ok result or a
    /// [`ReadXsoError::Parse`] error variant, another XSO can be read and the
    /// XML parsing will be at the same nesting depth as it was before the
    /// first call to `poll_advance`.
    ///
    /// Note that this guarantee does not hold for non-parse errors (i.e. for
    /// the other variants of [`ReadXsoError`]): I/O errors as well as
    /// occurrence of the outer closing element are fatal.
    ///
    /// The `source` passed to `poll_advance` should be the same on every
    /// call.
    pub(super) fn poll_advance<Io: AsyncBufRead>(
        &mut self,
        mut source: Pin<&mut RawXmlStream<Io>>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<T, ReadXsoError>> {
        loop {
            // Disable text buffering before the start event. That way, we
            // don't accumulate infinite amounts of XML whitespace caused by
            // whitespace keepalives.
            // (And also, we'll know faster when the remote side sends
            // non-whitespace garbage.)
            let text_buffering = !matches!(self, ReadXsoState::PreData);
            source
                .as_mut()
                .parser_pinned()
                .set_text_buffering(text_buffering);

            let ev = ready!(source.as_mut().poll_next(cx)).transpose();
            match self {
                ReadXsoState::PreData => {
                    log::trace!("ReadXsoState::PreData ev = {:?}", ev);
                    match ev {
                        Ok(Some(rxml::Event::XmlDeclaration(_, _))) => (),
                        Ok(Some(rxml::Event::Text(_, data))) => {
                            if xso::is_xml_whitespace(data.as_bytes()) {
                                log::trace!("Received {} bytes of whitespace", data.len());
                                source.as_mut().stream_pinned().discard_capture();
                                continue;
                            } else {
                                *self = ReadXsoState::Done;
                                return Poll::Ready(Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "non-whitespace text content before XSO",
                                )
                                .into()));
                            }
                        }
                        Ok(Some(rxml::Event::StartElement(_, name, attrs))) => {
                            let source_tmp = source.as_mut();
                            let ctx = xso::Context::empty()
                                .with_language(source_tmp.lang_stack.current());
                            *self = ReadXsoState::Parsing(
                                <Result<T, xso::error::Error> as FromXml>::from_events(
                                    name, attrs, &ctx,
                                )
                                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                            );
                        }
                        // Amounts to EOF, as we expect to start on the stream level.
                        Ok(Some(rxml::Event::EndElement(_))) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(ReadXsoError::Footer));
                        }
                        Ok(None) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "eof before XSO started",
                            )
                            .into()));
                        }
                        Err(RawError::SoftTimeout) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(ReadXsoError::SoftTimeout));
                        }
                        Err(RawError::Io(e)) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(ReadXsoError::Hard(e)));
                        }
                    }
                }
                ReadXsoState::Parsing(builder) => {
                    log::trace!("ReadXsoState::Parsing ev = {:?}", ev);
                    let ev = match ev {
                        Ok(Some(ev)) => ev,
                        Ok(None) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "eof during XSO parsing",
                            )
                            .into()));
                        }
                        Err(RawError::Io(e)) => {
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(e.into()));
                        }
                        Err(RawError::SoftTimeout) => {
                            // See also [`ReadXsoError::SoftTimeout`] for why
                            // we mask the SoftTimeout condition here.
                            *self = ReadXsoState::Done;
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "read timeout during XSO parsing",
                            )
                            .into()));
                        }
                    };

                    let source_tmp = source.as_mut();
                    let ctx = xso::Context::empty().with_language(source_tmp.lang_stack.current());
                    match builder.feed(ev, &ctx) {
                        Err(err) => {
                            *self = ReadXsoState::Done;
                            source.as_mut().stream_pinned().discard_capture();
                            return Poll::Ready(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                err,
                            )
                            .into()));
                        }
                        Ok(Some(Err(err))) => {
                            *self = ReadXsoState::Done;
                            log_recv(Some(&err), source.as_mut().stream_pinned().take_capture());
                            return Poll::Ready(Err(ReadXsoError::Parse(err)));
                        }
                        Ok(Some(Ok(value))) => {
                            *self = ReadXsoState::Done;
                            log_recv(None, source.as_mut().stream_pinned().take_capture());
                            return Poll::Ready(Ok(value));
                        }
                        Ok(None) => (),
                    }
                }

                // The error talks about "future", simply because that is
                // where `Self` is used (inside `core::future::Future::poll`).
                ReadXsoState::Done => panic!("future polled after completion"),
            }
        }
    }
}

/// Future to read a single XSO from a stream.
pub(super) struct ReadXso<'x, Io, T: FromXml> {
    /// Stream to read the future from.
    inner: Pin<&'x mut RawXmlStream<Io>>,

    /// Current state of parsing.
    state: ReadXsoState<T>,
}

impl<'x, Io: AsyncBufRead, T: FromXml> ReadXso<'x, Io, T> {
    /// Start reading a single XSO from a stream.
    pub(super) fn read_from(stream: Pin<&'x mut RawXmlStream<Io>>) -> Self {
        Self {
            inner: stream,
            state: ReadXsoState::PreData,
        }
    }
}

impl<Io: AsyncBufRead, T: FromXml> Future for ReadXso<'_, Io, T>
where
    T::Builder: Unpin,
{
    type Output = Result<T, ReadXsoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.state.poll_advance(this.inner.as_mut(), cx)
    }
}

/// Contains metadata from an XML stream header
#[derive(Default, Debug)]
pub struct StreamHeader<'x> {
    /// The optional `from` attribute.
    pub from: Option<Cow<'x, str>>,

    /// The optional `to` attribute.
    pub to: Option<Cow<'x, str>>,

    /// The optional `id` attribute.
    pub id: Option<Cow<'x, str>>,
}

impl StreamHeader<'_> {
    /// Take the contents and return them as new object.
    ///
    /// `self` will be left with all its parts set to `None`.
    pub fn take(&mut self) -> Self {
        Self {
            from: self.from.take(),
            to: self.to.take(),
            id: self.id.take(),
        }
    }

    pub(super) async fn send<Io: AsyncWrite>(
        self,
        mut stream: Pin<&mut RawXmlStream<Io>>,
    ) -> io::Result<()> {
        stream
            .as_mut()
            .start_send(Item::XmlDeclaration(rxml::XmlVersion::V1_0))?;
        stream.as_mut().start_send(Item::ElementHeadStart(
            Namespace::from(XML_STREAM_NS),
            Cow::Borrowed(xml_ncname!("stream")),
        ))?;
        if let Some(from) = self.from {
            stream.as_mut().start_send(Item::Attribute(
                Namespace::NONE,
                Cow::Borrowed(xml_ncname!("from")),
                from,
            ))?;
        }
        if let Some(to) = self.to {
            stream.as_mut().start_send(Item::Attribute(
                Namespace::NONE,
                Cow::Borrowed(xml_ncname!("to")),
                to,
            ))?;
        }
        if let Some(id) = self.id {
            stream.as_mut().start_send(Item::Attribute(
                Namespace::NONE,
                Cow::Borrowed(xml_ncname!("id")),
                id,
            ))?;
        }
        stream.as_mut().start_send(Item::Attribute(
            Namespace::NONE,
            Cow::Borrowed(xml_ncname!("version")),
            Cow::Borrowed("1.0"),
        ))?;
        stream.send(Item::ElementHeadEnd).await?;
        Ok(())
    }
}

impl StreamHeader<'static> {
    pub(super) async fn recv<Io: AsyncBufRead>(
        mut stream: Pin<&mut RawXmlStream<Io>>,
    ) -> io::Result<Self> {
        loop {
            match stream.as_mut().next().await {
                Some(Err(RawError::Io(e))) => return Err(e),
                Some(Err(RawError::SoftTimeout)) => (),
                Some(Ok(Event::StartElement(_, (ns, name), mut attrs))) => {
                    if ns != XML_STREAM_NS || name != "stream" {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unknown stream header",
                        ));
                    }

                    match attrs.remove(Namespace::none(), "version") {
                        Some(v) => {
                            if v != "1.0" {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!("unsuppored stream version: {}", v),
                                ));
                            }
                        }
                        None => {
                            if cfg!(not(feature = "component")) {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "required `version` attribute missing",
                                ));
                            }
                        }
                    }

                    let from = attrs.remove(Namespace::none(), "from");
                    let to = attrs.remove(Namespace::none(), "to");
                    let id = attrs.remove(Namespace::none(), "id");
                    let _ = attrs.remove(Namespace::xml(), "lang");

                    if let Some(((ns, name), _)) = attrs.into_iter().next() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("unexpected stream header attribute: {{{}}}{}", ns, name),
                        ));
                    }

                    return Ok(StreamHeader {
                        from: from.map(Cow::Owned),
                        to: to.map(Cow::Owned),
                        id: id.map(Cow::Owned),
                    });
                }
                Some(Ok(Event::Text(_, _))) | Some(Ok(Event::EndElement(_))) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected content before stream header",
                    ))
                }
                // We cannot loop infinitely here because the XML parser will
                // prevent more than one XML declaration from being parsed.
                Some(Ok(Event::XmlDeclaration(_, _))) => (),
                None => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "eof before stream header",
                    ))
                }
            }
        }
    }
}
