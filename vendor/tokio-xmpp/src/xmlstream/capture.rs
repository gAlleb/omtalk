// Copyright (c) 2024 Jonas Schäfer <jonas@zombofant.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Small helper struct to capture data read from an AsyncBufRead.

use core::pin::Pin;
use core::task::{Context, Poll};
use std::io::{self, IoSlice};

use futures::ready;

use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, ReadBuf};

use super::LogXsoBuf;

pin_project_lite::pin_project! {
    /// Wrapper around [`AsyncBufRead`] which stores bytes which have been
    /// read in an internal vector for later inspection.
    ///
    /// This struct implements [`AsyncRead`] and [`AsyncBufRead`] and passes
    /// read requests down to the wrapped [`AsyncBufRead`].
    ///
    /// After capturing has been enabled using [`Self::enable_capture`], any
    /// data which is read via the struct will be stored in an internal buffer
    /// and can be extracted with [`Self::take_capture`] or discarded using
    /// [`Self::discard_capture`].
    ///
    /// This can be used to log data which is being read from a source.
    ///
    /// In addition, this struct implements [`AsyncWrite`] if and only if `T`
    /// implements [`AsyncWrite`]. Writing is unaffected by capturing and is
    /// implemented solely for convenience purposes (to allow duplex usage
    /// of a wrapped I/O object).
    pub(super) struct CaptureBufRead<T> {
        #[pin]
        inner: T,
        buf: Option<(Vec<u8>, usize)>,
    }
}

impl<T> CaptureBufRead<T> {
    /// Wrap a given [`AsyncBufRead`].
    ///
    /// Note that capturing of data which is being read is disabled by default
    /// and needs to be enabled using [`Self::enable_capture`].
    pub fn wrap(inner: T) -> Self {
        Self { inner, buf: None }
    }

    /// Extract the inner [`AsyncBufRead`] and discard the capture buffer.
    pub fn into_inner(self) -> T {
        self.inner
    }

    /// Obtain a reference to the inner [`AsyncBufRead`].
    pub fn inner(&self) -> &T {
        &self.inner
    }

    /// Enable capturing of read data into the inner buffer.
    ///
    /// Any data which is read from now on will be copied into the internal
    /// buffer. That buffer will grow indefinitely until calls to
    /// [`Self::take_capture`] or [`Self::discard_capture`].
    pub fn enable_capture(&mut self) {
        self.buf = Some((Vec::new(), 0));
    }

    /// Discard the current buffer data, if any.
    ///
    /// Further data which is read will be captured again.
    pub(super) fn discard_capture(self: Pin<&mut Self>) {
        let this = self.project();
        if let Some((buf, consumed_up_to)) = this.buf.as_mut() {
            buf.drain(..*consumed_up_to);
            *consumed_up_to = 0;
        }
    }

    /// Take the currently captured data out of the inner buffer.
    ///
    /// Returns `None` unless capturing has been enabled using
    /// [`Self::enable_capture`].
    pub(super) fn take_capture(self: Pin<&mut Self>) -> Option<Vec<u8>> {
        let this = self.project();
        let (buf, consumed_up_to) = this.buf.as_mut()?;
        let result = buf.drain(..*consumed_up_to).collect();
        *consumed_up_to = 0;
        Some(result)
    }
}

impl<T: AsyncRead> AsyncRead for CaptureBufRead<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context,
        read_buf: &mut ReadBuf,
    ) -> Poll<io::Result<()>> {
        let this = self.project();
        let prev_len = read_buf.filled().len();
        let result = ready!(this.inner.poll_read(cx, read_buf));
        if let Some((buf, consumed_up_to)) = this.buf.as_mut() {
            buf.truncate(*consumed_up_to);
            buf.extend(&read_buf.filled()[prev_len..]);
            *consumed_up_to = buf.len();
        }
        Poll::Ready(result)
    }
}

impl<T: AsyncBufRead> AsyncBufRead for CaptureBufRead<T> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<&[u8]>> {
        let this = self.project();
        let result = ready!(this.inner.poll_fill_buf(cx))?;
        if let Some((buf, consumed_up_to)) = this.buf.as_mut() {
            buf.truncate(*consumed_up_to);
            buf.extend(result);
        }
        Poll::Ready(Ok(result))
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        let this = self.project();
        this.inner.consume(amt);
        if let Some((_, consumed_up_to)) = this.buf.as_mut() {
            // Increase the amount of data to preserve.
            *consumed_up_to += amt;
        }
    }
}

impl<T: AsyncWrite> AsyncWrite for CaptureBufRead<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.project().inner.poll_write(cx, buf)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().inner.poll_shutdown(cx)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().inner.poll_flush(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context,
        bufs: &[IoSlice],
    ) -> Poll<io::Result<usize>> {
        self.project().inner.poll_write_vectored(cx, bufs)
    }
}

/// Return true if logging via [`log_recv`] or [`log_send`] might be visible
/// to the user.
pub(super) fn log_enabled() -> bool {
    log::log_enabled!(log::Level::Trace)
}

/// Log received data.
///
/// `err` is an error which may be logged alongside the received data.
/// `capture` is the data which has been received and which should be logged.
/// If built with the `syntax-highlighting` feature, `capture` data will be
/// logged with XML syntax highlighting.
///
/// If both `err` and `capture` are None, nothing will be logged.
pub(super) fn log_recv(err: Option<&xmpp_parsers::Error>, capture: Option<Vec<u8>>) {
    match err {
        Some(err) => match capture {
            Some(capture) => {
                log::trace!("RECV (error: {}) {}", err, LogXsoBuf(&capture));
            }
            None => {
                log::trace!("RECV (error: {}) [data capture disabled]", err);
            }
        },
        None => {
            if let Some(capture) = capture {
                log::trace!("RECV (ok) {}", LogXsoBuf(&capture));
            }
        }
    }
}

/// Log sent data.
///
/// If built with the `syntax-highlighting` feature, `data` data will be
/// logged with XML syntax highlighting.
pub(super) fn log_send(data: &[u8]) {
    log::trace!("SEND {}", LogXsoBuf(data));
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt};

    #[tokio::test]
    async fn captures_data_read_via_async_read() {
        let mut src = &b"Hello World!"[..];
        let src = tokio::io::BufReader::new(&mut src);
        let mut src = CaptureBufRead::wrap(src);
        src.enable_capture();

        let mut dst = [0u8; 8];
        assert_eq!(src.read(&mut dst[..]).await.unwrap(), 8);
        assert_eq!(&dst, b"Hello Wo");
        assert_eq!(Pin::new(&mut src).take_capture().unwrap(), b"Hello Wo");
    }

    #[tokio::test]
    async fn captures_data_read_via_async_buf_read() {
        let mut src = &b"Hello World!"[..];
        let src = tokio::io::BufReader::new(&mut src);
        let mut src = CaptureBufRead::wrap(src);
        src.enable_capture();

        assert_eq!(src.fill_buf().await.unwrap().len(), 12);
        // We haven't consumed any bytes yet -> must return zero.
        assert_eq!(Pin::new(&mut src).take_capture().unwrap().len(), 0);

        src.consume(5);
        assert_eq!(Pin::new(&mut src).take_capture().unwrap(), b"Hello");

        src.consume(6);
        assert_eq!(Pin::new(&mut src).take_capture().unwrap(), b" World");
    }

    #[tokio::test]
    async fn discard_capture_drops_consumed_data() {
        let mut src = &b"Hello World!"[..];
        let src = tokio::io::BufReader::new(&mut src);
        let mut src = CaptureBufRead::wrap(src);
        src.enable_capture();

        assert_eq!(src.fill_buf().await.unwrap().len(), 12);
        // We haven't consumed any bytes yet -> must return zero.
        assert_eq!(Pin::new(&mut src).take_capture().unwrap().len(), 0);

        src.consume(5);
        Pin::new(&mut src).discard_capture();

        src.consume(6);
        assert_eq!(Pin::new(&mut src).take_capture().unwrap(), b" World");
    }

    #[tokio::test]
    async fn captured_data_accumulates() {
        let mut src = &b"Hello World!"[..];
        let src = tokio::io::BufReader::new(&mut src);
        let mut src = CaptureBufRead::wrap(src);
        src.enable_capture();

        assert_eq!(src.fill_buf().await.unwrap().len(), 12);
        // We haven't consumed any bytes yet -> must return zero.
        assert_eq!(Pin::new(&mut src).take_capture().unwrap().len(), 0);

        src.consume(5);
        src.consume(6);
        assert_eq!(Pin::new(&mut src).take_capture().unwrap(), b"Hello World");
    }
}
