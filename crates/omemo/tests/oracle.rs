//! Interop with python-omemo: every test talks to a real reference device
//! (`tests/oracle/oracle.py`), so a byte out of place anywhere fails here.
//!
//! Needs the oracle environment: `mise run setup`.

use std::collections::{BTreeMap, HashMap};

use omemo::{Bundle, EncryptedKey, Error, OwnKeys, Session, decrypt_payload, encrypt_payload, receive};
use omemo_oracle::{Oracle, Value, b64, json, key32, unb64};

const RUST_JID: &str = "rust@omtalk.test";

trait OracleExt {
    fn bundle(&mut self) -> Bundle;
    fn send_text(&mut self, to: &RustDevice, text: &str) -> Wire;
    fn receive_text(&mut self, from: &RustDevice, wire: &Wire) -> Result<String, String>;
}

impl OracleExt for Oracle {
    fn bundle(&mut self) -> Bundle {
        let b = self.call(json!({"cmd": "bundle"}));
        Bundle {
            identity_key: key32(&b["identity_key"]),
            signed_pre_key_id: b["signed_pre_key_id"].as_u64().unwrap() as u32,
            signed_pre_key: key32(&b["signed_pre_key"]),
            signed_pre_key_signature: unb64(&b["signed_pre_key_signature"]).try_into().unwrap(),
            pre_keys: b["pre_keys"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(id, key)| (id.parse().unwrap(), key32(key)))
                .collect(),
        }
    }

    /// The oracle encrypts a text for the Rust device.
    fn send_text(&mut self, to: &RustDevice, text: &str) -> Wire {
        let answer = self.call(json!({
            "cmd": "encrypt", "jid": RUST_JID, "device": to.keys.device_id,
            "bundle": bundle_json(&to.keys.bundle()), "plaintext": b64(text.as_bytes()), "key_material": null,
        }));
        Wire {
            key: EncryptedKey {
                data: unb64(&answer["key"]),
                prekey: answer["prekey"].as_bool().unwrap(),
            },
            iv: unb64(&answer["iv"]),
            payload: unb64(&answer["payload"]),
        }
    }

    fn receive_text(&mut self, from: &RustDevice, wire: &Wire) -> Result<String, String> {
        let answer = self.try_call(json!({
            "cmd": "decrypt", "jid": RUST_JID, "device": from.keys.device_id,
            "key": b64(&wire.key.data), "prekey": wire.key.prekey,
            "iv": b64(&wire.iv), "payload": b64(&wire.payload),
        }))?;
        Ok(String::from_utf8(unb64(&answer["plaintext"])).unwrap())
    }
}

fn bundle_json(bundle: &Bundle) -> Value {
    json!({
        "identity_key": b64(&bundle.identity_key),
        "signed_pre_key_id": bundle.signed_pre_key_id,
        "signed_pre_key": b64(&bundle.signed_pre_key),
        "signed_pre_key_signature": b64(&bundle.signed_pre_key_signature),
        "pre_keys": bundle.pre_keys.iter().map(|(id, key)| (id.to_string(), b64(key))).collect::<BTreeMap<_, _>>(),
    })
}

/// One `<encrypted>` element addressed to a single device.
#[derive(Clone)]
struct Wire {
    key: EncryptedKey,
    iv: Vec<u8>,
    payload: Vec<u8>,
}

/// The Rust side, handling sessions the way the client will.
struct RustDevice {
    keys: OwnKeys,
    sessions: HashMap<u32, Session>,
}

impl RustDevice {
    fn new() -> Self {
        Self {
            keys: OwnKeys::generate(0).unwrap(),
            sessions: HashMap::new(),
        }
    }

    fn send_text(&mut self, oracle: &mut Oracle, text: &str) -> Wire {
        if !self.sessions.contains_key(&oracle.device_id) {
            let bundle = oracle.bundle();
            self.sessions
                .insert(oracle.device_id, Session::initiate(&self.keys, &bundle).unwrap());
        }
        let payload = encrypt_payload(text.as_bytes()).unwrap();
        let key = self
            .sessions
            .get_mut(&oracle.device_id)
            .unwrap()
            .encrypt(&payload.key_material);
        Wire {
            key,
            iv: payload.iv.to_vec(),
            payload: payload.ciphertext,
        }
    }

    fn receive_text(&mut self, from: u32, wire: &Wire) -> Result<String, Error> {
        let received = receive(
            &self.keys,
            self.sessions.get(&from),
            &wire.key.data,
            wire.key.prekey,
        )?;
        let plaintext = decrypt_payload(&received.key_material, &wire.iv, &wire.payload)?;
        if let Some(id) = received.used_pre_key {
            assert!(self.keys.remove_pre_key(id));
            self.keys.refill_pre_keys().unwrap();
        }
        self.sessions.insert(from, received.session);
        Ok(String::from_utf8(plaintext).unwrap())
    }
}

#[test]
fn rust_starts_the_session() {
    let mut oracle = Oracle::spawn();
    let mut rust = RustDevice::new();

    // Until the oracle answers, every message carries the key exchange, and the
    // oracle may see them in any order.
    let first: Vec<_> = (0..3)
        .map(|i| rust.send_text(&mut oracle, &format!("привет {i}")))
        .collect();
    assert!(first.iter().all(|w| w.key.prekey));
    for i in [2, 0, 1] {
        assert_eq!(
            oracle.receive_text(&rust, &first[i]).unwrap(),
            format!("привет {i}")
        );
    }

    let reply = oracle.send_text(&rust, "ответ");
    assert!(!reply.key.prekey);
    assert_eq!(rust.receive_text(oracle.device_id, &reply).unwrap(), "ответ");

    let after = rust.send_text(&mut oracle, "уже без обмена ключами");
    assert!(
        !after.key.prekey,
        "a confirmed session must stop sending the key exchange"
    );
    assert_eq!(
        oracle.receive_text(&rust, &after).unwrap(),
        "уже без обмена ключами"
    );
}

#[test]
fn oracle_starts_the_session() {
    let mut oracle = Oracle::spawn();
    let mut rust = RustDevice::new();

    let first = oracle.send_text(&rust, "one");
    let second = oracle.send_text(&rust, "two");
    assert!(first.key.prekey && second.key.prekey);
    assert_eq!(rust.receive_text(oracle.device_id, &first).unwrap(), "one");
    // Its pre key is gone by now; the repeat must decrypt in the existing session.
    assert_eq!(rust.receive_text(oracle.device_id, &second).unwrap(), "two");

    let reply = rust.send_text(&mut oracle, "three");
    assert!(!reply.key.prekey);
    assert_eq!(oracle.receive_text(&rust, &reply).unwrap(), "three");
    assert!(!oracle.send_text(&rust, "four").key.prekey);
}

#[test]
fn long_conversation_with_bursts_and_reordering() {
    let mut oracle = Oracle::spawn();
    let mut rust = RustDevice::new();
    let opening = rust.send_text(&mut oracle, "start");
    oracle.receive_text(&rust, &opening).unwrap();

    // Burst sizes cover a single message, chains longer than one, and ratchet steps in
    // both directions; reversing a burst exercises the skipped message keys.
    for (round, burst) in [1usize, 3, 1, 7, 2, 1, 5, 4, 1, 1, 6, 2].into_iter().enumerate() {
        let texts: Vec<String> = (0..burst).map(|i| format!("o{round}.{i}")).collect();
        let wires: Vec<_> = texts.iter().map(|t| oracle.send_text(&rust, t)).collect();
        for i in (0..burst).rev() {
            assert_eq!(rust.receive_text(oracle.device_id, &wires[i]).unwrap(), texts[i]);
        }

        let texts: Vec<String> = (0..burst).map(|i| format!("r{round}.{i}")).collect();
        let wires: Vec<_> = texts.iter().map(|t| rust.send_text(&mut oracle, t)).collect();
        for i in (0..burst).rev() {
            assert_eq!(oracle.receive_text(&rust, &wires[i]).unwrap(), texts[i]);
        }
    }
}

#[test]
fn messages_from_a_previous_chain_still_decrypt() {
    let mut oracle = Oracle::spawn();
    let mut rust = RustDevice::new();
    let opening = rust.send_text(&mut oracle, "start");
    oracle.receive_text(&rust, &opening).unwrap();

    // Oracle to Rust: the last message of a chain arrives after the next chain began.
    // Its key can only be derived because the next chain announces how long the old
    // one was.
    let early = oracle.send_text(&rust, "early");
    let late = oracle.send_text(&rust, "late");
    assert_eq!(rust.receive_text(oracle.device_id, &early).unwrap(), "early");
    let reply = rust.send_text(&mut oracle, "reply");
    oracle.receive_text(&rust, &reply).unwrap();
    let next = oracle.send_text(&rust, "next");
    assert_eq!(rust.receive_text(oracle.device_id, &next).unwrap(), "next");
    assert_eq!(rust.receive_text(oracle.device_id, &late).unwrap(), "late");

    // Rust to oracle: the same, with Rust announcing its old chain.
    let early = rust.send_text(&mut oracle, "early");
    let late = rust.send_text(&mut oracle, "late");
    assert_eq!(oracle.receive_text(&rust, &early).unwrap(), "early");
    let reply = oracle.send_text(&rust, "reply");
    rust.receive_text(oracle.device_id, &reply).unwrap();
    let next = rust.send_text(&mut oracle, "next");
    assert_eq!(oracle.receive_text(&rust, &next).unwrap(), "next");
    assert_eq!(oracle.receive_text(&rust, &late).unwrap(), "late");
}

#[test]
fn duplicates_and_forgeries_are_rejected_without_breaking_the_session() {
    let mut oracle = Oracle::spawn();
    let mut rust = RustDevice::new();
    let opening = rust.send_text(&mut oracle, "start");
    oracle.receive_text(&rust, &opening).unwrap();

    let message = oracle.send_text(&rust, "once");
    assert_eq!(rust.receive_text(oracle.device_id, &message).unwrap(), "once");
    assert_eq!(
        rust.receive_text(oracle.device_id, &message),
        Err(Error::DuplicateMessage)
    );

    let mut forged = oracle.send_text(&rust, "genuine");
    let last = forged.key.data.len() - 1;
    forged.key.data[last] ^= 1;
    assert_eq!(rust.receive_text(oracle.device_id, &forged), Err(Error::Mac));
    forged.key.data[last] ^= 1;
    assert_eq!(rust.receive_text(oracle.device_id, &forged).unwrap(), "genuine");

    let mut forged = rust.send_text(&mut oracle, "to oracle");
    forged.payload[0] ^= 1;
    assert!(oracle.receive_text(&rust, &forged).is_err());
}

#[test]
fn bundle_signatures_verify_both_ways() {
    // python-omemo identity keys are Ed25519 seeds, so about half of them have the
    // Edwards sign bit set; that half only verifies if the bit is read from the
    // signature. Eight keys miss that case with probability 1/256.
    for _ in 0..8 {
        Oracle::spawn().bundle().verify().unwrap();
    }

    let mut oracle = Oracle::spawn();
    let rust = RustDevice::new();
    let mut bundle = rust.keys.bundle();
    bundle.signed_pre_key_signature[0] ^= 1;
    let answer = oracle.try_call(json!({
        "cmd": "encrypt", "jid": RUST_JID, "device": rust.keys.device_id,
        "bundle": bundle_json(&bundle), "plaintext": b64(b"x"), "key_material": null,
    }));
    assert!(
        answer.unwrap_err().contains("KeyExchangeFailed"),
        "oracle must reject a forged bundle"
    );
}
