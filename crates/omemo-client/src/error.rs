use omemo::DeviceId;
use omtalk_xmpp::BareJid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Xmpp(#[from] omtalk_xmpp::Error),
    #[error(transparent)]
    Omemo(#[from] omemo_xml::Error),
    #[error("{0} has no OMEMO device that can receive messages")]
    NoDevices(BareJid),
    #[error("{nick} in {room} is not known to the room as a real account")]
    UnknownOccupant { room: BareJid, nick: String },
    #[error("device {device} of {jid} presents a different identity key than before")]
    IdentityChanged { jid: BareJid, device: DeviceId },
}

impl From<omemo::Error> for Error {
    fn from(error: omemo::Error) -> Self {
        Self::Omemo(error.into())
    }
}
