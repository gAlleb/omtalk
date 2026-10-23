//! The `<encrypted>` element: one payload, one `<key>` per recipient device.

use omemo::{
    DeviceId, EncryptedKey, OwnKeys, Received, Session, decrypt_payload, encrypt_payload, receive_from,
};

use crate::{Error, xml};

/// Encrypts `plaintext` once and its key for every given device. Include the sender's
/// own other devices, so the message shows up there too.
pub fn encrypt<'a>(
    sender_device: DeviceId,
    plaintext: &[u8],
    recipients: impl IntoIterator<Item = (DeviceId, &'a mut Session)>,
) -> Result<xml::Encrypted, Error> {
    let payload = encrypt_payload(plaintext)?;
    Ok(xml::Encrypted {
        header: header(sender_device, &payload.key_material, &payload.iv, recipients),
        payload: Some(xml::Payload {
            data: payload.ciphertext,
        }),
    })
}

/// An `<encrypted>` without payload. It advances the recipients' sessions without
/// showing anything: the answer that completes a key exchange, or the heartbeat that
/// keeps a long one-sided chain from going stale.
pub fn empty<'a>(
    sender_device: DeviceId,
    recipients: impl IntoIterator<Item = (DeviceId, &'a mut Session)>,
) -> Result<xml::Encrypted, Error> {
    let payload = encrypt_payload(&[])?;
    Ok(xml::Encrypted {
        header: header(sender_device, &payload.key_material, &payload.iv, recipients),
        payload: None,
    })
}

fn header<'a>(
    sender_device: DeviceId,
    key_material: &[u8],
    iv: &[u8],
    recipients: impl IntoIterator<Item = (DeviceId, &'a mut Session)>,
) -> xml::Header {
    xml::Header {
        sid: sender_device,
        keys: recipients
            .into_iter()
            .map(|(rid, session)| {
                let EncryptedKey { data, prekey } = session.encrypt(key_material);
                xml::Key { rid, prekey, data }
            })
            .collect(),
        iv: xml::IV { data: iv.to_vec() },
    }
}

pub struct Decrypted {
    pub received: Received,
    /// `None` for a message without payload (see [`empty`]).
    pub plaintext: Option<Vec<u8>>,
}

/// Decrypts a message from device `encrypted.header.sid`, whose stored session (if any)
/// is `existing`.
///
/// Store `received.session` and handle `received.used_pre_key` only after this returns
/// `Ok`. The sender's identity key still has to be checked against the known one.
pub fn decrypt(
    own: &OwnKeys,
    existing: Option<&Session>,
    encrypted: &xml::Encrypted,
) -> Result<Decrypted, Error> {
    decrypt_from(own, existing.as_slice(), encrypted)
}

/// [`decrypt`] with several sessions with the sending device, the current one first
/// (see [`omemo::receive_from`]); `received.continued` says which one went on.
pub fn decrypt_from(
    own: &OwnKeys,
    sessions: &[&Session],
    encrypted: &xml::Encrypted,
) -> Result<Decrypted, Error> {
    let mut keys = encrypted
        .header
        .keys
        .iter()
        .filter(|key| key.rid == own.device_id)
        .peekable();
    if keys.peek().is_none() {
        return Err(Error::NotForThisDevice);
    }

    // In a group, another member's device may share our id, so there may be several
    // keys for it. The one that decrypts is ours.
    let mut last_error = Error::NotForThisDevice;
    for key in keys {
        let attempt = receive_from(own, sessions, &key.data, key.prekey)
            .map_err(Error::from)
            .and_then(|received| {
                let plaintext = match &encrypted.payload {
                    Some(payload) => Some(decrypt_payload(
                        &received.key_material,
                        &encrypted.header.iv.data,
                        &payload.data,
                    )?),
                    None => None,
                };
                Ok(Decrypted { received, plaintext })
            });
        match attempt {
            Ok(decrypted) => return Ok(decrypted),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_key_with_our_device_id_is_skipped() {
        let alice_keys = OwnKeys::generate(0).unwrap();
        let bob_keys = OwnKeys::generate(0).unwrap();
        let mut session = Session::initiate(&alice_keys, &bob_keys.bundle()).unwrap();

        let mut message = encrypt(alice_keys.device_id, b"hi", [(bob_keys.device_id, &mut session)]).unwrap();
        // A colliding device of another member gets its key first.
        let mut foreign = message.header.keys[0].clone();
        foreign.data[10] ^= 0xff;
        message.header.keys.insert(0, foreign);

        let decrypted = decrypt(&bob_keys, None, &message).unwrap();
        assert_eq!(decrypted.plaintext.as_deref(), Some(&b"hi"[..]));
    }

    #[test]
    fn message_for_other_devices_only() {
        let alice_keys = OwnKeys::generate(0).unwrap();
        let bob_keys = OwnKeys::generate(0).unwrap();
        let mut session = Session::initiate(&alice_keys, &bob_keys.bundle()).unwrap();
        let message = encrypt(
            alice_keys.device_id,
            b"hi",
            [(bob_keys.device_id ^ 1, &mut session)],
        )
        .unwrap();
        assert_eq!(
            decrypt(&bob_keys, None, &message).err(),
            Some(Error::NotForThisDevice)
        );
    }
}
