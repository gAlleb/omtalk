//! `starttls::ServerConfig` provides a `ServerConnector` for starttls connections

use alloc::borrow::Cow;

use tokio::{io::BufStream, net::TcpStream};

use crate::{
    connect::{ChannelBinding, DnsConfig, ServerConnector},
    xmlstream::{initiate_stream, PendingFeaturesRecv, StreamHeader, Timeouts},
    Client, Component, Error,
};

/// Component that connects over TCP
pub type TcpComponent = Component<TcpServerConnector>;

/// Client that connects over TCP
#[deprecated(since = "5.0.0", note = "use tokio_xmpp::Client instead")]
pub type TcpClient = Client;

/// Connect via insecure plaintext TCP to an XMPP server
/// This should only be used over localhost or otherwise when you know what you are doing
/// Probably mostly useful for Components
#[derive(Debug, Clone)]
pub struct TcpServerConnector(pub DnsConfig);

impl From<DnsConfig> for TcpServerConnector {
    fn from(dns_config: DnsConfig) -> TcpServerConnector {
        Self(dns_config)
    }
}

impl ServerConnector for TcpServerConnector {
    type Stream = BufStream<TcpStream>;

    async fn connect(
        &self,
        jid: &xmpp_parsers::jid::Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), Error> {
        let stream = BufStream::new(self.0.resolve().await?);
        Ok((
            initiate_stream(
                stream,
                ns,
                StreamHeader {
                    to: Some(Cow::Borrowed(jid.domain().as_str())),
                    from: None,
                    id: None,
                },
                timeouts,
            )
            .await?,
            ChannelBinding::None,
        ))
    }
}
