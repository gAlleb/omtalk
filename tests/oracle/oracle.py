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
import secrets
import sys

import x3dh
import xeddsa
from omemo.session import Initiation
from omemo.storage import Just, Maybe, Nothing, Storage
from oldmemo.oldmemo import (
    BundleImpl,
    ContentImpl,
    EncryptedKeyMaterialImpl,
    KeyExchangeImpl,
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

    async def encrypt(self, jid: str, device: int, bundle: dict | None, plaintext: str | None,
                      key_material: str | None) -> dict:
        if plaintext is not None:
            content, plain = await self.backend.encrypt_plaintext(unb64(plaintext))
        else:
            material = unb64(key_material)
            content, plain = None, PlainKeyMaterialImpl(material[:16], material[16:])

        session = await self.backend.load_session(jid, device)
        if session is None:
            session, encrypted = await self.backend.build_session_active(
                jid, device, parse_bundle(jid, device, bundle), plain)
        else:
            encrypted = await self.backend.encrypt_key_material(session, plain)
        await self.backend.store_session(session)

        data, prekey = encrypted.serialize(), False
        if session.initiation is Initiation.ACTIVE and not session.confirmed:
            data, _ = session.key_exchange.serialize(data)
            prekey = True

        answer = {"key": b64(data), "prekey": prekey}
        if content is not None:
            answer |= {"iv": b64(content.initialization_vector), "payload": b64(content.ciphertext)}
        return answer

    async def decrypt(self, jid: str, device: int, key: str, prekey: bool, iv: str | None = None,
                      payload: str | None = None) -> dict:
        data = unb64(key)
        session = await self.backend.load_session(jid, device)
        if prekey:
            exchange, authenticated = KeyExchangeImpl.parse(data, False)
            encrypted = EncryptedKeyMaterialImpl.parse(authenticated, OWN_JID, self.device_id)
            if session is None or not (session.initiation is Initiation.PASSIVE
                                       and session.key_exchange.builds_same_session(exchange)):
                session, plain = await self.backend.build_session_passive(jid, device, exchange, encrypted)
            else:
                plain = await self.backend.decrypt_key_material(session, encrypted)
        else:
            if session is None:
                raise RuntimeError("no session")
            encrypted = EncryptedKeyMaterialImpl.parse(data, OWN_JID, self.device_id)
            plain = await self.backend.decrypt_key_material(session, encrypted)

        answer = {"key_material": b64(plain.key + plain.auth_tag)}
        if payload is not None:
            plaintext = await self.backend.decrypt_plaintext(ContentImpl(unb64(payload), unb64(iv)), plain)
            answer["plaintext"] = b64(plaintext)

        await self.backend.store_session(session)
        if prekey:
            await self.backend.delete_pre_key(session)
        return answer


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
    }
    for line in sys.stdin:
        request = json.loads(line)
        try:
            answer = await commands[request.pop("cmd")](**request)
        except Exception as error:  # the test decides whether a failure was expected
            answer = {"error": f"{type(error).__name__}: {error}"}
        print(json.dumps(answer), flush=True)


asyncio.run(main())
