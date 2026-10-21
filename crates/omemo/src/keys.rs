//! Key pairs, XEdDSA signatures, and the own key set a device publishes as its bundle.

use std::collections::BTreeMap;

use curve25519_dalek::{EdwardsPoint, MontgomeryPoint, Scalar};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{Error, crypto};

/// OMEMO device id: a random number in `1..2^31`.
pub type DeviceId = u32;

pub fn generate_device_id() -> Result<DeviceId, Error> {
    loop {
        let id = u32::from_le_bytes(crypto::random()?) & 0x7fff_ffff;
        if id != 0 {
            return Ok(id);
        }
    }
}

/// libsignal's "DJB" key type prefix in front of every serialized Curve25519 key.
const KEY_TYPE_DJB: u8 = 0x05;

/// Curve25519 public key in the 33-byte network format.
pub fn serialize_public(public: &[u8; 32]) -> [u8; 33] {
    let mut out = [0u8; 33];
    out[0] = KEY_TYPE_DJB;
    out[1..].copy_from_slice(public);
    out
}

pub fn parse_public(serialized: &[u8]) -> Result<[u8; 32], Error> {
    match serialized {
        [KEY_TYPE_DJB, key @ ..] if key.len() == 32 => Ok(key.try_into().unwrap()),
        _ => Err(Error::InvalidPublicKey),
    }
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct KeyPair {
    secret: [u8; 32],
    #[zeroize(skip)]
    public: [u8; 32],
}

impl KeyPair {
    pub fn generate() -> Result<Self, Error> {
        Ok(Self::from_secret(crypto::random()?))
    }

    pub fn from_secret(secret: [u8; 32]) -> Self {
        let public = PublicKey::from(&StaticSecret::from(secret)).to_bytes();
        Self { secret, public }
    }

    pub fn public(&self) -> &[u8; 32] {
        &self.public
    }

    pub fn dh(&self, their_public: &[u8; 32]) -> [u8; 32] {
        StaticSecret::from(self.secret)
            .diffie_hellman(&PublicKey::from(*their_public))
            .to_bytes()
    }

    /// XEdDSA signature, as libsignal makes it: the Edwards form of the key always has
    /// the sign bit cleared, so the signature's top bit stays zero.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], Error> {
        let edwards = EdwardsPoint::mul_base_clamped(self.secret);
        let mut a = Scalar::from_bytes_mod_order(clamp(self.secret));
        let mut a_public = edwards.compress().to_bytes();
        if a_public[31] & 0x80 != 0 {
            a = -a;
            a_public[31] &= 0x7f;
        }

        let nonce: [u8; 64] = crypto::random()?;
        let mut hash1_prefix = [0xffu8; 32];
        hash1_prefix[0] = 0xfe;
        let r = wide_scalar(&[&hash1_prefix, a.as_bytes(), message, &nonce]);
        let big_r = EdwardsPoint::mul_base(&r).compress().to_bytes();
        let h = wide_scalar(&[&big_r, &a_public, message]);
        let s = r + h * a;

        let mut signature = [0u8; 64];
        signature[..32].copy_from_slice(&big_r);
        signature[32..].copy_from_slice(s.as_bytes());
        Ok(signature)
    }
}

fn clamp(mut bytes: [u8; 32]) -> [u8; 32] {
    bytes[0] &= 248;
    bytes[31] &= 127;
    bytes[31] |= 64;
    bytes
}

fn wide_scalar(parts: &[&[u8]]) -> Scalar {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    Scalar::from_bytes_mod_order_wide(&hasher.finalize().into())
}

/// Verifies an XEdDSA signature made with the Curve25519 key `public`.
///
/// libsignal reads the sign bit of the Edwards key from the top bit of the signature.
/// python-omemo relies on that: its identity keys are Ed25519 seeds whose public key
/// may have the bit set, and it moves the bit into the signature when publishing.
pub fn verify(public: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let sign = signature[63] >> 7;
    let mut signature = *signature;
    signature[63] &= 0x7f;
    let Some(edwards) = MontgomeryPoint(*public).to_edwards(sign) else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(&edwards.compress().to_bytes()) else {
        return false;
    };
    key.verify(message, &Signature::from_bytes(&signature)).is_ok()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct IdentityKeyPair(pub(crate) KeyPair);

impl IdentityKeyPair {
    pub fn generate() -> Result<Self, Error> {
        KeyPair::generate().map(Self)
    }

    /// Curve25519 public key; its hex form is the fingerprint other clients show.
    pub fn public(&self) -> &[u8; 32] {
        self.0.public()
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SignedPreKey {
    pub id: u32,
    pub(crate) pair: KeyPair,
    #[serde(with = "bytes64")]
    pub signature: [u8; 64],
    /// Unix time of creation, for rotation.
    pub created: u64,
}

/// A device's public key material, as published in its PEP bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub identity_key: [u8; 32],
    pub signed_pre_key_id: u32,
    pub signed_pre_key: [u8; 32],
    #[serde(with = "bytes64")]
    pub signed_pre_key_signature: [u8; 64],
    pub pre_keys: BTreeMap<u32, [u8; 32]>,
}

impl Bundle {
    pub fn verify(&self) -> Result<(), Error> {
        let signed = serialize_public(&self.signed_pre_key);
        if verify(&self.identity_key, &signed, &self.signed_pre_key_signature) {
            Ok(())
        } else {
            Err(Error::BadSignature)
        }
    }
}

/// Everything secret a device owns. Persist it whole after every change.
#[derive(Clone, Serialize, Deserialize)]
pub struct OwnKeys {
    pub device_id: DeviceId,
    pub identity: IdentityKeyPair,
    pub signed_pre_key: SignedPreKey,
    /// Kept for one rotation period, for key exchanges that are still in flight.
    pub old_signed_pre_key: Option<SignedPreKey>,
    pre_keys: BTreeMap<u32, KeyPair>,
    next_pre_key_id: u32,
}

/// How many pre keys a bundle offers; Gajim and Conversations use the same number.
pub const PRE_KEY_COUNT: usize = 100;

impl OwnKeys {
    pub fn generate(created: u64) -> Result<Self, Error> {
        let identity = IdentityKeyPair::generate()?;
        let signed_pre_key = new_signed_pre_key(&identity, 1, created)?;
        let mut keys = Self {
            device_id: generate_device_id()?,
            identity,
            signed_pre_key,
            old_signed_pre_key: None,
            pre_keys: BTreeMap::new(),
            next_pre_key_id: 1,
        };
        keys.refill_pre_keys()?;
        Ok(keys)
    }

    pub fn bundle(&self) -> Bundle {
        Bundle {
            identity_key: *self.identity.public(),
            signed_pre_key_id: self.signed_pre_key.id,
            signed_pre_key: *self.signed_pre_key.pair.public(),
            signed_pre_key_signature: self.signed_pre_key.signature,
            pre_keys: self
                .pre_keys
                .iter()
                .map(|(id, pair)| (*id, *pair.public()))
                .collect(),
        }
    }

    /// Tops the pre keys up to [`PRE_KEY_COUNT`]. Returns whether the bundle changed.
    pub fn refill_pre_keys(&mut self) -> Result<bool, Error> {
        let missing = PRE_KEY_COUNT.saturating_sub(self.pre_keys.len());
        for _ in 0..missing {
            self.pre_keys.insert(self.next_pre_key_id, KeyPair::generate()?);
            self.next_pre_key_id += 1;
        }
        Ok(missing > 0)
    }

    pub fn rotate_signed_pre_key(&mut self, created: u64) -> Result<(), Error> {
        let next = new_signed_pre_key(&self.identity, self.signed_pre_key.id + 1, created)?;
        self.old_signed_pre_key = Some(std::mem::replace(&mut self.signed_pre_key, next));
        Ok(())
    }

    pub(crate) fn signed_pre_key_pair(&self, id: u32) -> Result<&KeyPair, Error> {
        [Some(&self.signed_pre_key), self.old_signed_pre_key.as_ref()]
            .into_iter()
            .flatten()
            .find(|key| key.id == id)
            .map(|key| &key.pair)
            .ok_or(Error::UnknownSignedPreKey(id))
    }

    pub(crate) fn pre_key_pair(&self, id: u32) -> Result<&KeyPair, Error> {
        self.pre_keys.get(&id).ok_or(Error::UnknownPreKey(id))
    }

    /// A pre key is single-use: drop it once a session was built from it.
    pub fn remove_pre_key(&mut self, id: u32) -> bool {
        self.pre_keys.remove(&id).is_some()
    }
}

fn new_signed_pre_key(identity: &IdentityKeyPair, id: u32, created: u64) -> Result<SignedPreKey, Error> {
    let pair = KeyPair::generate()?;
    let signature = identity.0.sign(&serialize_public(pair.public()))?;
    Ok(SignedPreKey {
        id,
        pair,
        signature,
        created,
    })
}

mod bytes64 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("expected 64 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_verifies_and_rejects_tampering() {
        // Run several keys: half of them have the Edwards sign bit set and need the
        // negation in `sign`, which a single key would cover only by luck.
        for _ in 0..16 {
            let pair = KeyPair::generate().unwrap();
            let signature = pair.sign(b"message").unwrap();
            assert_eq!(signature[63] & 0x80, 0);
            assert!(verify(pair.public(), b"message", &signature));
            assert!(!verify(pair.public(), b"massage", &signature));
        }
    }

    #[test]
    fn generated_bundle_verifies() {
        let keys = OwnKeys::generate(0).unwrap();
        let bundle = keys.bundle();
        assert_eq!(bundle.pre_keys.len(), PRE_KEY_COUNT);
        bundle.verify().unwrap();
    }
}
