use omemo::Bundle;
use omemo::keys::{parse_public, serialize_public};

use crate::{Error, xml};

pub fn to_xml(bundle: &Bundle) -> xml::Bundle {
    xml::Bundle {
        signed_pre_key_public: Some(xml::SignedPreKeyPublic {
            signed_pre_key_id: Some(bundle.signed_pre_key_id),
            data: serialize_public(&bundle.signed_pre_key).to_vec(),
        }),
        signed_pre_key_signature: Some(xml::SignedPreKeySignature {
            data: bundle.signed_pre_key_signature.to_vec(),
        }),
        identity_key: Some(xml::IdentityKey {
            data: serialize_public(&bundle.identity_key).to_vec(),
        }),
        prekeys: Some(xml::Prekeys {
            keys: bundle
                .pre_keys
                .iter()
                .map(|(&pre_key_id, key)| xml::PreKeyPublic {
                    pre_key_id,
                    data: serialize_public(key).to_vec(),
                })
                .collect(),
        }),
    }
}

/// Reads a bundle. The signature is checked later, when a session is built from it.
pub fn from_xml(bundle: &xml::Bundle) -> Result<Bundle, Error> {
    let signed = bundle
        .signed_pre_key_public
        .as_ref()
        .ok_or(Error::Malformed("bundle without signedPreKeyPublic"))?;
    let signature = bundle
        .signed_pre_key_signature
        .as_ref()
        .ok_or(Error::Malformed("bundle without signedPreKeySignature"))?;
    let identity = bundle
        .identity_key
        .as_ref()
        .ok_or(Error::Malformed("bundle without identityKey"))?;
    let pre_keys = bundle
        .prekeys
        .as_ref()
        .ok_or(Error::Malformed("bundle without prekeys"))?;

    Ok(Bundle {
        identity_key: parse_public(&identity.data)?,
        signed_pre_key_id: signed
            .signed_pre_key_id
            .ok_or(Error::Malformed("signedPreKeyPublic without signedPreKeyId"))?,
        signed_pre_key: parse_public(&signed.data)?,
        signed_pre_key_signature: signature
            .data
            .as_slice()
            .try_into()
            .map_err(|_| Error::Malformed("signedPreKeySignature is not 64 bytes"))?,
        pre_keys: pre_keys
            .keys
            .iter()
            .map(|key| Ok((key.pre_key_id, parse_public(&key.data)?)))
            .collect::<Result<_, Error>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_a_verifiable_bundle() {
        let bundle = omemo::OwnKeys::generate(0).unwrap().bundle();
        let parsed = from_xml(&to_xml(&bundle)).unwrap();
        assert_eq!(parsed, bundle);
        parsed.verify().unwrap();
    }

    #[test]
    fn missing_or_misformatted_parts_are_rejected() {
        let good = to_xml(&omemo::OwnKeys::generate(0).unwrap().bundle());

        let mut no_id = good.clone();
        no_id.signed_pre_key_public.as_mut().unwrap().signed_pre_key_id = None;
        assert!(matches!(from_xml(&no_id), Err(Error::Malformed(_))));

        // A key without the 0x05 type byte is how a broken client shows up.
        let mut raw_key = good.clone();
        raw_key.identity_key.as_mut().unwrap().data.remove(0);
        assert_eq!(from_xml(&raw_key), Err(omemo::Error::InvalidPublicKey.into()));
    }
}
