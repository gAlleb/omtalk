#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error(transparent)]
    Omemo(#[from] omemo::Error),
    #[error("malformed OMEMO element: {0}")]
    Malformed(&'static str),
    #[error("the message has no key for this device")]
    NotForThisDevice,
}
