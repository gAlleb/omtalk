//! Sessions between two devices: X3DH to start one, the ratchet to keep it going, and
//! the rules python-omemo and libsignal follow around key exchanges.

use serde::{Deserialize, Serialize};

use crate::keys::{Bundle, KeyPair, OwnKeys, serialize_public};
use crate::proto::{KeyExchange, Message};
use crate::ratchet::{AssociatedData, DoubleRatchet};
use crate::{Error, crypto};

const X3DH_INFO: &[u8] = b"WhisperText";
const KEY_EXCHANGE_VERSION: u8 = 0x33;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Initiation {
    /// Built by us from the peer's bundle.
    Active,
    /// Built from a key exchange the peer sent.
    Passive,
}

/// The X3DH data a session was built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct KeyExchangeIds {
    base_key: [u8; 32],
    pre_key_id: u32,
    signed_pre_key_id: u32,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    own_identity: [u8; 32],
    their_identity: [u8; 32],
    initiation: Initiation,
    /// Whether the peer has answered an actively built session. Until then every
    /// message carries the key exchange, because any of them may be the first one the
    /// peer receives.
    confirmed: bool,
    key_exchange: KeyExchangeIds,
    ratchet: DoubleRatchet,
}

/// The `<key>` element content for one recipient device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedKey {
    pub data: Vec<u8>,
    /// Value of the `prekey="true"` attribute.
    pub prekey: bool,
}

pub struct Received {
    pub session: Session,
    pub key_material: Vec<u8>,
    /// The own pre key this message consumed. Remove it from [`OwnKeys`] (or hide it
    /// until history catch-up ends) and republish the bundle.
    pub used_pre_key: Option<u32>,
    /// Which of the sessions given to [`receive_from`] the message went on in; `None`
    /// for a session it started.
    pub continued: Option<usize>,
}

impl Session {
    /// Starts a session from the peer's bundle (the active side of X3DH).
    pub fn initiate(own: &OwnKeys, bundle: &Bundle) -> Result<Self, Error> {
        bundle.verify()?;
        if bundle.pre_keys.is_empty() {
            return Err(Error::NoPreKeys);
        }
        let index = u32::from_le_bytes(crypto::random()?) as usize % bundle.pre_keys.len();
        let (&pre_key_id, pre_key) = bundle.pre_keys.iter().nth(index).unwrap();

        let ephemeral = KeyPair::generate()?;
        let secret = shared_secret(
            &own.identity.0.dh(&bundle.signed_pre_key),
            &ephemeral.dh(&bundle.identity_key),
            &ephemeral.dh(&bundle.signed_pre_key),
            &ephemeral.dh(pre_key),
        );

        Ok(Self {
            own_identity: *own.identity.public(),
            their_identity: bundle.identity_key,
            initiation: Initiation::Active,
            confirmed: false,
            key_exchange: KeyExchangeIds {
                base_key: *ephemeral.public(),
                pre_key_id,
                signed_pre_key_id: bundle.signed_pre_key_id,
            },
            ratchet: DoubleRatchet::new_active(secret, bundle.signed_pre_key)?,
        })
    }

    pub fn their_identity(&self) -> &[u8; 32] {
        &self.their_identity
    }

    pub fn initiation(&self) -> Initiation {
        self.initiation
    }

    pub fn confirmed(&self) -> bool {
        self.confirmed
    }

    /// Messages received in the current chain; python-omemo answers with an empty
    /// message once this grows large, so the peer's chain does not go stale.
    pub fn receiving_length(&self) -> u32 {
        self.ratchet.receiving_length().unwrap_or(0)
    }

    /// Encrypts key material (payload key and tag) for this device.
    pub fn encrypt(&mut self, key_material: &[u8]) -> EncryptedKey {
        let message = self
            .ratchet
            .encrypt(key_material, &ad(&self.own_identity, &self.their_identity));
        if self.initiation == Initiation::Passive || self.confirmed {
            return EncryptedKey {
                data: message,
                prekey: false,
            };
        }
        let mut data = vec![KEY_EXCHANGE_VERSION];
        data.extend(
            KeyExchange {
                pre_key_id: self.key_exchange.pre_key_id,
                base_key: self.key_exchange.base_key,
                identity_key: self.own_identity,
                message,
                signed_pre_key_id: self.key_exchange.signed_pre_key_id,
            }
            .encode(),
        );
        EncryptedKey { data, prekey: true }
    }

    fn decrypt(&mut self, message: &[u8]) -> Result<Vec<u8>, Error> {
        let key_material = self
            .ratchet
            .decrypt(message, &ad(&self.their_identity, &self.own_identity))?;
        self.confirmed = true;
        Ok(key_material)
    }
}

/// Decrypts the key material addressed to us in one `<key>` element.
///
/// `existing` is the stored session with the sending device, if any. The returned
/// session replaces it, but only store it once the payload decrypted too.
///
/// The caller still has to check the sender's identity key (`session.their_identity()`)
/// against the one it knows and trusts for that device.
pub fn receive(
    own: &OwnKeys,
    existing: Option<&Session>,
    data: &[u8],
    prekey: bool,
) -> Result<Received, Error> {
    receive_from(own, existing.as_slice(), data, prekey)
}

/// [`receive`] with several sessions with the sending device: the current one first,
/// then those it replaced, newest first. Two devices that start a session with each
/// other at once each take the other's key exchange, replacing their own; the peer
/// may go on in the one replaced, so it is tried too, as libsignal does.
pub fn receive_from(
    own: &OwnKeys,
    sessions: &[&Session],
    data: &[u8],
    prekey: bool,
) -> Result<Received, Error> {
    if !prekey {
        let mut first_error = Error::NoSession;
        for (index, existing) in sessions.iter().enumerate() {
            let mut session = (*existing).clone();
            match session.decrypt(data) {
                Ok(key_material) => {
                    return Ok(Received {
                        session,
                        key_material,
                        used_pre_key: None,
                        continued: Some(index),
                    });
                }
                Err(error) if index == 0 => first_error = error,
                Err(_) => {}
            }
        }
        return Err(first_error);
    }

    let (&version, encoded) = data.split_first().ok_or(Error::Protobuf)?;
    if version != KEY_EXCHANGE_VERSION {
        return Err(Error::Version);
    }
    let exchange = KeyExchange::decode(encoded)?;
    let ids = KeyExchangeIds {
        base_key: exchange.base_key,
        pre_key_id: exchange.pre_key_id,
        signed_pre_key_id: exchange.signed_pre_key_id,
    };

    // The peer repeats the key exchange until we answer. A repeat decrypts in the
    // session it built; anything else starts over, as python-omemo does.
    if let Some((index, session)) = sessions.iter().enumerate().find(|(_, s)| {
        s.initiation == Initiation::Passive
            && s.key_exchange == ids
            && s.their_identity == exchange.identity_key
    }) {
        let mut session = (*session).clone();
        let key_material = session.decrypt(&exchange.message)?;
        return Ok(Received {
            session,
            key_material,
            used_pre_key: None,
            continued: Some(index),
        });
    }

    let signed_pre_key = own.signed_pre_key_pair(exchange.signed_pre_key_id)?;
    let pre_key = own.pre_key_pair(exchange.pre_key_id)?;
    let secret = shared_secret(
        &signed_pre_key.dh(&exchange.identity_key),
        &own.identity.0.dh(&exchange.base_key),
        &signed_pre_key.dh(&exchange.base_key),
        &pre_key.dh(&exchange.base_key),
    );
    let their_ratchet = ratchet_key_of(&exchange.message)?;

    let mut session = Session {
        own_identity: *own.identity.public(),
        their_identity: exchange.identity_key,
        initiation: Initiation::Passive,
        confirmed: false,
        key_exchange: ids,
        ratchet: DoubleRatchet::new_passive(secret, signed_pre_key.clone(), their_ratchet)?,
    };
    let key_material = session.decrypt(&exchange.message)?;
    Ok(Received {
        session,
        key_material,
        used_pre_key: Some(exchange.pre_key_id),
        continued: None,
    })
}

fn shared_secret(dh1: &[u8; 32], dh2: &[u8; 32], dh3: &[u8; 32], dh4: &[u8; 32]) -> [u8; 32] {
    let mut input = Vec::with_capacity(32 * 5);
    input.extend_from_slice(&[0xff; 32]);
    for dh in [dh1, dh2, dh3, dh4] {
        input.extend_from_slice(dh);
    }
    crypto::hkdf(&[0u8; 32], &input, X3DH_INFO)
}

fn ad(sender: &[u8; 32], recipient: &[u8; 32]) -> AssociatedData {
    let mut out = [0u8; 66];
    out[..33].copy_from_slice(&serialize_public(sender));
    out[33..].copy_from_slice(&serialize_public(recipient));
    out
}

fn ratchet_key_of(authenticated: &[u8]) -> Result<[u8; 32], Error> {
    // version byte, SignalMessage, 8-byte MAC
    let inner = authenticated
        .get(1..authenticated.len().saturating_sub(8))
        .ok_or(Error::Protobuf)?;
    Ok(Message::decode(inner)?.ratchet_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_repeats_then_confirms() {
        let alice_keys = OwnKeys::generate(0).unwrap();
        let mut bob_keys = OwnKeys::generate(0).unwrap();
        let mut alice = Session::initiate(&alice_keys, &bob_keys.bundle()).unwrap();

        let first = alice.encrypt(b"k1");
        let second = alice.encrypt(b"k2");
        assert!(first.prekey && second.prekey);

        let received = receive(&bob_keys, None, &first.data, true).unwrap();
        assert_eq!(received.key_material, b"k1");
        let used = received.used_pre_key.unwrap();
        assert!(bob_keys.remove_pre_key(used));
        let mut bob = received.session;

        // The repeated key exchange names a pre key that is gone now; it must still
        // decrypt, in the session the first one built.
        let received = receive(&bob_keys, Some(&bob), &second.data, true).unwrap();
        assert_eq!(received.key_material, b"k2");
        assert_eq!(received.used_pre_key, None);
        bob = received.session;

        let reply = bob.encrypt(b"r1");
        assert!(!reply.prekey);
        alice = receive(&alice_keys, Some(&alice), &reply.data, false)
            .unwrap()
            .session;
        assert!(alice.confirmed());
        assert!(!alice.encrypt(b"k3").prekey);
    }

    #[test]
    fn unknown_pre_key_without_session_fails() {
        let alice_keys = OwnKeys::generate(0).unwrap();
        let mut bob_keys = OwnKeys::generate(0).unwrap();
        let mut alice = Session::initiate(&alice_keys, &bob_keys.bundle()).unwrap();
        let message = alice.encrypt(b"k");
        let exchange = KeyExchange::decode(&message.data[1..]).unwrap();
        bob_keys.remove_pre_key(exchange.pre_key_id);
        assert_eq!(
            receive(&bob_keys, None, &message.data, true).err(),
            Some(Error::UnknownPreKey(exchange.pre_key_id))
        );
    }
}
