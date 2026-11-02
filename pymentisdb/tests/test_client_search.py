"""``compact`` on ranked and federated search through the client.

Responses are recorded from the real server (see ``test_compact_types.py``);
the HTTP layer is mocked, so nothing here opens a socket.
"""

import importlib.util
import json
from datetime import datetime, timezone
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
DfScope = client_module.DfScope
DfCutoffs = client_module.DfCutoffs
VectorFusion = client_module.VectorFusion
GraphOptions = client_module.GraphOptions
GraphMode = sys.modules["pymentisdb.types"].GraphMode


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


class RankingKnobTests(unittest.TestCase):
    """Graph, point-in-time and ranking knobs reach the request, unset ones do not."""

    KNOBS = dict(
        graph=GraphOptions(max_depth=2, mode=GraphMode.BIDIRECTIONAL, include_seeds=True),
        as_of=datetime(2026, 1, 2, 3, 4, 5, tzinfo=timezone.utc),
        include_invalidated=True,
        df_scope=DfScope.FIELD,
        df_cutoffs=DfCutoffs(content=0.5, agent_registry=0.9),
        vector_fusion="absolute",
        resolve_successors=False,
        collapse_duplicates=0.8,
        synonym_weight=0.0,
    )
    EXPECTED = {
        "graph": {"max_depth": 2, "mode": "bidirectional", "include_seeds": True},
        "as_of": "2026-01-02T03:04:05+00:00",
        "include_invalidated": True,
        "df_scope": "field",
        "df_cutoffs": {"content": 0.5, "agent_registry": 0.9},
        "vector_fusion": "absolute",
        "resolve_successors": False,
        "collapse_duplicates": 0.8,
        "synonym_weight": 0.0,
    }

    def test_ranked_search_sends_every_knob_by_its_rest_name(self) -> None:
        client = client_returning(recorded("ranked_full"))
        client.ranked_search(text="q", chain_key="c", **self.KNOBS)
        sent = client._post.call_args.args[1]
        self.assertEqual({k: sent[k] for k in self.EXPECTED}, self.EXPECTED)

    def test_federated_search_and_context_bundles_send_them_too(self) -> None:
        client = client_returning(recorded("federated_full"))
        client.federated_search(["a", "b"], text="q", compact=True, **self.KNOBS)
        sent = client._post.call_args.args[1]
        self.assertEqual({k: sent[k] for k in self.EXPECTED}, self.EXPECTED)
        self.assertIs(sent["compact"], True)

        bundles = MentisDbClient()
        bundles._post = MagicMock(return_value={"total_bundles": 0, "consumed_hits": 0, "bundles": []})
        bundles.context_bundles(text="q", **self.KNOBS)
        sent = bundles._post.call_args.args[1]
        self.assertEqual({k: sent[k] for k in self.EXPECTED}, self.EXPECTED)
        self.assertNotIn("compact", sent)

    def test_unset_knobs_are_not_sent(self) -> None:
        client = client_returning(recorded("ranked_full"))
        client.ranked_search(text="q")
        self.assertEqual(client._post.call_args.args[1], {"chain_key": None, "text": "q"})

    def test_knobs_are_keyword_only(self) -> None:
        import inspect

        for method in (MentisDbClient.ranked_search, MentisDbClient.federated_search,
                       MentisDbClient.context_bundles):
            parameters = inspect.signature(method).parameters
            for knob in self.EXPECTED:
                self.assertEqual(parameters[knob].kind, inspect.Parameter.KEYWORD_ONLY, knob)

    def test_unknown_enum_names_fail_locally(self) -> None:
        client = client_returning(recorded("ranked_full"))
        with self.assertRaises(ValueError):
            client.ranked_search(text="q", df_scope="everywhere")
        with self.assertRaises(ValueError):
            client.ranked_search(text="q", vector_fusion="weird")
        client._post.assert_not_called()

    def test_empty_cutoffs_and_graph_options_serialize_to_empty_objects(self) -> None:
        self.assertEqual(DfCutoffs().to_dict(), {})
        self.assertEqual(GraphOptions().to_dict(), {})
        self.assertEqual(GraphOptions(mode="outgoing").to_dict(), {"mode": "outgoing"})


if __name__ == "__main__":
    unittest.main()
