//! Thin wrappers over the primitives, so the protocol code reads like the spec.

use aes::Aes256;
use cbc::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use hkdf::Hkdf;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::Error;

pub fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).map_err(|_| Error::Random)?;
    Ok(out)
}

pub fn hkdf<const N: usize>(salt: &[u8], ikm: &[u8], info: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, &mut out)
        .expect("HKDF-SHA256 output length is far below the limit");
    out
}

pub fn hmac(key: &[u8], data: &[&[u8]]) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes keys of any length");
    for part in data {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

pub fn aes256_cbc_encrypt(key: &[u8; 32], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    cbc::Encryptor::<Aes256>::new(key.into(), iv.into()).encrypt_padded_vec::<Pkcs7>(plaintext)
}

pub fn aes256_cbc_decrypt(key: &[u8; 32], iv: &[u8; 16], ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    cbc::Decryptor::<Aes256>::new(key.into(), iv.into())
        .decrypt_padded_vec::<Pkcs7>(ciphertext)
        .map_err(|_| Error::Decrypt)
}
