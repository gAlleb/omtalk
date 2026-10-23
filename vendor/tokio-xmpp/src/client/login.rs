use alloc::borrow::Cow;
use core::str::FromStr;
use futures::{SinkExt, StreamExt};
use sasl::client::mechanisms::{Anonymous, Plain, Scram};
use sasl::client::Mechanism;
use sasl::common::scram::{Sha1, Sha256};
use sasl::common::Credentials;
use std::collections::BTreeSet;
use std::io;
use tokio::io::{AsyncBufRead, AsyncWrite};
use xmpp_parsers::{
    jid::Jid,
    ns,
    sasl::{Auth, Mechanism as XMPPMechanism, Nonza, Response},
    stream_features::StreamFeatures,
};

use crate::{
    connect::ServerConnector,
    error::{AuthError, Error, ProtocolError},
    xmlstream::{
        xmpp::XmppStreamElement, InitiatingStream, ReadError, StreamHeader, Timeouts, XmppStream,
    },
};

/// Run the authentication handshake on a given stream.
///
/// Uses the given `sasl_mechanisms` and `creds` to perform the full
/// authentication handshake. As authentication ends with a stream reset,
/// this returns the `stream` as [`InitiatingStream`] on success.
pub async fn auth<S: AsyncBufRead + AsyncWrite + Unpin>(
    mut stream: XmppStream<S>,
    sasl_mechanisms: BTreeSet<String>,
    creds: Credentials,
) -> Result<InitiatingStream<S>, Error> {
    let local_mechs: Vec<Box<dyn Fn() -> Box<dyn Mechanism + Send + Sync> + Send>> = vec![
        Box::new(|| Box::new(Scram::<Sha256>::from_credentials(creds.clone()).unwrap())),
        Box::new(|| Box::new(Scram::<Sha1>::from_credentials(creds.clone()).unwrap())),
        Box::new(|| Box::new(Plain::from_credentials(creds.clone()).unwrap())),
        Box::new(|| Box::new(Anonymous::new())),
    ];

    for local_mech in local_mechs {
        let mut mechanism = local_mech();
        if sasl_mechanisms.contains(mechanism.name()) {
            let initial = mechanism.initial();
            let mechanism_name =
                XMPPMechanism::from_str(mechanism.name()).map_err(ProtocolError::Parsers)?;

            stream
                .send(&XmppStreamElement::Sasl(Nonza::Auth(Auth {
                    mechanism: mechanism_name,
                    data: initial,
                })))
                .await?;

            loop {
                match stream
                    .next()
                    .await
                    .map(|v| v.map(|v| v.into_read_error()).flatten())
                {
                    Some(Ok(XmppStreamElement::Sasl(sasl))) => match sasl {
                        Nonza::Challenge(challenge) => {
                            let response = mechanism
                                .response(&challenge.data)
                                .map_err(AuthError::Sasl)?;

                            // Send response and loop
                            stream
                                .send(&XmppStreamElement::Sasl(Nonza::Response(Response {
                                    data: response,
                                })))
                                .await?;
                        }
                        Nonza::Success(_) => return Ok(stream.initiate_reset()),
                        Nonza::Failure(failure) => {
                            return Err(Error::Auth(AuthError::Fail(failure.defined_condition)));
                        }
                        _ => {
                            // Ignore?!
                        }
                    },
                    Some(Ok(el)) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "unexpected stream element during SASL negotiation: {:?}",
                                el
                            ),
                        )
                        .into())
                    }
                    Some(Err(ReadError::HardError(e))) => return Err(e.into()),
                    Some(Err(ReadError::ParseError(e))) => {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, e).into())
                    }
                    Some(Err(ReadError::SoftTimeout)) => {
                        // We cannot do anything about soft timeouts here...
                    }
                    Some(Err(ReadError::StreamFooterReceived)) | None => {
                        return Err(Error::Disconnected)
                    }
                }
            }
        }
    }

    Err(AuthError::NoMechanism.into())
}

/// Authenticate to an XMPP server, but do not bind a resource.
pub async fn client_auth<C: ServerConnector>(
    server: C,
    jid: Jid,
    password: String,
    timeouts: Timeouts,
) -> Result<(StreamFeatures, XmppStream<C::Stream>), Error> {
    let username = jid.node().unwrap().as_str();

    let (xmpp_stream, channel_binding) = server.connect(&jid, ns::JABBER_CLIENT, timeouts).await?;
    let (features, xmpp_stream) = xmpp_stream.recv_features().await?;

    let creds = Credentials::default()
        .with_username(username)
        .with_password(password)
        .with_channel_binding(channel_binding);
    // Authenticated (unspecified) stream
    let stream = auth(xmpp_stream, features.sasl_mechanisms, creds).await?;
    let stream = stream
        .send_header(StreamHeader {
            to: Some(Cow::Borrowed(jid.domain().as_str())),
            from: None,
            id: None,
        })
        .await?;
    Ok(stream.recv_features().await?)
}
