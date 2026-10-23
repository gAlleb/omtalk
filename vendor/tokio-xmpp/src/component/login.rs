use std::io;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufRead, AsyncWrite};
use xmpp_parsers::{component::Handshake, jid::BareJid, ns};

use crate::component::ServerConnector;
use crate::error::{AuthError, Error};
use crate::xmlstream::{ReadError, Timeouts, XmppStream, XmppStreamElement};

/// Log into an XMPP server as a client with a jid+pass
pub async fn component_login<C: ServerConnector>(
    connector: C,
    jid: BareJid,
    password: &str,
    timeouts: Timeouts,
) -> Result<XmppStream<C::Stream>, Error> {
    let (mut stream, _) = connector.connect(&jid, ns::COMPONENT, timeouts).await?;
    let header = stream.take_header();
    let mut stream = stream.skip_features();
    let stream_id = match header.id {
        Some(id) => id.into_owned(),
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "stream id missing on component stream",
            )
            .into())
        }
    };
    auth(&mut stream, stream_id, password).await?;
    Ok(stream)
}

pub async fn auth<S: AsyncBufRead + AsyncWrite + Unpin>(
    stream: &mut XmppStream<S>,
    stream_id: String,
    password: &str,
) -> Result<(), Error> {
    let nonza = Handshake::from_stream_id_and_password(stream_id, password);
    stream
        .send(&XmppStreamElement::ComponentHandshake(nonza))
        .await?;

    loop {
        match stream
            .next()
            .await
            .map(|v| v.map(|v| v.into_read_error()).flatten())
        {
            Some(Ok(XmppStreamElement::ComponentHandshake(_))) => {
                return Ok(());
            }
            Some(Ok(_)) => {
                return Err(AuthError::ComponentFail.into());
            }
            Some(Err(ReadError::SoftTimeout)) => (),
            Some(Err(ReadError::HardError(e))) => return Err(e.into()),
            Some(Err(ReadError::ParseError(e))) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, e).into())
            }
            Some(Err(ReadError::StreamFooterReceived)) | None => return Err(Error::Disconnected),
        }
    }
}
