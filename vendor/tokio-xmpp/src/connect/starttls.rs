//! `starttls::ServerConfig` provides a `ServerConnector` for starttls connections

use alloc::borrow::Cow;
use std::io;

use futures::{sink::SinkExt, stream::StreamExt};
use sasl::common::ChannelBinding;
use tokio::{io::BufStream, net::TcpStream};
use xmpp_parsers::{
    jid::Jid,
    starttls::{self, Request},
};

use crate::{
    connect::{
        tls_common::{establish_tls_connection, TlsAsyncStream, TlsConnectorError, TlsStream},
        DnsConfig, ServerConnector,
    },
    error::{Error, ProtocolError},
    xmlstream::{
        initiate_stream, PendingFeaturesRecv, ReadError, StreamHeader, Timeouts, XmppStream,
        XmppStreamElement,
    },
    Client,
};

/// Client that connects over StartTls
#[deprecated(since = "5.0.0", note = "use tokio_xmpp::Client instead")]
pub type StartTlsClient = Client;

/// Connect via TCP+StartTLS to an XMPP server
#[derive(Debug, Clone)]
pub struct StartTlsServerConnector(pub DnsConfig);

impl From<DnsConfig> for StartTlsServerConnector {
    fn from(dns_config: DnsConfig) -> StartTlsServerConnector {
        Self(dns_config)
    }
}

impl ServerConnector for StartTlsServerConnector {
    type Stream = BufStream<TlsStream<TcpStream>>;

    async fn connect(
        &self,
        jid: &Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), Error> {
        let tcp_stream = tokio::io::BufStream::new(self.0.resolve().await?);

        // Unencryped XmppStream
        let xmpp_stream = initiate_stream(
            tcp_stream,
            ns,
            StreamHeader {
                to: Some(Cow::Borrowed(jid.domain().as_str())),
                from: None,
                id: None,
            },
            timeouts,
        )
        .await?;
        let (features, xmpp_stream) = xmpp_stream.recv_features().await?;

        if features.can_starttls() {
            // TlsStream
            let (tls_stream, channel_binding) =
                starttls(xmpp_stream, jid.domain().as_str()).await?;
            // Encrypted XmppStream
            Ok((
                initiate_stream(
                    tokio::io::BufStream::new(tls_stream),
                    ns,
                    StreamHeader {
                        to: Some(Cow::Borrowed(jid.domain().as_str())),
                        from: None,
                        id: None,
                    },
                    timeouts,
                )
                .await?,
                channel_binding,
            ))
        } else {
            Err(crate::Error::Protocol(ProtocolError::NoTls))
        }
    }
}

/// Performs `<starttls/>` on an XmppStream and returns a binary
/// TlsStream.
pub async fn starttls<S: TlsAsyncStream>(
    mut stream: XmppStream<BufStream<S>>,
    domain: &str,
) -> Result<(TlsStream<S>, ChannelBinding), Error> {
    stream
        .send(&XmppStreamElement::Starttls(starttls::Nonza::Request(
            Request,
        )))
        .await?;

    loop {
        match stream
            .next()
            .await
            .map(|v| v.map(|v| v.into_read_error()).flatten())
        {
            Some(Ok(XmppStreamElement::Starttls(starttls::Nonza::Proceed(_)))) => {
                break;
            }
            Some(Ok(_)) => (),
            Some(Err(ReadError::SoftTimeout)) => (),
            Some(Err(ReadError::HardError(e))) => return Err(e.into()),
            Some(Err(ReadError::ParseError(e))) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e).into())
            }
            None | Some(Err(ReadError::StreamFooterReceived)) => {
                return Err(crate::Error::Disconnected)
            }
        }
    }

    let inner_stream = stream.into_inner().into_inner();
    establish_tls_connection(inner_stream, domain).await
}

/// StartTLS ServerConnector Error - now just an alias to the common error type
pub type StartTlsError = TlsConnectorError;
