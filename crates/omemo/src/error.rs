#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("public key is not in the 0x05-prefixed Curve25519 network format")]
    InvalidPublicKey,
    #[error("signature of the signed pre key does not verify")]
    BadSignature,
    #[error("the bundle contains no pre keys")]
    NoPreKeys,
    #[error("malformed protobuf structure")]
    Protobuf,
    #[error("message version byte is not 0x33")]
    Version,
    #[error("authentication tag does not match")]
    Mac,
    #[error("ciphertext could not be decrypted")]
    Decrypt,
    #[error("message was already decrypted")]
    DuplicateMessage,
    #[error("message would skip {0} keys, more than allowed")]
    TooManySkipped(u32),
    #[error("signed pre key {0} is not known")]
    UnknownSignedPreKey(u32),
    #[error("pre key {0} is not known")]
    UnknownPreKey(u32),
    #[error("identity key of the key exchange differs from the known one")]
    IdentityMismatch,
    #[error("no session with this device")]
    NoSession,
    #[error("randomness source failed")]
    Random,
}
