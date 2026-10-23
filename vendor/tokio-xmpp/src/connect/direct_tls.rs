// Copyright (c) 2025 Saarko <saarko@tutanota.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! `direct_tls::ServerConfig` provides a `ServerConnector` for direct TLS connections

use alloc::borrow::Cow;
use sasl::common::ChannelBinding;
use tokio::{io::BufStream, net::TcpStream};
use xmpp_parsers::jid::Jid;

use crate::{
    connect::{
        tls_common::{establish_tls_connection, TlsConnectorError, TlsStream},
        DnsConfig, ServerConnector,
    },
    error::Error,
    xmlstream::{initiate_stream, PendingFeaturesRecv, StreamHeader, Timeouts},
};

/// Connect via direct TLS to an XMPP server
#[derive(Debug, Clone)]
pub struct DirectTlsServerConnector(pub DnsConfig);

impl From<DnsConfig> for DirectTlsServerConnector {
    fn from(dns_config: DnsConfig) -> DirectTlsServerConnector {
        Self(dns_config)
    }
}

impl ServerConnector for DirectTlsServerConnector {
    type Stream = BufStream<TlsStream<TcpStream>>;

    async fn connect(
        &self,
        jid: &Jid,
        ns: &'static str,
        timeouts: Timeouts,
    ) -> Result<(PendingFeaturesRecv<Self::Stream>, ChannelBinding), Error> {
        let tcp_stream = self.0.resolve().await?;

        // Immediately establish TLS connection
        let (tls_stream, channel_binding) =
            establish_tls_connection(tcp_stream, jid.domain().as_str()).await?;

        // Establish XMPP stream over TLS
        Ok((
            initiate_stream(
                tokio::io::BufStream::new(tls_stream),
                ns,
                StreamHeader {
                    to: Some(Cow::Borrowed(jid.domain().as_str())),
                    // Setting explicitly `from` here, because server require it
                    // in order to advertise i.e. SASL2 (XEP-0388).
                    from: Some(Cow::Borrowed(jid.to_bare().as_str())),
                    id: None,
                },
                timeouts,
            )
            .await?,
            channel_binding,
        ))
    }
}

/// Direct TLS ServerConnector Error - now just an alias to the common error type
pub type DirectTlsError = TlsConnectorError;
