"""
Read proofs for private MentisDB chains.

A client that holds an Ed25519 key listed in a chain's access policy signs each
request locally and sends the result in ``X-Mentis-Auth``. The private key never
leaves the process. The header format and the signed bytes are specified in
``docs/CHAIN_ACCESS.md`` ("Read authentication") and are byte-identical to
``mentisdb::read_auth`` in the Rust crate.

Signing needs the optional ``cryptography`` package::

    pip install pymentisdb[signing]
"""

from __future__ import annotations

import hashlib
import os
import secrets
import struct
import time
from typing import Optional

READ_AUTH_HEADER = "X-Mentis-Auth"
READ_AUTH_DOMAIN = b"mentisdb/read-auth/v1\x00"
ENV_READ_KEY_FILE = "MENTISDB_READ_KEY_FILE"
ENV_READ_AGENT_ID = "MENTISDB_READ_AGENT_ID"
ENV_READ_KEY_ID = "MENTISDB_READ_KEY_ID"
MAX_ID_LEN = 256

_UNRESERVED = frozenset(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._~-"
)


def _pct_encode(text: str) -> str:
    return "".join(
        chr(b) if b in _UNRESERVED else f"%{b:02X}" for b in text.encode("utf-8")
    )


def _string(value: str) -> bytes:
    raw = value.encode("utf-8")
    return struct.pack(">I", len(raw)) + raw


def read_auth_payload(
    method: str,
    target: str,
    body: bytes,
    agent_id: str,
    key_id: str,
    public_key: bytes,
    timestamp: int,
    nonce: str,
) -> bytes:
    """The exact bytes a read proof signs."""
    return (
        READ_AUTH_DOMAIN
        + _string(method.upper())
        + _string(target)
        + hashlib.sha256(body).digest()
        + _string(agent_id)
        + _string(key_id)
        + public_key
        + struct.pack(">q", timestamp)
        + _string(nonce)
    )


class ReadSigner:
    """Holds the Ed25519 seed and signs requests. The seed stays in this object."""

    def __init__(self, seed: bytes, agent_id: str, key_id: str):
        if len(seed) != 32:
            raise ValueError("an Ed25519 seed is exactly 32 bytes")
        if not agent_id or not key_id:
            raise ValueError("agent_id and key_id are required")
        if len(agent_id.encode()) > MAX_ID_LEN or len(key_id.encode()) > MAX_ID_LEN:
            raise ValueError(f"agent_id and key_id are at most {MAX_ID_LEN} bytes")
        try:
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric.ed25519 import (
                Ed25519PrivateKey,
            )
        except ImportError as error:  # pragma: no cover - depends on the environment
            raise ImportError(
                "read proofs need the 'cryptography' package: pip install pymentisdb[signing]"
            ) from error
        self._key = Ed25519PrivateKey.from_private_bytes(seed)
        self._public = self._key.public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw
        )
        self.agent_id = agent_id
        self.key_id = key_id

    def __repr__(self) -> str:  # never show the seed
        return f"ReadSigner({self.agent_id}/{self.key_id})"

    @classmethod
    def from_file(cls, path: str, agent_id: str, key_id: str) -> "ReadSigner":
        """Load a seed file written by ``mentisdb key generate`` (64 hex characters)."""
        with open(path, "r", encoding="utf-8") as handle:
            text = handle.read().strip()
        try:
            seed = bytes.fromhex(text)
        except ValueError as error:
            raise ValueError(f"{path} must hold a 32-byte seed as 64 hex characters") from error
        return cls(seed, agent_id, key_id)

    @classmethod
    def from_env(cls) -> Optional["ReadSigner"]:
        """Build a signer from MENTISDB_READ_KEY_FILE / _AGENT_ID / _KEY_ID, or None."""
        path = os.environ.get(ENV_READ_KEY_FILE, "").strip()
        if not path:
            return None
        agent = os.environ.get(ENV_READ_AGENT_ID, "").strip()
        key = os.environ.get(ENV_READ_KEY_ID, "").strip()
        if not agent or not key:
            raise ValueError(
                f"{ENV_READ_KEY_FILE} also needs {ENV_READ_AGENT_ID} and {ENV_READ_KEY_ID}"
            )
        return cls.from_file(path, agent, key)

    def header(
        self,
        method: str,
        target: str,
        body: bytes = b"",
        timestamp: Optional[int] = None,
        nonce: Optional[str] = None,
    ) -> str:
        """The ``X-Mentis-Auth`` value for one request.

        ``target`` is the path plus ``?query`` exactly as sent; ``body`` is the
        complete request body (empty for GET).
        """
        ts = int(time.time()) if timestamp is None else timestamp
        nonce = nonce or secrets.token_hex(16)
        if not (16 <= len(nonce) <= 64) or any(c not in "0123456789abcdef" for c in nonce):
            raise ValueError("nonce must be 16 to 64 lowercase hex characters")
        payload = read_auth_payload(
            method, target, body, self.agent_id, self.key_id, self._public, ts, nonce
        )
        signature = self._key.sign(payload)
        return (
            f"v1 agent={_pct_encode(self.agent_id)};key={_pct_encode(self.key_id)};"
            f"pk={self._public.hex()};ts={ts};nonce={nonce};sig={signature.hex()}"
        )
