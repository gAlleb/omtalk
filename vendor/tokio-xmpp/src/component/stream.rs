//! Components in XMPP are services/gateways that are logged into an
//! XMPP server under a JID consisting of just a domain name. They are
//! allowed to use any user and resource identifiers in their stanzas.
use futures::{task::Poll, Sink, Stream};
use std::pin::Pin;
use std::task::Context;

use crate::{
    component::Component,
    connect::ServerConnector,
    xmlstream::{XmppStream, XmppStreamElement},
    Error, Stanza,
};

impl<C: ServerConnector> Stream for Component<C> {
    type Item = Stanza;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Ready(v) => match v.map(|v| v.map(|v| v.into_read_error()).flatten()) {
                    Some(Ok(XmppStreamElement::Stanza(stanza))) => {
                        return Poll::Ready(Some(stanza))
                    }
                    Some(Ok(_)) =>
                    // unexpected
                    {
                        return Poll::Ready(None)
                    }
                    Some(Err(_)) => return Poll::Ready(None),
                    None => return Poll::Ready(None),
                },
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<C: ServerConnector> Sink<Stanza> for Component<C> {
    type Error = Error;

    fn start_send(mut self: Pin<&mut Self>, item: Stanza) -> Result<(), Self::Error> {
        Pin::new(&mut self.stream)
            .start_send(&item)
            .map_err(|e| e.into())
    }

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        <XmppStream<C::Stream> as Sink<&XmppStreamElement>>::poll_ready(
            Pin::new(&mut self.stream),
            cx,
        )
        .map_err(|e| e.into())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        <XmppStream<C::Stream> as Sink<&XmppStreamElement>>::poll_flush(
            Pin::new(&mut self.stream),
            cx,
        )
        .map_err(|e| e.into())
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<(), Self::Error>> {
        <XmppStream<C::Stream> as Sink<&XmppStreamElement>>::poll_close(
            Pin::new(&mut self.stream),
            cx,
        )
        .map_err(|e| e.into())
    }
}
