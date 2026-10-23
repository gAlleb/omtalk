//! Components in XMPP are services/gateways that are logged into an
//! XMPP server under a JID consisting of just a domain name. They are
//! allowed to use any user and resource identifiers in their stanzas.
use futures::sink::SinkExt;
use std::str::FromStr;
use xmpp_parsers::jid::BareJid;

use crate::{
    component::login::component_login,
    connect::ServerConnector,
    event::ensure_stanza_id,
    xmlstream::{Timeouts, XmppStream},
    Error, Stanza,
};

#[cfg(any(feature = "starttls", feature = "insecure-tcp"))]
use crate::connect::DnsConfig;
#[cfg(feature = "insecure-tcp")]
use crate::connect::TcpServerConnector;

mod login;
mod stream;

/// Component connection to an XMPP server
///
/// This simplifies the `XmppStream` to a `Stream`/`Sink` of `Element`
/// (stanzas). Connection handling however is up to the user.
pub struct Component<C: ServerConnector> {
    /// The component's Jabber-Id
    pub jid: BareJid,
    stream: XmppStream<C::Stream>,
}

impl<C: ServerConnector> Component<C> {
    /// Send stanza
    pub async fn send_stanza(&mut self, mut stanza: Stanza) -> Result<(), Error> {
        ensure_stanza_id(&mut stanza);
        self.send(stanza).await
    }

    /// End connection
    pub async fn send_end(&mut self) -> Result<(), Error> {
        self.close().await
    }
}

#[cfg(feature = "insecure-tcp")]
impl Component<TcpServerConnector> {
    /// Start a new XMPP component over plaintext TCP to localhost:5347
    #[cfg(feature = "insecure-tcp")]
    pub async fn new(jid: &str, password: &str) -> Result<Self, Error> {
        Self::new_plaintext(
            jid,
            password,
            DnsConfig::addr("127.0.0.1:5347"),
            Timeouts::tight(),
        )
        .await
    }

    /// Start a new XMPP component over plaintext TCP
    #[cfg(feature = "insecure-tcp")]
    pub async fn new_plaintext(
        jid: &str,
        password: &str,
        dns_config: DnsConfig,
        timeouts: Timeouts,
    ) -> Result<Self, Error> {
        Component::new_with_connector(
            jid,
            password,
            TcpServerConnector::from(dns_config),
            timeouts,
        )
        .await
    }
}

impl<C: ServerConnector> Component<C> {
    /// Start a new XMPP component.
    ///
    /// Unfortunately [`StartTlsConnector`](crate::connect::StartTlsServerConnector) is not supported yet.
    /// The tracking issue is [#143](https://gitlab.com/xmpp-rs/xmpp-rs/-/issues/143).
    pub async fn new_with_connector(
        jid: &str,
        password: &str,
        connector: C,
        timeouts: Timeouts,
    ) -> Result<Self, Error> {
        let jid = BareJid::from_str(jid)?;
        let stream = component_login(connector, jid.clone(), password, timeouts).await?;
        Ok(Component { jid, stream })
    }
}
