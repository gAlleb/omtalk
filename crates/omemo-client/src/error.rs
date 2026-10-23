use omemo::DeviceId;
use omtalk_xmpp::BareJid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Xmpp(#[from] omtalk_xmpp::Error),
    #[error(transparent)]
    Omemo(#[from] omemo_xml::Error),
    #[error(transparent)]
    Persist(#[from] crate::PersistError),
    #[error("{0} has no OMEMO device that can receive messages")]
    NoDevices(BareJid),
    #[error("{0} has no trusted OMEMO device: decide about its devices first")]
    NoTrustedDevices(BareJid),
    #[error("this device cannot remove itself")]
    ThisDevice,
    #[error("{nick} in {room} is not known to the room as a real account")]
    UnknownOccupant { room: BareJid, nick: String },
    /// The own device list came back, but not as a list: publishing over it would drop
    /// the account's other devices, so it is left as it is.
    #[error("the own OMEMO device list on the server cannot be read; it is left as it is")]
    UnreadableOwnList,
    #[error("device {device} of {jid} presents a different identity key than before")]
    IdentityChanged { jid: BareJid, device: DeviceId },
}

impl From<omemo::Error> for Error {
    fn from(error: omemo::Error) -> Self {
        Self::Omemo(error.into())
    }
}
