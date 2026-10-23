//! `ServerConnector` provides streams for XMPP clients

use sasl::common::ChannelBinding;
use tokio::io::{AsyncBufRead, AsyncWrite};
use xmpp_parsers::jid::Jid;

use crate::xmlstream::{PendingFeaturesRecv, Timeouts};
use crate::Error;

#[cfg(feature = "starttls")]
pub mod starttls;
#[cfg(feature = "starttls")]
pub use starttls::StartTlsServerConnector;

#[cfg(feature = "direct-tls")]
pub mod direct_tls;
#[cfg(feature = "direct-tls")]
pub use direct_tls::DirectTlsServerConnector;

#[cfg(feature = "insecure-tcp")]
pub mod tcp;
#[cfg(feature = "insecure-tcp")]
pub use tcp::TcpServerConnector;

#[cfg(any(feature = "direct-tls", feature = "starttls"))]
pub mod tls_common;

mod dns;
pub use dns::DnsConfig;

/// trait returned wrapped in XmppStream by ServerConnector
pub trait AsyncReadAndWrite: AsyncBufRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncBufRead + AsyncWrite + Unpin + Send> AsyncReadAndWrite for T {}

/// Trait that must be extended by the implementation of ServerConnector
pub trait ServerConnectorError: core::error::Error + Sync + Send {}

/// Trait called to connect to an XMPP server, perhaps called multiple times
pub trait ServerConnector: Clone + core::fmt::Debug + Send + Unpin + 'static {
    /// The type of Stream this ServerConnector produces
    type Stream: AsyncReadAndWrite;
    /// This must return the connection ready to login, ie if starttls is involved, after TLS has been started, and then after the <stream headers are exchanged
    fn connect(
        &self,
        jid: &Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> impl core::future::Future<
        Output = Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), Error>,
    > + Send;
}
