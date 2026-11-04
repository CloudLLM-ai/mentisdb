import importlib.util
import json
import pathlib
import sys
import types
import unittest
from unittest.mock import MagicMock

PACKAGE_DIR = pathlib.Path(__file__).resolve().parents[1]

if "pymentisdb" not in sys.modules:
    package = types.ModuleType("pymentisdb")
    package.__path__ = [str(PACKAGE_DIR)]
    sys.modules["pymentisdb"] = package


def _load(name, file_name):
    spec = importlib.util.spec_from_file_location(name, PACKAGE_DIR / file_name)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


types_module = sys.modules.get("pymentisdb.types") or _load("pymentisdb.types", "types.py")
read_auth = sys.modules.get("pymentisdb.read_auth") or _load("pymentisdb.read_auth", "read_auth.py")
client_module = sys.modules.get("pymentisdb.client") or _load("pymentisdb.client", "client.py")

# Same vector as `the_pinned_cross_language_vector_matches` in tests/read_auth_tests.rs.
PINNED_VECTOR = (
    "v1 agent=alice%20smith;key=alice-1;"
    "pk=ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c;"
    "ts=1767225600;nonce=00112233445566778899aabbccddeeff;"
    "sig=34598ee03953d6edb1eb52cc9853b9af891745d392e94123b1708a64dcb4ff8ab9d86bed01b1773ef79542e7ac3eff0e6410d846a94b29db517c33f504706b0e"
)
BODY = b'{"chain_key":"vault","text":"launch"}'


class ReadAuthTests(unittest.TestCase):
    def signer(self):
        return read_auth.ReadSigner(bytes([7] * 32), "alice smith", "alice-1")

    def test_matches_the_rust_implementation_byte_for_byte(self):
        header = self.signer().header(
            "post", "/v1/ranked-search?x=1", BODY, timestamp=1767225600,
            nonce="00112233445566778899aabbccddeeff",
        )
        self.assertEqual(header, PINNED_VECTOR)

    def test_every_request_gets_a_fresh_nonce_and_binds_the_body(self):
        s = self.signer()
        a, b = s.header("POST", "/v1/search", b"{}"), s.header("POST", "/v1/search", b"{}")
        self.assertNotEqual(a, b)
        c = s.header("POST", "/v1/search", b"{ }", timestamp=1, nonce="a" * 16)
        d = s.header("POST", "/v1/search", b"{}", timestamp=1, nonce="a" * 16)
        self.assertNotEqual(c, d)

    def test_inputs_are_validated_and_the_seed_is_not_shown(self):
        with self.assertRaises(ValueError):
            read_auth.ReadSigner(b"short", "a", "k")
        with self.assertRaises(ValueError):
            read_auth.ReadSigner(bytes(32), "", "k")
        with self.assertRaises(ValueError):
            read_auth.ReadSigner(bytes(32), "a" * 257, "k")
        with self.assertRaises(ValueError):
            self.signer().header("GET", "/", nonce="XYZ")
        self.assertNotIn("07", repr(self.signer()))

    def test_loading_from_a_key_file_and_the_environment(self):
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "k.key"
            path.write_text("07" * 32 + "\n")
            s = read_auth.ReadSigner.from_file(str(path), "alice smith", "alice-1")
            self.assertEqual(
                s.header("POST", "/v1/ranked-search?x=1", BODY, timestamp=1767225600,
                         nonce="00112233445566778899aabbccddeeff"),
                PINNED_VECTOR,
            )
            path.write_text("not hex")
            with self.assertRaises(ValueError):
                read_auth.ReadSigner.from_file(str(path), "a", "k")
            env = {
                read_auth.ENV_READ_KEY_FILE: str(path),
            }
            old = {k: os.environ.get(k) for k in (*env, read_auth.ENV_READ_AGENT_ID, read_auth.ENV_READ_KEY_ID)}
            try:
                os.environ.update(env)
                os.environ.pop(read_auth.ENV_READ_AGENT_ID, None)
                os.environ.pop(read_auth.ENV_READ_KEY_ID, None)
                with self.assertRaises(ValueError):
                    read_auth.ReadSigner.from_env()
            finally:
                for k, v in old.items():
                    if v is None:
                        os.environ.pop(k, None)
                    else:
                        os.environ[k] = v
            self.assertIsNone(read_auth.ReadSigner.from_env())

    def test_client_signs_exactly_the_bytes_it_sends(self):
        client = client_module.MentisDbClient("http://127.0.0.1:9472/", read_signer=self.signer())
        response = MagicMock()
        response.json.return_value = {}
        client._session = MagicMock()
        client._session.post.return_value = response
        client._post("/v1/head", {"chain_key": "vault"})
        _, kwargs = client._session.post.call_args
        sent = kwargs["data"]
        self.assertEqual(json.loads(sent), {"chain_key": "vault"})
        header = kwargs["headers"][read_auth.READ_AUTH_HEADER]
        # Recompute the signature for the bytes and target that were sent.
        fields = dict(part.split("=", 1) for part in header[3:].split(";"))
        payload = read_auth.read_auth_payload(
            "POST", "/v1/head", sent, "alice smith", "alice-1",
            bytes.fromhex(fields["pk"]), int(fields["ts"]), fields["nonce"],
        )
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

        Ed25519PublicKey.from_public_bytes(bytes.fromhex(fields["pk"])).verify(
            bytes.fromhex(fields["sig"]), payload
        )

    def test_client_declares_the_digest_of_the_body_it_signed(self):
        import hashlib

        client = client_module.MentisDbClient("http://127.0.0.1:9472/", read_signer=self.signer())
        response = MagicMock()
        response.json.return_value = {}
        client._session = MagicMock()
        client._session.post.return_value = response
        client._post("/v1/import-markdown", {"chain_key": "vault", "markdown": "x" * 3_000_000})
        _, kwargs = client._session.post.call_args
        declared = kwargs["headers"][read_auth.CONTENT_DIGEST_HEADER]
        self.assertEqual(declared, hashlib.sha256(kwargs["data"]).hexdigest())
        self.assertRegex(declared, r"^[0-9a-f]{64}$")
        # The pinned proof is unchanged by the extra header.
        self.assertEqual(
            self.signer().header("post", "/v1/ranked-search?x=1", BODY, timestamp=1767225600,
                                 nonce="00112233445566778899aabbccddeeff"),
            PINNED_VECTOR,
        )

    def test_requests_never_follow_redirects(self):
        client = client_module.MentisDbClient("http://127.0.0.1:9472/", read_signer=self.signer())
        client._session = MagicMock()
        client._session.post.return_value = MagicMock()
        client._session.get.return_value = MagicMock()
        client._post("/v1/head", {})
        client._get("/v1/chains")
        self.assertIs(client._session.post.call_args.kwargs["allow_redirects"], False)
        self.assertIs(client._session.get.call_args.kwargs["allow_redirects"], False)

    def test_a_redirect_is_an_error_not_followed(self):
        import requests

        client = client_module.MentisDbClient("http://127.0.0.1:9472/", read_signer=self.signer())
        redirect = MagicMock()
        redirect.status_code = 302
        redirect.url = "http://127.0.0.1:9472/v1/head"
        client._session = MagicMock()
        client._session.post.return_value = redirect
        with self.assertRaises(requests.HTTPError) as caught:
            client._post("/v1/head", {})
        self.assertIn("redirect", str(caught.exception))
        redirect.json.assert_not_called()

    def test_without_a_signer_no_header_is_sent(self):
        client = client_module.MentisDbClient()
        client._read_signer = None
        response = MagicMock()
        response.json.return_value = {}
        client._session = MagicMock()
        client._session.get.return_value = response
        client._get("/v1/chains")
        headers = client._session.get.call_args.kwargs["headers"]
        self.assertNotIn(read_auth.READ_AUTH_HEADER, headers)
        self.assertNotIn(read_auth.CONTENT_DIGEST_HEADER, headers)


if __name__ == "__main__":
    unittest.main()
