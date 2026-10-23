// Copyright (c) 2025 Saarko <saarko@tutanota.com>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Common TLS functionality shared between direct_tls and starttls modules

use core::{error::Error as StdError, fmt};
#[cfg(feature = "ktls")]
use std::os::fd::AsRawFd;
use tokio::io::{AsyncRead, AsyncWrite};

/// Trait alias for async streams that can be used with TLS.
// When the `ktls` feature is enabled, this additionally requires `AsRawFd`.
#[cfg(feature = "ktls")]
pub trait TlsAsyncStream: AsyncRead + AsyncWrite + Unpin + AsRawFd {}
#[cfg(feature = "ktls")]
impl<T: AsyncRead + AsyncWrite + Unpin + AsRawFd> TlsAsyncStream for T {}

/// Trait alias for async streams that can be used with TLS.
#[cfg(not(feature = "ktls"))]
pub trait TlsAsyncStream: AsyncRead + AsyncWrite + Unpin {}
#[cfg(not(feature = "ktls"))]
impl<T: AsyncRead + AsyncWrite + Unpin> TlsAsyncStream for T {}

#[cfg(feature = "native-tls")]
use native_tls::Error as TlsError;
#[cfg(feature = "rustls-any-backend")]
use tokio_rustls::rustls::pki_types::InvalidDnsNameError;
#[cfg(all(feature = "rustls-any-backend", not(feature = "native-tls")))]
use tokio_rustls::rustls::Error as TlsError;

#[cfg(all(feature = "rustls-any-backend", not(feature = "native-tls")))]
use {
    alloc::sync::Arc,
    tokio_rustls::{
        rustls::pki_types::ServerName,
        rustls::{ClientConfig, RootCertStore},
        TlsConnector,
    },
};

#[cfg(all(
    feature = "rustls-any-backend",
    not(feature = "ktls"),
    not(feature = "native-tls")
))]
pub use tokio_rustls::client::TlsStream;

#[cfg(all(feature = "ktls", not(feature = "native-tls")))]
/// Tls Stream type based on Ktls
pub type TlsStream<S> = ktls::KtlsStream<S>;

#[cfg(feature = "native-tls")]
pub use tokio_native_tls::TlsStream;

#[cfg(feature = "native-tls")]
use {native_tls::TlsConnector as NativeTlsConnector, tokio_native_tls::TlsConnector};

use crate::{connect::ServerConnectorError, error::Error};
use sasl::common::ChannelBinding;

/// Common TLS error type used by both direct_tls and starttls
#[derive(Debug)]
pub enum TlsConnectorError {
    /// TLS error
    Tls(TlsError),
    #[cfg(feature = "rustls-any-backend")]
    /// DNS name parsing error
    DnsNameError(InvalidDnsNameError),
    #[cfg(feature = "ktls")]
    /// Error while setting up kernel TLS
    KtlsError(ktls::Error),
}

impl ServerConnectorError for TlsConnectorError {}

impl fmt::Display for TlsConnectorError {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Tls(e) => write!(fmt, "TLS error: {}", e),
            #[cfg(feature = "rustls-any-backend")]
            Self::DnsNameError(e) => write!(fmt, "DNS name error: {}", e),
            #[cfg(feature = "ktls")]
            Self::KtlsError(e) => write!(fmt, "Kernel TLS error: {}", e),
        }
    }
}

impl StdError for TlsConnectorError {}

impl From<TlsError> for TlsConnectorError {
    fn from(e: TlsError) -> Self {
        Self::Tls(e)
    }
}

#[cfg(feature = "rustls-any-backend")]
impl From<InvalidDnsNameError> for TlsConnectorError {
    fn from(e: InvalidDnsNameError) -> Self {
        Self::DnsNameError(e)
    }
}

/// Establish TLS connection using native-tls
#[cfg(feature = "native-tls")]
pub async fn establish_tls_connection<S: TlsAsyncStream>(
    stream: S,
    domain: &str,
) -> Result<(TlsStream<S>, ChannelBinding), Error> {
    let domain = domain.to_owned();
    let tls_stream = TlsConnector::from(NativeTlsConnector::builder().build().unwrap())
        .connect(&domain, stream)
        .await
        .map_err(|e| TlsConnectorError::Tls(e))?;
    log::warn!(
        "tls-native doesn't support channel binding, please use tls-rust if you want this feature!"
    );
    Ok((tls_stream, ChannelBinding::None))
}

/// Establish TLS connection using rustls
#[cfg(all(feature = "rustls-any-backend", not(feature = "native-tls")))]
pub async fn establish_tls_connection<S: TlsAsyncStream>(
    stream: S,
    domain: &str,
) -> Result<(TlsStream<S>, ChannelBinding), Error> {
    let domain =
        ServerName::try_from(domain.to_owned()).map_err(TlsConnectorError::DnsNameError)?;
    let mut root_store = RootCertStore::empty();

    #[cfg(feature = "webpki-roots")]
    {
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }

    #[cfg(feature = "rustls-native-certs")]
    {
        root_store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    }

    #[allow(unused_mut, reason = "This config is mutable when using ktls")]
    let mut config = ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    #[cfg(feature = "ktls")]
    let stream = {
        config.enable_secret_extraction = true;
        ktls::CorkStream::new(stream)
    };

    let tls_stream = TlsConnector::from(Arc::new(config))
        .connect(domain, stream)
        .await
        .map_err(crate::Error::Io)?;

    // Extract the channel-binding information before we hand the stream over to ktls.
    let (_, connection) = tls_stream.get_ref();
    let channel_binding = match connection.protocol_version() {
        // TODO: Add support for TLS 1.2 and earlier.
        Some(tokio_rustls::rustls::ProtocolVersion::TLSv1_3) => {
            let data = vec![0u8; 32];
            let data = connection
                .export_keying_material(data, b"EXPORTER-Channel-Binding", None)
                .map_err(TlsConnectorError::Tls)?;
            ChannelBinding::TlsExporter(data)
        }
        _ => ChannelBinding::None,
    };

    #[cfg(feature = "ktls")]
    let tls_stream = ktls::config_ktls_client(tls_stream)
        .await
        .map_err(TlsConnectorError::KtlsError)?;

    Ok((tls_stream, channel_binding))
}
