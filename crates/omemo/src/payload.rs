//! The message body: AES-128-GCM, with key and tag sent through the ratchet.

use aes_gcm::aead::AeadInOut;
use aes_gcm::{Aes128Gcm, KeyInit};

use crate::{Error, crypto};

pub struct EncryptedPayload {
    /// 16-byte key followed by the 16-byte GCM tag: what goes into every `<key>`.
    pub key_material: [u8; 32],
    pub iv: [u8; 12],
    pub ciphertext: Vec<u8>,
}

pub fn encrypt_payload(plaintext: &[u8]) -> Result<EncryptedPayload, Error> {
    let key: [u8; 16] = crypto::random()?;
    let iv: [u8; 12] = crypto::random()?;
    let mut ciphertext = plaintext.to_vec();
    let tag = Aes128Gcm::new(&key.into())
        .encrypt_inout_detached(&iv.into(), &[], ciphertext.as_mut_slice().into())
        .map_err(|_| Error::Decrypt)?;

    let mut key_material = [0u8; 32];
    key_material[..16].copy_from_slice(&key);
    key_material[16..].copy_from_slice(&tag);
    Ok(EncryptedPayload {
        key_material,
        iv,
        ciphertext,
    })
}

/// Accepts both layouts found in the wild: the tag in the key material (32 bytes, what
/// everyone sends today) and the tag at the end of the payload (16-byte key material,
/// sent by old clients).
pub fn decrypt_payload(key_material: &[u8], iv: &[u8], payload: &[u8]) -> Result<Vec<u8>, Error> {
    let (key, tag, ciphertext) = match key_material.len() {
        32 => (&key_material[..16], &key_material[16..], payload),
        16 if payload.len() >= 16 => {
            let (ciphertext, tag) = payload.split_at(payload.len() - 16);
            (key_material, tag, ciphertext)
        }
        _ => return Err(Error::Decrypt),
    };
    let iv: [u8; 12] = iv.try_into().map_err(|_| Error::Decrypt)?;
    let cipher = Aes128Gcm::new_from_slice(key).map_err(|_| Error::Decrypt)?;
    let mut plaintext = ciphertext.to_vec();
    cipher
        .decrypt_inout_detached(
            &iv.into(),
            &[],
            plaintext.as_mut_slice().into(),
            tag.try_into().unwrap(),
        )
        .map_err(|_| Error::Decrypt)?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tag_layouts_decrypt() {
        let encrypted = encrypt_payload("привет".as_bytes()).unwrap();
        let plaintext =
            decrypt_payload(&encrypted.key_material, &encrypted.iv, &encrypted.ciphertext).unwrap();
        assert_eq!(plaintext, "привет".as_bytes());

        let mut legacy = encrypted.ciphertext.clone();
        legacy.extend_from_slice(&encrypted.key_material[16..]);
        let plaintext = decrypt_payload(&encrypted.key_material[..16], &encrypted.iv, &legacy).unwrap();
        assert_eq!(plaintext, "привет".as_bytes());
    }

    #[test]
    fn tampered_payload_fails() {
        let mut encrypted = encrypt_payload(b"hello").unwrap();
        encrypted.ciphertext[0] ^= 1;
        assert!(decrypt_payload(&encrypted.key_material, &encrypted.iv, &encrypted.ciphertext).is_err());
    }
}
