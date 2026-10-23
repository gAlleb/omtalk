use xmpp_parsers::stanza_error::StanzaError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the account's connection task has stopped")]
    Stopped,
    #[error("the stanza could not be sent: {0}")]
    Send(String),
    #[error("the server or the peer answered with an error: {0:?}")]
    Stanza(Box<StanzaError>),
    #[error("no answer in time")]
    Timeout,
    /// No connection now: the stanza waits in the queue and goes when it is back.
    #[error("not sent yet: no connection; it goes when the connection is back")]
    Queued,
    /// Sent, but the server never said it got it (XEP-0198): it may be lost.
    #[error("the server did not confirm it")]
    NotConfirmed,
    #[error("unexpected answer: {0}")]
    Unexpected(String),
}
