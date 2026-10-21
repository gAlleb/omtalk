//! The Double Ratchet with libsignal's KDFs and message encryption.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::keys::KeyPair;
use crate::proto::Message;
use crate::{Error, crypto};

const ROOT_INFO: &[u8] = b"WhisperRatchet";
const MESSAGE_KEYS_INFO: &[u8] = b"WhisperMessageKeys";
const MESSAGE_VERSION: u8 = 0x33;
const MAC_LENGTH: usize = 8;

/// Most message keys kept for out-of-order messages, and most a single message may
/// skip. python-omemo uses 1000 for both.
pub const MAX_SKIPPED: usize = 1000;

/// Two serialized identity keys, sender first: what libsignal authenticates every
/// message with.
pub type AssociatedData = [u8; 66];

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct Chain {
    key: [u8; 32],
    length: u32,
}

impl Chain {
    fn new(key: [u8; 32]) -> Self {
        Self { key, length: 0 }
    }

    fn next_message_key(&mut self) -> [u8; 32] {
        let message_key = crypto::hmac(&self.key, &[&[0x01]]);
        self.key = crypto::hmac(&self.key, &[&[0x02]]);
        self.length += 1;
        message_key
    }
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SkippedKey {
    ratchet_key: [u8; 32],
    counter: u32,
    message_key: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DoubleRatchet {
    root_key: [u8; 32],
    own_ratchet: KeyPair,
    their_ratchet: [u8; 32],
    sending: Chain,
    receiving: Option<Chain>,
    previous_sending_length: u32,
    /// Oldest first.
    skipped: Vec<SkippedKey>,
}

impl Drop for DoubleRatchet {
    fn drop(&mut self) {
        self.root_key.zeroize();
    }
}

fn root_step(root_key: &mut [u8; 32], dh: &[u8; 32]) -> Chain {
    let out: [u8; 64] = crypto::hkdf(root_key, dh, ROOT_INFO);
    root_key.copy_from_slice(&out[..32]);
    Chain::new(out[32..].try_into().unwrap())
}

impl DoubleRatchet {
    /// The side that built the session from a bundle. The X3DH secret is the root key
    /// and the peer's signed pre key is its first ratchet key.
    pub fn new_active(shared_secret: [u8; 32], their_signed_pre_key: [u8; 32]) -> Result<Self, Error> {
        let mut root_key = shared_secret;
        let own_ratchet = KeyPair::generate()?;
        let sending = root_step(&mut root_key, &own_ratchet.dh(&their_signed_pre_key));
        Ok(Self {
            root_key,
            own_ratchet,
            their_ratchet: their_signed_pre_key,
            sending,
            receiving: None,
            previous_sending_length: 0,
            skipped: Vec::new(),
        })
    }

    /// The side that received the key exchange; its signed pre key is its first ratchet key.
    pub fn new_passive(
        shared_secret: [u8; 32],
        own_signed_pre_key: KeyPair,
        their_ratchet: [u8; 32],
    ) -> Result<Self, Error> {
        let mut root_key = shared_secret;
        let receiving = root_step(&mut root_key, &own_signed_pre_key.dh(&their_ratchet));
        let own_ratchet = KeyPair::generate()?;
        let sending = root_step(&mut root_key, &own_ratchet.dh(&their_ratchet));
        Ok(Self {
            root_key,
            own_ratchet,
            their_ratchet,
            sending,
            receiving: Some(receiving),
            previous_sending_length: 0,
            skipped: Vec::new(),
        })
    }

    pub fn receiving_length(&self) -> Option<u32> {
        self.receiving.as_ref().map(|chain| chain.length)
    }

    /// Returns a serialized authenticated message: version byte, `SignalMessage`, MAC.
    pub fn encrypt(&mut self, plaintext: &[u8], ad: &AssociatedData) -> Vec<u8> {
        let ratchet_key = *self.own_ratchet.public();
        let counter = self.sending.length;
        let message_key = self.sending.next_message_key();
        let (cipher_key, mac_key, iv) = message_keys(&message_key);

        let mut out = vec![MESSAGE_VERSION];
        out.extend(
            Message {
                ratchet_key,
                counter,
                previous_counter: self.previous_sending_length,
                ciphertext: crypto::aes256_cbc_encrypt(&cipher_key, &iv, plaintext),
            }
            .encode(),
        );
        let mac = crypto::hmac(&mac_key, &[ad, &out]);
        out.extend_from_slice(&mac[..MAC_LENGTH]);
        out
    }

    /// Decrypts an authenticated message. The state changes only if decryption succeeds,
    /// so a forged or broken message cannot desynchronize the session.
    pub fn decrypt(&mut self, data: &[u8], ad: &AssociatedData) -> Result<Vec<u8>, Error> {
        if data.len() < 1 + MAC_LENGTH {
            return Err(Error::Protobuf);
        }
        let (body, mac) = data.split_at(data.len() - MAC_LENGTH);
        let (&version, encoded) = body.split_first().unwrap();
        if version != MESSAGE_VERSION {
            return Err(Error::Version);
        }
        let message = Message::decode(encoded)?;

        if let Some(index) = self
            .skipped
            .iter()
            .position(|key| key.ratchet_key == message.ratchet_key && key.counter == message.counter)
        {
            let plaintext = open(&self.skipped[index].message_key, body, mac, ad, &message)?;
            self.skipped.remove(index);
            return Ok(plaintext);
        }

        let mut next = self.clone();
        let message_key = next.advance_receiving(&message)?;
        let plaintext = open(&message_key, body, mac, ad, &message)?;
        let overflow = next.skipped.len().saturating_sub(MAX_SKIPPED);
        next.skipped.drain(..overflow);
        *self = next;
        Ok(plaintext)
    }

    fn advance_receiving(&mut self, message: &Message) -> Result<[u8; 32], Error> {
        if message.ratchet_key != self.their_ratchet {
            if let Some(receiving) = self.receiving.as_mut() {
                // Keys left over in the old chain are kept, unless there are suspiciously
                // many of them; then they are dropped rather than computed.
                let missing = message.previous_counter.saturating_sub(receiving.length);
                if missing as usize <= MAX_SKIPPED {
                    skip(
                        &mut self.skipped,
                        receiving,
                        self.their_ratchet,
                        message.previous_counter,
                    );
                }
            }
            self.their_ratchet = message.ratchet_key;
            self.receiving = Some(root_step(
                &mut self.root_key,
                &self.own_ratchet.dh(&self.their_ratchet),
            ));
            self.own_ratchet = KeyPair::generate()?;
            self.previous_sending_length = self.sending.length;
            self.sending = root_step(&mut self.root_key, &self.own_ratchet.dh(&self.their_ratchet));
        }

        let receiving = self.receiving.as_mut().ok_or(Error::NoSession)?;
        let missing = message.counter.saturating_sub(receiving.length);
        if missing as usize > MAX_SKIPPED {
            return Err(Error::TooManySkipped(missing));
        }
        skip(&mut self.skipped, receiving, self.their_ratchet, message.counter);
        if message.counter < receiving.length {
            return Err(Error::DuplicateMessage);
        }
        Ok(receiving.next_message_key())
    }
}

fn skip(skipped: &mut Vec<SkippedKey>, chain: &mut Chain, ratchet_key: [u8; 32], until: u32) {
    while chain.length < until {
        let counter = chain.length;
        let message_key = chain.next_message_key();
        skipped.push(SkippedKey {
            ratchet_key,
            counter,
            message_key,
        });
    }
}

fn message_keys(message_key: &[u8; 32]) -> ([u8; 32], [u8; 32], [u8; 16]) {
    let out: [u8; 80] = crypto::hkdf(&[0u8; 32], message_key, MESSAGE_KEYS_INFO);
    (
        out[..32].try_into().unwrap(),
        out[32..64].try_into().unwrap(),
        out[64..].try_into().unwrap(),
    )
}

fn open(
    message_key: &[u8; 32],
    body: &[u8],
    mac: &[u8],
    ad: &AssociatedData,
    message: &Message,
) -> Result<Vec<u8>, Error> {
    let (cipher_key, mac_key, iv) = message_keys(message_key);
    let expected = crypto::hmac(&mac_key, &[ad, body]);
    if !constant_time_eq(&expected[..MAC_LENGTH], mac) {
        return Err(Error::Mac);
    }
    crypto::aes256_cbc_decrypt(&cipher_key, &iv, &message.ciphertext)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (DoubleRatchet, DoubleRatchet) {
        let secret = [42u8; 32];
        let bob_spk = KeyPair::generate().unwrap();
        let alice = DoubleRatchet::new_active(secret, *bob_spk.public()).unwrap();
        // Bob learns Alice's ratchet key from her first message; take it directly here.
        let bob = DoubleRatchet::new_passive(secret, bob_spk, *alice.own_ratchet.public()).unwrap();
        (alice, bob)
    }

    const AD: AssociatedData = [1u8; 66];

    #[test]
    fn out_of_order_and_replies() {
        let (mut alice, mut bob) = pair();
        let messages: Vec<_> = (0..5).map(|i| alice.encrypt(&[i], &AD)).collect();
        for i in [3, 0, 4, 1, 2] {
            assert_eq!(bob.decrypt(&messages[i], &AD).unwrap(), [i as u8]);
        }
        let reply = bob.encrypt(b"reply", &AD);
        assert_eq!(alice.decrypt(&reply, &AD).unwrap(), b"reply");
    }

    #[test]
    fn duplicate_and_forgery_leave_state_intact() {
        let (mut alice, mut bob) = pair();
        let first = alice.encrypt(b"one", &AD);
        bob.decrypt(&first, &AD).unwrap();
        assert_eq!(bob.decrypt(&first, &AD), Err(Error::DuplicateMessage));

        let mut forged = alice.encrypt(b"two", &AD);
        *forged.last_mut().unwrap() ^= 1;
        assert_eq!(bob.decrypt(&forged, &AD), Err(Error::Mac));
        *forged.last_mut().unwrap() ^= 1;
        assert_eq!(bob.decrypt(&forged, &AD).unwrap(), b"two");
    }

    #[test]
    fn wrong_associated_data_fails() {
        let (mut alice, mut bob) = pair();
        let message = alice.encrypt(b"x", &AD);
        assert_eq!(bob.decrypt(&message, &[2u8; 66]), Err(Error::Mac));
    }
}
