"""``compact`` on ranked and federated search through the client.

Responses are recorded from the real server (see ``test_compact_types.py``);
the HTTP layer is mocked, so nothing here opens a socket.
"""

import importlib.util
import json
import pathlib
import sys
import types
import unittest
from unittest.mock import MagicMock

PACKAGE_DIR = pathlib.Path(__file__).resolve().parents[1]
FIXTURES = PACKAGE_DIR / "tests" / "fixtures"

if "pymentisdb" not in sys.modules:
    package = types.ModuleType("pymentisdb")
    package.__path__ = [str(PACKAGE_DIR)]
    sys.modules["pymentisdb"] = package


def _load_module(module_name: str, file_name: str):
    spec = importlib.util.spec_from_file_location(module_name, PACKAGE_DIR / file_name)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return module


_load_module("pymentisdb.types", "types.py")
client_module = _load_module("pymentisdb.client", "client.py")

# The classes the client itself returns, whichever test module loaded `types` last.
MentisDbClient = client_module.MentisDbClient
RankedSearchResponse = client_module.RankedSearchResponse
CompactRankedSearchResponse = client_module.CompactRankedSearchResponse


def recorded(name: str) -> dict:
    return json.loads((FIXTURES / f"{name}.json").read_text())["response"]


def client_returning(response: dict) -> MentisDbClient:
    client = MentisDbClient()
    client._post = MagicMock(return_value=response)
    return client


class RankedSearchCompactTests(unittest.TestCase):
    def test_default_sends_no_compact_field_and_returns_the_full_form(self) -> None:
        client = client_returning(recorded("ranked_full"))
        response = client.ranked_search(text="job queue storage", chain_key="fx-main", limit=5)
        client._post.assert_called_once_with(
            "/v1/ranked-search",
            {"chain_key": "fx-main", "text": "job queue storage", "limit": 5},
        )
        self.assertIsInstance(response, RankedSearchResponse)
        self.assertFalse(response.compact)
        self.assertTrue(response.results[0].thought.hash)

    def test_compact_false_is_the_same_as_the_default(self) -> None:
        client = client_returning(recorded("ranked_full"))
        client.ranked_search(text="q", compact=False)
        self.assertNotIn("compact", client._post.call_args.args[1])

    def test_compact_true_is_sent_and_returns_the_compact_form(self) -> None:
        client = client_returning(recorded("ranked_compact"))
        response = client.ranked_search(text="job queue storage", chain_key="fx-main", limit=5, compact=True)
        client._post.assert_called_once_with(
            "/v1/ranked-search",
            {"chain_key": "fx-main", "text": "job queue storage", "limit": 5, "compact": True},
        )
        self.assertIsInstance(response, CompactRankedSearchResponse)
        self.assertTrue(response.compact)
        first = response.results[0]
        self.assertIsInstance(first.score, float)
        self.assertFalse(hasattr(first.thought, "hash"))
        self.assertEqual(first.thought.provenance.trust, "untrusted_data")

    def test_both_forms_answer_total_score_and_thought_content(self) -> None:
        full = client_returning(recorded("ranked_full")).ranked_search(text="q")
        compact = client_returning(recorded("ranked_compact")).ranked_search(text="q", compact=True)
        for f, c in zip(full.results, compact.results):
            self.assertAlmostEqual(f.total_score, c.total_score, places=4)
            self.assertEqual(f.thought.content, c.thought.content)


class FederatedSearchTests(unittest.TestCase):
    def test_federated_search_posts_chain_keys_and_filters(self) -> None:
        client = client_returning(recorded("federated_full"))
        response = client.federated_search(
            ["fx-main", "fx-dup"], text="job queue backup", limit=5, tags_any=["ops"], scope="user"
        )
        client._post.assert_called_once_with(
            "/v1/federated-search",
            {
                "chain_keys": ["fx-main", "fx-dup"],
                "text": "job queue backup",
                "limit": 5,
                "tags_any": ["ops"],
                "scope": "user",
            },
        )
        self.assertIsInstance(response, RankedSearchResponse)
        self.assertEqual({hit.chain_key for hit in response.results}, {"fx-main", "fx-dup"})

    def test_federated_compact(self) -> None:
        client = client_returning(recorded("federated_compact"))
        response = client.federated_search(["fx-main", "fx-dup"], text="job queue backup", compact=True)
        sent = client._post.call_args.args[1]
        self.assertIs(sent["compact"], True)
        self.assertEqual(sent["chain_keys"], ["fx-main", "fx-dup"])
        self.assertIsInstance(response, CompactRankedSearchResponse)
        self.assertEqual({hit.chain_key for hit in response.results}, {"fx-main", "fx-dup"})

    def test_chain_keys_are_copied_not_aliased(self) -> None:
        client = client_returning(recorded("federated_full"))
        keys = ["fx-main"]
        client.federated_search(keys, text="q")
        keys.append("other")
        self.assertEqual(client._post.call_args.args[1]["chain_keys"], ["fx-main"])


if __name__ == "__main__":
    unittest.main()
