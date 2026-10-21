"""Reference OMEMO 0.3 device for the interop tests of the `omemo` crate.

Wraps the python-omemo `Oldmemo` backend (the code base Gajim's OMEMO comes from) and
speaks JSON lines on stdin/stdout: one command in, one answer out. Binary values are
base64. Keys go over the pipe as raw 32-byte Curve25519 keys; the signed pre key
signature carries the identity key's Edwards sign bit in its top bit, exactly as
python-omemo publishes it in a bundle.

Session handling follows omemo.SessionManager: the key exchange is repeated until the
session is confirmed, a repeated key exchange decrypts in the session it built, and a
pre key is deleted once used.
"""

import asyncio
import base64
import json
import xml.etree.ElementTree as ET
import secrets
import sys

import x3dh
import xeddsa
from oldmemo import etree
from omemo.message import Message
from omemo.session import Initiation
from omemo.storage import Just, Maybe, Nothing, Storage
from oldmemo.oldmemo import (
    BundleImpl,
    ContentImpl,
    EncryptedKeyMaterialImpl,
    KeyExchangeImpl,
    NAMESPACE,
    Oldmemo,
    PlainKeyMaterialImpl,
)

OWN_JID = "oracle@omtalk.test"


def b64(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def unb64(text: str) -> bytes:
    return base64.b64decode(text)


class MemoryStorage(Storage):
    def __init__(self) -> None:
        super().__init__()
        self.data: dict = {}

    async def _load(self, key: str) -> Maybe:
        return Just(self.data[key]) if key in self.data else Nothing()

    async def _store(self, key: str, value) -> None:
        self.data[key] = value

    async def _delete(self, key: str) -> None:
        self.data.pop(key, None)


class Device:
    def __init__(self) -> None:
        self.device_id = secrets.randbelow(2**31 - 1) + 1
        self.backend = Oldmemo(MemoryStorage())

    async def start(self) -> dict:
        await self.backend.generate_pre_keys(100)
        return {"device_id": self.device_id}

    async def bundle(self) -> dict:
        bundle = await self.backend.get_bundle(OWN_JID, self.device_id)
        ed_identity = bundle.bundle.identity_key
        signature = bytearray(bundle.bundle.signed_pre_key_sig)
        signature[63] |= ed_identity[31] & 0x80
        return {
            "device_id": self.device_id,
            "identity_key": b64(xeddsa.ed25519_pub_to_curve25519_pub(ed_identity)),
            "signed_pre_key_id": bundle.signed_pre_key_id,
            "signed_pre_key": b64(bundle.bundle.signed_pre_key),
            "signed_pre_key_signature": b64(bytes(signature)),
            "pre_keys": {str(i): b64(k) for k, i in bundle.pre_key_ids.items()},
        }

    async def _encrypt(self, jid: str, device: int, bundle: BundleImpl | None, plaintext: bytes | None,
                       key_material: bytes | None):
        if plaintext is not None:
            content, plain = await self.backend.encrypt_plaintext(plaintext)
        elif key_material is not None:
            content, plain = None, PlainKeyMaterialImpl(key_material[:16], key_material[16:])
        else:
            content, plain = await self.backend.encrypt_empty()

        session = await self.backend.load_session(jid, device)
        if session is None:
            session, encrypted = await self.backend.build_session_active(jid, device, bundle, plain)
        else:
            encrypted = await self.backend.encrypt_key_material(session, plain)
        await self.backend.store_session(session)

        exchange = None
        if session.initiation is Initiation.ACTIVE and not session.confirmed:
            exchange = session.key_exchange
        return encrypted, exchange, content

    async def _decrypt(self, jid: str, device: int, exchange: KeyExchangeImpl | None,
                       encrypted: EncryptedKeyMaterialImpl, content: ContentImpl | None) -> dict:
        session = await self.backend.load_session(jid, device)
        if exchange is not None:
            if session is None or not (session.initiation is Initiation.PASSIVE
                                       and session.key_exchange.builds_same_session(exchange)):
                session, plain = await self.backend.build_session_passive(jid, device, exchange, encrypted)
            else:
                plain = await self.backend.decrypt_key_material(session, encrypted)
        else:
            if session is None:
                raise RuntimeError("no session")
            plain = await self.backend.decrypt_key_material(session, encrypted)

        answer = {"key_material": b64(plain.key + plain.auth_tag)}
        if content is not None and not content.empty:
            answer["plaintext"] = b64(await self.backend.decrypt_plaintext(content, plain))

        await self.backend.store_session(session)
        if exchange is not None:
            await self.backend.delete_pre_key(session)
        return answer

    async def encrypt(self, jid: str, device: int, bundle: dict | None, plaintext: str | None,
                      key_material: str | None) -> dict:
        encrypted, exchange, content = await self._encrypt(
            jid, device, None if bundle is None else parse_bundle(jid, device, bundle),
            None if plaintext is None else unb64(plaintext),
            None if key_material is None else unb64(key_material))
        data, prekey = encrypted.serialize(), exchange is not None
        if prekey:
            data, _ = exchange.serialize(data)

        answer = {"key": b64(data), "prekey": prekey}
        if content is not None:
            answer |= {"iv": b64(content.initialization_vector), "payload": b64(content.ciphertext)}
        return answer

    async def decrypt(self, jid: str, device: int, key: str, prekey: bool, iv: str | None = None,
                      payload: str | None = None) -> dict:
        data = unb64(key)
        exchange = None
        if prekey:
            exchange, data = KeyExchangeImpl.parse(data, False)
        encrypted = EncryptedKeyMaterialImpl.parse(data, OWN_JID, self.device_id)
        content = None if payload is None else ContentImpl(unb64(payload), unb64(iv))
        return await self._decrypt(jid, device, exchange, encrypted, content)

    # The XML commands go through oldmemo.etree, the serializer and parser python-omemo
    # clients use on the wire.

    async def bundle_xml(self) -> dict:
        bundle = await self.backend.get_bundle(OWN_JID, self.device_id)
        return {"xml": to_xml(etree.serialize_bundle(bundle))}

    async def device_list_xml(self, devices: list[int]) -> dict:
        return {"xml": to_xml(etree.serialize_device_list({d: None for d in devices}))}

    async def parse_device_list_xml(self, xml: str) -> dict:
        return {"devices": sorted(etree.parse_device_list(ET.fromstring(xml)))}

    async def encrypt_xml(self, jid: str, device: int, bundle_xml: str | None, plaintext: str | None) -> dict:
        bundle = None if bundle_xml is None else etree.parse_bundle(ET.fromstring(bundle_xml), jid, device)
        encrypted, exchange, content = await self._encrypt(
            jid, device, bundle, None if plaintext is None else unb64(plaintext), None)
        message = Message(NAMESPACE, OWN_JID, self.device_id, content, frozenset({(encrypted, exchange)}))
        return {"xml": to_xml(etree.serialize_message(message))}

    async def decrypt_xml(self, jid: str, identity_key: str, xml: str) -> dict:
        """`identity_key` is the sender's Curve25519 identity key, as its bundle names it."""
        message = await etree.parse_message(
            ET.fromstring(xml), jid, OWN_JID, KnownDevices(unb64(identity_key)))
        ours = [(encrypted, exchange) for encrypted, exchange in message.keys
                if encrypted.device_id == self.device_id]
        if not ours:
            raise RuntimeError("no key for this device")
        encrypted, exchange = ours[0]
        return await self._decrypt(jid, message.device_id, exchange, encrypted, message.content)


class KnownDevices:
    """Just enough of omemo.SessionManager for oldmemo.etree.parse_message: whatever
    device the message names is known, with the identity key the test supplies."""

    def __init__(self, curve_identity_key: bytes) -> None:
        # Rust signs with the Edwards sign bit cleared.
        self.identity_key = xeddsa.curve25519_pub_to_ed25519_pub(curve_identity_key, False)

    async def get_device_information(self, bare_jid: str):
        identity_key = self.identity_key

        class AnyDevice:
            def __eq__(self, other):
                return True

        class Info:
            device_id = AnyDevice()

        Info.identity_key = identity_key
        return [Info]

    async def refresh_device_list(self, namespace: str, bare_jid: str) -> None:
        pass


def to_xml(element: ET.Element) -> str:
    return ET.tostring(element, encoding="unicode")


def parse_bundle(jid: str, device: int, bundle: dict) -> BundleImpl:
    signature = bytearray(unb64(bundle["signed_pre_key_signature"]))
    sign_bit = bool(signature[63] >> 7)
    signature[63] &= 0x7F
    pre_keys = {unb64(k): int(i) for i, k in bundle["pre_keys"].items()}
    return BundleImpl(
        jid,
        device,
        x3dh.Bundle(
            xeddsa.curve25519_pub_to_ed25519_pub(unb64(bundle["identity_key"]), sign_bit),
            unb64(bundle["signed_pre_key"]),
            bytes(signature),
            frozenset(pre_keys),
        ),
        bundle["signed_pre_key_id"],
        pre_keys,
    )


async def main() -> None:
    device = Device()
    commands = {
        "start": device.start,
        "bundle": device.bundle,
        "encrypt": device.encrypt,
        "decrypt": device.decrypt,
        "bundle_xml": device.bundle_xml,
        "device_list_xml": device.device_list_xml,
        "parse_device_list_xml": device.parse_device_list_xml,
        "encrypt_xml": device.encrypt_xml,
        "decrypt_xml": device.decrypt_xml,
    }
    for line in sys.stdin:
        request = json.loads(line)
        try:
            answer = await commands[request.pop("cmd")](**request)
        except Exception as error:  # the test decides whether a failure was expected
            answer = {"error": f"{type(error).__name__}: {error}"}
        print(json.dumps(answer), flush=True)


asyncio.run(main())
