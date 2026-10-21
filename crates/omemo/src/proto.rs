//! The two protobuf structures of libsignal's WhisperTextProtocol, written by hand:
//! they are small, and a code generator would be the larger dependency.
//!
//! Fields are written in field-number order, as python-omemo and libsignal do.

use crate::Error;
use crate::keys::{parse_public, serialize_public};

const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const BYTES: u8 = 2;
const FIXED32: u8 = 5;

/// `SignalMessage`: one Double Ratchet message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub ratchet_key: [u8; 32],
    pub counter: u32,
    pub previous_counter: u32,
    pub ciphertext: Vec<u8>,
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48 + self.ciphertext.len());
        put_bytes(&mut out, 1, &serialize_public(&self.ratchet_key));
        put_varint_field(&mut out, 2, self.counter);
        put_varint_field(&mut out, 3, self.previous_counter);
        put_bytes(&mut out, 4, &self.ciphertext);
        out
    }

    pub fn decode(mut input: &[u8]) -> Result<Self, Error> {
        let (mut ratchet_key, mut counter, mut previous_counter, mut ciphertext) = (None, None, None, None);
        while !input.is_empty() {
            match read_field(&mut input)? {
                (1, Field::Bytes(b)) => ratchet_key = Some(parse_public(b)?),
                (2, Field::Varint(v)) => counter = Some(to_u32(v)?),
                (3, Field::Varint(v)) => previous_counter = Some(to_u32(v)?),
                (4, Field::Bytes(b)) => ciphertext = Some(b.to_vec()),
                _ => {}
            }
        }
        Ok(Self {
            ratchet_key: ratchet_key.ok_or(Error::Protobuf)?,
            counter: counter.ok_or(Error::Protobuf)?,
            previous_counter: previous_counter.unwrap_or(0),
            ciphertext: ciphertext.ok_or(Error::Protobuf)?,
        })
    }
}

/// `PreKeySignalMessage`: the X3DH data that starts a session, around the first message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyExchange {
    pub pre_key_id: u32,
    pub base_key: [u8; 32],
    pub identity_key: [u8; 32],
    /// Serialized authenticated [`Message`].
    pub message: Vec<u8>,
    pub signed_pre_key_id: u32,
}

impl KeyExchange {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + self.message.len());
        put_varint_field(&mut out, 1, self.pre_key_id);
        put_bytes(&mut out, 2, &serialize_public(&self.base_key));
        put_bytes(&mut out, 3, &serialize_public(&self.identity_key));
        put_bytes(&mut out, 4, &self.message);
        put_varint_field(&mut out, 6, self.signed_pre_key_id);
        out
    }

    pub fn decode(mut input: &[u8]) -> Result<Self, Error> {
        let (mut pre_key_id, mut base_key, mut identity_key, mut message, mut signed_pre_key_id) =
            (None, None, None, None, None);
        while !input.is_empty() {
            match read_field(&mut input)? {
                (1, Field::Varint(v)) => pre_key_id = Some(to_u32(v)?),
                (2, Field::Bytes(b)) => base_key = Some(parse_public(b)?),
                (3, Field::Bytes(b)) => identity_key = Some(parse_public(b)?),
                (4, Field::Bytes(b)) => message = Some(b.to_vec()),
                (6, Field::Varint(v)) => signed_pre_key_id = Some(to_u32(v)?),
                _ => {}
            }
        }
        Ok(Self {
            // OMEMO requires a pre key; a key exchange without one is not accepted.
            pre_key_id: pre_key_id.ok_or(Error::Protobuf)?,
            base_key: base_key.ok_or(Error::Protobuf)?,
            identity_key: identity_key.ok_or(Error::Protobuf)?,
            message: message.ok_or(Error::Protobuf)?,
            signed_pre_key_id: signed_pre_key_id.ok_or(Error::Protobuf)?,
        })
    }
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn to_u32(value: u64) -> Result<u32, Error> {
    u32::try_from(value).map_err(|_| Error::Protobuf)
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_varint_field(out: &mut Vec<u8>, number: u8, value: u32) {
    out.push(number << 3 | VARINT);
    put_varint(out, value.into());
}

fn put_bytes(out: &mut Vec<u8>, number: u8, bytes: &[u8]) {
    out.push(number << 3 | BYTES);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn read_varint(input: &mut &[u8]) -> Result<u64, Error> {
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = input.split_first().ok_or(Error::Protobuf)?;
        *input = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(Error::Protobuf)
}

fn take<'a>(input: &mut &'a [u8], len: usize) -> Result<&'a [u8], Error> {
    if input.len() < len {
        return Err(Error::Protobuf);
    }
    let (taken, rest) = input.split_at(len);
    *input = rest;
    Ok(taken)
}

fn read_field<'a>(input: &mut &'a [u8]) -> Result<(u64, Field<'a>), Error> {
    let key = read_varint(input)?;
    let field = match (key & 7) as u8 {
        VARINT => Field::Varint(read_varint(input)?),
        BYTES => {
            let len = usize::try_from(read_varint(input)?).map_err(|_| Error::Protobuf)?;
            Field::Bytes(take(input, len)?)
        }
        FIXED64 => take(input, 8).map(|_| Field::Fixed)?,
        FIXED32 => take(input, 4).map(|_| Field::Fixed)?,
        _ => return Err(Error::Protobuf),
    };
    Ok((key >> 3, field))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_and_matches_libsignal_layout() {
        let message = Message {
            ratchet_key: [7; 32],
            counter: 300,
            previous_counter: 0,
            ciphertext: vec![1, 2, 3],
        };
        let encoded = message.encode();
        // ratchetKey (field 1, 33 bytes) comes first, then counter as a two-byte varint.
        assert_eq!(&encoded[..3], &[0x0a, 33, 0x05]);
        assert_eq!(&encoded[35..38], &[0x10, 0xac, 0x02]);
        assert_eq!(Message::decode(&encoded).unwrap(), message);
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let encoded = Message {
            ratchet_key: [7; 32],
            counter: 1,
            previous_counter: 0,
            ciphertext: vec![9; 40],
        }
        .encode();
        for len in 0..encoded.len() {
            assert!(Message::decode(&encoded[..len]).is_err());
        }
    }
}
