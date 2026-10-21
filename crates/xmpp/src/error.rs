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
    #[error("unexpected answer: {0}")]
    Unexpected(String),
}
