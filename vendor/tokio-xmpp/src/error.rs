use core::{fmt, net::AddrParseError, str::Utf8Error};
#[cfg(feature = "dns")]
use hickory_resolver::{net::NetError as DnsNetError, proto::ProtoError as DnsProtoError};
use sasl::client::MechanismError as SaslMechanismError;
use std::io;
use thiserror::Error;

use xmpp_parsers::stream_error::ReceivedStreamError;

use crate::{
    connect::ServerConnectorError, jid, minidom,
    parsers::sasl::DefinedCondition as SaslDefinedCondition, xmlstream::RecvFeaturesError,
};

/// Top-level error type
#[derive(Debug, Error)]
pub enum Error {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    /// Error parsing Jabber-Id
    #[error("JID parse error: {0}")]
    JidParse(#[from] jid::Error),
    /// Protocol-level error
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// Authentication error
    #[error("authentication error: {0}")]
    Auth(#[from] AuthError),
    /// Connection closed
    #[error("disconnected")]
    Disconnected,
    /// Should never happen
    #[error("invalid state")]
    InvalidState,
    /// Fmt error
    #[error("fmt error: {0}")]
    Fmt(#[from] fmt::Error),
    /// Utf8 error
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] Utf8Error),
    /// Error specific to ServerConnector impl
    #[error("connection error: {0}")]
    Connection(Box<dyn ServerConnectorError>),
    /// DNS protocol error
    #[cfg(feature = "dns")]
    #[error("{0:?}")]
    DnsProto(#[from] DnsProtoError),
    /// DNS network error
    #[cfg(feature = "dns")]
    #[error("{0:?}")]
    DnsNet(#[from] DnsNetError),
    /// DNS label conversion error, no details available from module
    /// `idna`
    #[cfg(feature = "dns")]
    #[error("IDNA error")]
    Idna,
    /// Invalid IP/Port address
    #[error("wrong network address: {0}")]
    Addr(#[from] AddrParseError),
    /// Received a stream error
    #[error("{0}")]
    StreamError(ReceivedStreamError),
}

impl<T: ServerConnectorError + 'static> From<T> for Error {
    fn from(e: T) -> Self {
        Error::Connection(Box::new(e))
    }
}

#[cfg(feature = "dns")]
impl From<idna::Errors> for Error {
    fn from(_e: idna::Errors) -> Self {
        Error::Idna
    }
}

impl From<RecvFeaturesError> for Error {
    fn from(e: RecvFeaturesError) -> Self {
        match e {
            RecvFeaturesError::Io(e) => e.into(),
            RecvFeaturesError::StreamError(e) => Self::StreamError(e),
        }
    }
}

/// XMPP protocol-level error
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// XML parser error
    #[error("XML parser error: {0}")]
    Parser(#[from] minidom::Error),
    /// Error with expected stanza schema
    #[error("error with expected stanza schema: {0}")]
    Parsers(#[from] xso::error::Error),
    /// No TLS available
    #[error("no TLS available")]
    NoTls,
    /// Invalid response to resource binding
    #[error("invalid response to resource binding")]
    InvalidBindResponse,
    /// No xmlns attribute in <stream:stream>
    #[error("no xmlns attribute in <stream:stream>")]
    NoStreamNamespace,
    /// No id attribute in <stream:stream>
    #[error("no id attribute in <stream:stream>")]
    NoStreamId,
    /// Encountered an unexpected XML token
    #[error("encountered an unexpected XML token")]
    InvalidToken,
    /// Unexpected <stream:stream> (shouldn't occur)
    #[error("unexpected <stream:stream>")]
    InvalidStreamStart,
}

/// Authentication error
#[derive(Debug, Error)]
pub enum AuthError {
    /// No matching SASL mechanism available
    #[error("no matching SASL mechanism available")]
    NoMechanism,
    /// Local SASL implementation error
    #[error("local SASL implementation error: {0}")]
    Sasl(SaslMechanismError),
    /// Failure from server
    #[error("failure from the server: {0:?}")]
    Fail(SaslDefinedCondition),
    /// Component authentication failure
    #[error("component authentication failure")]
    ComponentFail,
}
