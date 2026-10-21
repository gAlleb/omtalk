//! XML interop with python-omemo: the oracle serializes and parses with `oldmemo.etree`,
//! the code python-omemo clients put on the wire.
//!
//! Needs the oracle environment: `mise run setup`.

use omemo::{OwnKeys, Session};
use omemo_oracle::{Oracle, b64, json, unb64};
use omemo_xml::Element;
use omemo_xml::{bundle, device_list, message, xml};

const RUST_JID: &str = "rust@omtalk.test";

fn to_string(element: impl Into<Element>) -> String {
    let mut out = Vec::new();
    element.into().write_to(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

fn parse<T: TryFrom<Element>>(text: &str) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(text.parse::<Element>().unwrap()).unwrap()
}

fn oracle_bundle(oracle: &mut Oracle) -> omemo::Bundle {
    let text = oracle.call(json!({"cmd": "bundle_xml"}))["xml"]
        .as_str()
        .unwrap()
        .to_owned();
    bundle::from_xml(&parse(&text)).unwrap()
}

fn rust_message_to_oracle(
    oracle: &mut Oracle,
    keys: &OwnKeys,
    encrypted: xml::Encrypted,
) -> Result<String, String> {
    let answer = oracle.try_call(json!({
        "cmd": "decrypt_xml", "jid": RUST_JID, "identity_key": b64(keys.identity.public()),
        "xml": to_string(encrypted),
    }))?;
    Ok(String::from_utf8(unb64(&answer["plaintext"])).unwrap())
}

fn oracle_message_to_rust(oracle: &mut Oracle, keys: &OwnKeys, text: Option<&str>) -> xml::Encrypted {
    let answer = oracle.call(json!({
        "cmd": "encrypt_xml", "jid": RUST_JID, "device": keys.device_id,
        "bundle_xml": to_string(bundle::to_xml(&keys.bundle())),
        "plaintext": text.map(|t| b64(t.as_bytes())),
    }));
    parse(answer["xml"].as_str().unwrap())
}

#[test]
fn rust_writes_a_message_the_oracle_reads() {
    let mut oracle = Oracle::spawn();
    let keys = OwnKeys::generate(0).unwrap();
    let mut session = Session::initiate(&keys, &oracle_bundle(&mut oracle)).unwrap();

    let first = message::encrypt(
        keys.device_id,
        "первое".as_bytes(),
        [(oracle.device_id, &mut session)],
    )
    .unwrap();
    assert!(first.header.keys[0].prekey);
    assert_eq!(
        rust_message_to_oracle(&mut oracle, &keys, first).unwrap(),
        "первое"
    );
}

#[test]
fn oracle_writes_messages_rust_reads() {
    let mut oracle = Oracle::spawn();
    let mut keys = OwnKeys::generate(0).unwrap();

    let first = oracle_message_to_rust(&mut oracle, &keys, Some("hello"));
    assert_eq!(first.header.sid, oracle.device_id);
    assert!(
        first.header.keys[0].prekey,
        "the oracle's prekey=\"true\" must survive parsing"
    );
    let decrypted = message::decrypt(&keys, None, &first).unwrap();
    assert_eq!(decrypted.plaintext.as_deref(), Some(&b"hello"[..]));
    keys.remove_pre_key(decrypted.received.used_pre_key.unwrap());
    let mut session = decrypted.received.session;

    // Rust answers, which confirms the session for the oracle; then an empty message
    // from the oracle advances the session without content.
    let reply = message::encrypt(keys.device_id, b"reply", [(oracle.device_id, &mut session)]).unwrap();
    assert_eq!(
        rust_message_to_oracle(&mut oracle, &keys, reply).unwrap(),
        "reply"
    );
    let empty = oracle_message_to_rust(&mut oracle, &keys, None);
    assert!(empty.payload.is_none() && !empty.header.keys[0].prekey);
    let decrypted = message::decrypt(&keys, Some(&session), &empty).unwrap();
    assert_eq!(decrypted.plaintext, None);
}

#[test]
fn rust_empty_message_completes_the_oracle_handshake() {
    let mut oracle = Oracle::spawn();
    let mut keys = OwnKeys::generate(0).unwrap();
    let first = oracle_message_to_rust(&mut oracle, &keys, Some("hello"));
    let decrypted = message::decrypt(&keys, None, &first).unwrap();
    keys.remove_pre_key(decrypted.received.used_pre_key.unwrap());
    let mut session = decrypted.received.session;

    let empty = message::empty(keys.device_id, [(oracle.device_id, &mut session)]).unwrap();
    assert!(empty.payload.is_none());
    let answer = oracle.call(json!({
        "cmd": "decrypt_xml", "jid": RUST_JID, "identity_key": b64(keys.identity.public()),
        "xml": to_string(empty),
    }));
    assert!(answer.get("plaintext").is_none());
    let next = oracle_message_to_rust(&mut oracle, &keys, Some("after"));
    assert!(
        !next.header.keys[0].prekey,
        "the empty message must have confirmed the session"
    );
}

#[test]
fn one_message_for_several_devices() {
    let mut first = Oracle::spawn();
    let mut second = Oracle::spawn();
    let keys = OwnKeys::generate(0).unwrap();
    let mut to_first = Session::initiate(&keys, &oracle_bundle(&mut first)).unwrap();
    let mut to_second = Session::initiate(&keys, &oracle_bundle(&mut second)).unwrap();

    let encrypted = message::encrypt(
        keys.device_id,
        b"for both",
        [
            (first.device_id, &mut to_first),
            (second.device_id, &mut to_second),
        ],
    )
    .unwrap();
    assert_eq!(
        rust_message_to_oracle(&mut first, &keys, encrypted.clone()).unwrap(),
        "for both"
    );
    assert_eq!(
        rust_message_to_oracle(&mut second, &keys, encrypted).unwrap(),
        "for both"
    );
}

#[test]
fn device_lists_both_ways() {
    let mut oracle = Oracle::spawn();
    let text = oracle.call(json!({"cmd": "device_list_xml", "devices": [7, 1_000_000, 42]}))["xml"]
        .as_str()
        .unwrap()
        .to_owned();
    let devices: Vec<_> = device_list::from_xml(&parse(&text)).into_iter().collect();
    assert_eq!(devices, [7, 42, 1_000_000]);

    let answer = oracle.call(json!({
        "cmd": "parse_device_list_xml", "xml": to_string(device_list::to_xml([3, 2_147_483_647])),
    }));
    assert_eq!(answer["devices"], json!([3, 2_147_483_647]));
}

#[test]
fn oracle_bundles_parse_and_verify() {
    // Half of python-omemo's identity keys need the sign bit carried in the signature.
    for _ in 0..8 {
        let mut oracle = Oracle::spawn();
        let bundle = oracle_bundle(&mut oracle);
        assert_eq!(bundle.pre_keys.len(), 100);
        bundle.verify().unwrap();
    }
}
