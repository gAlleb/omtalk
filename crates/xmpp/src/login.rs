//! A single login before the connection proper. tokio-xmpp retries a failed login
//! forever, logging the reason and telling the application nothing, so a wrong password
//! would look like an endless "connecting". This tells the two apart.

use sasl::common::Credentials;
use tokio_xmpp::connect::{DnsConfig, ServerConnector, StartTlsServerConnector};
use tokio_xmpp::xmlstream::Timeouts;
use tokio_xmpp::{Error as XmppError, client_login};
use xmpp_parsers::jid::Jid;
use xmpp_parsers::ns;

use crate::{AccountConfig, Server};

pub(crate) enum Login {
    Accepted,
    /// The server refused the credentials. Retrying will not help.
    Refused(String),
    /// The server could not be reached or the stream broke; worth retrying.
    Unreachable(String),
}

pub(crate) async fn check(config: &AccountConfig) -> Login {
    let jid: Jid = config.jid.clone().into();
    let timeouts = config.timeouts();
    let result = match &config.server {
        Server::Auto => {
            let dns = DnsConfig::srv_default_client(config.jid.domain().as_str());
            attempt(
                StartTlsServerConnector::from(dns),
                &jid,
                &config.password,
                timeouts,
            )
            .await
        }
        Server::StartTls { host, port } => {
            let dns = DnsConfig::no_srv(host, *port);
            attempt(
                StartTlsServerConnector::from(dns),
                &jid,
                &config.password,
                timeouts,
            )
            .await
        }
        Server::DirectTls { host, port } => {
            let dns = DnsConfig::no_srv(host, *port);
            attempt(
                tokio_xmpp::connect::DirectTlsServerConnector::from(dns),
                &jid,
                &config.password,
                timeouts,
            )
            .await
        }
        #[cfg(feature = "insecure-tcp")]
        Server::Plaintext { addr } => {
            let dns = DnsConfig::addr(addr);
            attempt(
                tokio_xmpp::connect::TcpServerConnector::from(dns),
                &jid,
                &config.password,
                timeouts,
            )
            .await
        }
    };
    match result {
        Ok(()) => Login::Accepted,
        // A server that fails a login for now (temporary-auth-failure) or breaks off
        // (aborted) may take the same password a moment later: retried, not given up.
        Err(XmppError::Auth(tokio_xmpp::error::AuthError::Fail(
            xmpp_parsers::sasl::DefinedCondition::TemporaryAuthFailure
            | xmpp_parsers::sasl::DefinedCondition::Aborted,
        ))) => Login::Unreachable("the server could not check the password just now".into()),
        Err(XmppError::Auth(error)) => Login::Refused(error.to_string()),
        Err(error) => Login::Unreachable(error.to_string()),
    }
}

async fn attempt<C: ServerConnector>(
    connector: C,
    jid: &Jid,
    password: &str,
    timeouts: Timeouts,
) -> Result<(), XmppError> {
    let (stream, channel_binding) = connector.connect(jid, ns::JABBER_CLIENT, timeouts).await?;
    let (features, stream) = stream.recv_features().await?;
    let credentials = Credentials::default()
        .with_username(jid.node().map(|n| n.as_str()).unwrap_or_default())
        .with_password(password)
        .with_channel_binding(channel_binding);
    // Logged in; the stream is dropped, the connection proper follows.
    client_login(stream, features.sasl_mechanisms, credentials)
        .await
        .map(drop)
}
