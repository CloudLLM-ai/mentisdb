import unittest
from unittest.mock import MagicMock
import importlib.util
import pathlib
import sys
import types

PACKAGE_DIR = pathlib.Path(__file__).resolve().parents[1]

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


types_module = _load_module("pymentisdb.types", "types.py")
client_module = _load_module("pymentisdb.client", "client.py")

MentisDbClient = client_module.MentisDbClient
MemoryScope = types_module.MemoryScope
ThoughtRole = types_module.ThoughtRole
ThoughtType = types_module.ThoughtType


class MentisDbClientTests(unittest.TestCase):
    def test_append_thought_uses_thought_input_payload(self) -> None:
        client = MentisDbClient()
        client._post = MagicMock(
            return_value={
                "thought": {
                    "id": "thought-1",
                    "index": 0,
                    "timestamp": "2026-04-16T12:00:00",
                    "thought_type": "Insight",
                    "content": "payload",
                    "agent_id": "agent-1",
                    "prev_hash": "",
                    "hash": "abc123",
                    "role": "Memory",
                    "tags": ["alpha"],
                    "concepts": ["beta"],
                    "refs": [2],
                }
            }
        )

        client.append_thought(
            thought_type=ThoughtType.INSIGHT,
            content="payload",
            chain_key="chain-a",
            agent_id="agent-1",
            agent_name="Agent One",
            agent_owner="team",
            role=ThoughtRole.MEMORY,
            importance=0.9,
            confidence=0.7,
            tags=["alpha"],
            concepts=["beta"],
            refs=[2],
            entity_type="note",
            source_episode="episode-1",
            scope=MemoryScope.SESSION,
        )

        client._post.assert_called_once_with(
            "/v1/thoughts",
            {
                "chain_key": "chain-a",
                "thought_type": "Insight",
                "content": "payload",
                "agent_id": "agent-1",
                "agent_name": "Agent One",
                "agent_owner": "team",
                "role": "Memory",
                "importance": 0.9,
                "confidence": 0.7,
                "tags": ["alpha"],
                "concepts": ["beta"],
                "refs": [2],
                "entity_type": "note",
                "source_episode": "episode-1",
                "scope": "session",
            },
        )

    def test_ranked_search_accepts_memory_scope_enum(self) -> None:
        client = MentisDbClient()
        client._post = MagicMock(return_value={"backend": "hybrid_graph", "total": 0, "results": []})

        client.ranked_search(text="query", scope=MemoryScope.AGENT)

        client._post.assert_called_once_with(
            "/v1/ranked-search",
            {"chain_key": None, "text": "query", "scope": "agent"},
        )

    def test_context_bundles_decodes_seed_and_support_types(self) -> None:
        client = MentisDbClient()
        client._post = MagicMock(
            return_value={
                "total_bundles": 1,
                "consumed_hits": 2,
                "bundles": [
                    {
                        "seed": {
                            "locator": {
                                "chain_key": "chain-a",
                                "thought_id": "seed-1",
                                "thought_index": 3,
                            },
                            "lexical_score": 1.25,
                            "matched_terms": ["alpha"],
                            "thought": {
                                "id": "seed-1",
                                "index": 3,
                                "timestamp": "2026-04-16T12:00:00",
                                "thought_type": "Decision",
                                "content": "seed thought",
                                "agent_id": "planner",
                                "prev_hash": "prev",
                                "hash": "seedhash",
                                "role": "Memory",
                            },
                        },
                        "support": [
                            {
                                "locator": {
                                    "chain_key": "chain-a",
                                    "thought_id": "support-1",
                                    "thought_index": 4,
                                },
                                "thought": {
                                    "id": "support-1",
                                    "index": 4,
                                    "timestamp": "2026-04-16T12:00:01",
                                    "thought_type": "Summary",
                                    "content": "support thought",
                                    "agent_id": "planner",
                                    "prev_hash": "seedhash",
                                    "hash": "supporthash",
                                    "role": "Memory",
                                },
                                "depth": 1,
                                "seed_path_count": 2,
                                "relation_kinds": ["references"],
                            }
                        ],
                    }
                ],
            }
        )

        response = client.context_bundles(text="alpha", scope=MemoryScope.USER)

        self.assertEqual(response.total_bundles, 1)
        self.assertEqual(response.consumed_hits, 2)
        self.assertEqual(len(response.bundles), 1)
        bundle = response.bundles[0]
        self.assertEqual(bundle.seed.thought_id, "seed-1")
        self.assertEqual(bundle.seed.thought.content, "seed thought")
        self.assertEqual(len(bundle.support), 1)
        self.assertEqual(bundle.support[0].thought_id, "support-1")
        self.assertEqual(bundle.support[0].thought.content, "support thought")
        self.assertEqual(bundle.support[0].relation_kinds, ["references"])

        client._post.assert_called_once_with(
            "/v1/context-bundles",
            {"chain_key": None, "text": "alpha", "scope": "user"},
        )


if __name__ == "__main__":
    unittest.main()
