"""Parsing of full and compact search responses.

The fixtures in ``tests/fixtures`` are responses recorded from the real server
(``rest_router`` of MentisDB at the commit that added ``compact``), one pair
per scenario: the same request in the default form (``*_full.json``) and with
``"compact": true`` (``*_compact.json``). Each file holds ``endpoint``,
``request`` and ``response``. They cover plain ranked search, graph expansion,
successor redirection, history (``include_invalidated``), a contested thought,
collapsed duplicates, a signed thought and federated search.
"""

import glob
import importlib.util
import json
import pathlib
import sys
import types
import unittest

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


T = sys.modules.get("pymentisdb.types") or _load_module("pymentisdb.types", "types.py")


def fixture(name: str) -> dict:
    return json.loads((FIXTURES / f"{name}.json").read_text())["response"]


def scenarios() -> list[str]:
    names = sorted(pathlib.Path(p).name[: -len("_full.json")] for p in glob.glob(str(FIXTURES / "*_full.json")))
    assert names, "no fixtures recorded"
    return names


class FullResponseTests(unittest.TestCase):
    """Default responses parse exactly as before and now expose trust fields."""

    def test_every_recorded_full_response_parses(self) -> None:
        for name in scenarios():
            with self.subTest(name):
                response = T.RankedSearchResponse.from_dict(fixture(f"{name}_full"))
                self.assertFalse(response.compact)
                self.assertTrue(response.results)
                for hit in response.results:
                    self.assertIsInstance(hit, T.RankedSearchHit)
                    self.assertIsInstance(hit.score, T.RankedSearchScore)
                    self.assertIsInstance(hit.thought, T.Thought)
                    self.assertTrue(hit.thought.hash)
                    self.assertEqual(hit.total_score, hit.score.total)

    def test_full_hit_keeps_every_field_it_had_before(self) -> None:
        raw = fixture("ranked_full")["results"][0]
        hit = T.RankedSearchHit.from_dict(raw)
        self.assertEqual(hit.chain_key, "fx-main")
        self.assertEqual(hit.matched_terms, raw["matched_terms"])
        self.assertEqual(hit.match_sources, raw["match_sources"])
        self.assertAlmostEqual(hit.score.lexical, raw["score"]["lexical"])
        self.assertEqual(hit.thought.prev_hash, raw["thought"]["prev_hash"])
        self.assertEqual(hit.thought.hash, raw["thought"]["hash"])
        self.assertEqual(hit.thought.tags, raw["thought"]["tags"])
        self.assertIsNone(hit.graph_distance)
        self.assertEqual(hit.graph_seed_paths, 0)

    def test_full_thought_exposes_provenance_and_version_fields(self) -> None:
        signed = [
            h.thought
            for h in T.RankedSearchResponse.from_dict(fixture("ranked_full")).results
            if h.thought.provenance and h.thought.provenance.signed
        ]
        self.assertEqual(len(signed), 1)
        provenance = signed[0].provenance
        self.assertEqual(provenance.segment, "public")
        self.assertEqual(provenance.signature, "valid")
        self.assertEqual(provenance.verified_agent, "signer")
        self.assertEqual(provenance.trust, "untrusted_data")
        self.assertIsNotNone(signed[0].thought_signature)

        history = T.RankedSearchResponse.from_dict(fixture("versions_history_full")).results
        old = next(h.thought for h in history if h.thought.version_status == "superseded")
        self.assertEqual(old.current_version["kind"], "supersedes")
        self.assertEqual(old.successors[0]["thought_id"], old.current_version["thought_id"])

    def test_thought_without_the_new_fields_still_parses(self) -> None:
        thought = T.Thought.from_dict(
            {
                "id": "t1", "index": 0, "timestamp": "2026-04-16T12:00:00",
                "thought_type": "Insight", "content": "old server", "agent_id": "a",
                "prev_hash": "", "hash": "h",
            }
        )
        self.assertIsNone(thought.provenance)
        self.assertIsNone(thought.version_status)
        self.assertEqual(thought.successors, [])
        self.assertEqual(thought.collapsed_duplicates, [])

    def test_server_timestamp_with_z_suffix_parses(self) -> None:
        parsed = T._parse_timestamp("2026-11-01T23:03:19.669167Z")
        self.assertEqual(parsed.utcoffset().total_seconds(), 0)
        self.assertEqual(parsed.microsecond, 669167)


class CompactResponseTests(unittest.TestCase):
    """Compact responses parse into explicit compact types."""

    def test_every_recorded_compact_response_parses(self) -> None:
        for name in scenarios():
            with self.subTest(name):
                response = T.CompactRankedSearchResponse.from_dict(fixture(f"{name}_compact"))
                self.assertTrue(response.compact)
                self.assertTrue(response.results)
                for hit in response.results:
                    self.assertIsInstance(hit, T.CompactRankedSearchHit)
                    self.assertIsInstance(hit.score, float)
                    self.assertIsInstance(hit.thought, T.CompactThought)
                    self.assertEqual(hit.total_score, hit.score)

    def test_compact_types_do_not_pretend_to_have_dropped_fields(self) -> None:
        hit = T.CompactRankedSearchResponse.from_dict(fixture("ranked_compact")).results[0]
        for gone in ("hash", "prev_hash", "thought_signature", "signing_key_id",
                     "schema_version", "agent_name", "agent_owner"):
            self.assertFalse(hasattr(hit.thought, gone), gone)
        for gone in ("matched_terms", "match_sources"):
            self.assertFalse(hasattr(hit, gone), gone)

    def test_omitted_empty_fields_read_as_their_empty_value(self) -> None:
        raw = fixture("ranked_compact")["results"][0]["thought"]
        for omitted in ("confidence", "session_id", "entity_type", "source_episode"):
            self.assertNotIn(omitted, raw)
        thought = T.CompactThought.from_dict(raw)
        self.assertIsNone(thought.confidence)
        self.assertIsNone(thought.session_id)
        self.assertIsNone(thought.entity_type)
        self.assertEqual(thought.refs, [])
        self.assertEqual(thought.extra, {})

    def test_zero_false_and_empty_content(self) -> None:
        thought = T.CompactThought.from_dict(
            {
                "id": "t", "index": 0, "timestamp": "2026-04-16T12:00:00Z",
                "thought_type": "Insight", "agent_id": "a", "role": "Memory",
                "importance": 0.0, "confidence": 0.0,
            }
        )
        self.assertEqual(thought.index, 0)
        self.assertEqual(thought.importance, 0.0)
        self.assertEqual(thought.confidence, 0.0)
        self.assertEqual(thought.content, "")

    def test_missing_always_sent_field_is_an_error(self) -> None:
        raw = dict(fixture("ranked_compact")["results"][0]["thought"])
        del raw["id"]
        with self.assertRaises(KeyError):
            T.CompactThought.from_dict(raw)

    def test_unknown_server_field_is_kept_in_extra(self) -> None:
        raw = dict(fixture("ranked_compact")["results"][0]["thought"])
        raw["a_future_field"] = {"weight": 2}
        thought = T.CompactThought.from_dict(raw)
        self.assertEqual(thought.extra, {"a_future_field": {"weight": 2}})

    def test_graph_fields_only_on_graph_expanded_hits(self) -> None:
        plain = T.CompactRankedSearchResponse.from_dict(fixture("ranked_compact")).results
        for hit in plain:
            self.assertIsNone(hit.graph_distance)
            self.assertIsNone(hit.graph_path)
        graph = T.CompactRankedSearchResponse.from_dict(fixture("ranked_graph_compact")).results
        for hit in graph:
            self.assertIsNotNone(hit.graph_distance)
            self.assertIsNotNone(hit.graph_seed_paths)
            self.assertIsInstance(hit.graph_relation_kinds, list)
            self.assertIn("seed", hit.graph_path)


class FullCompactAgreementTests(unittest.TestCase):
    """For the same request, compact is the full response minus what it drops."""

    def test_compact_hits_agree_with_their_full_twins(self) -> None:
        for name in scenarios():
            with self.subTest(name):
                full = T.RankedSearchResponse.from_dict(fixture(f"{name}_full"))
                compact = T.CompactRankedSearchResponse.from_dict(fixture(f"{name}_compact"))
                self.assertEqual(full.backend, compact.backend)
                self.assertEqual(full.total, compact.total)
                self.assertEqual(len(full.results), len(compact.results))
                for f, c in zip(full.results, compact.results):
                    self.assertEqual(f.chain_key, c.chain_key)
                    self.assertAlmostEqual(f.total_score, c.total_score, places=4)
                    self.assertEqual(f.graph_distance, c.graph_distance)
                    self.assertEqual(f.graph_path, c.graph_path)
                    ft, ct = f.thought, c.thought
                    self.assertEqual(ft.id, ct.id)
                    self.assertEqual(ft.index, ct.index)
                    self.assertEqual(ft.timestamp, ct.timestamp)
                    self.assertEqual(ft.content, ct.content)
                    self.assertEqual(ft.tags, ct.tags)
                    self.assertEqual(ft.concepts, ct.concepts)
                    self.assertEqual(ft.refs, ct.refs)
                    self.assertEqual(ft.relations, ct.relations)
                    self.assertEqual(ft.confidence, ct.confidence)
                    self.assertAlmostEqual(ft.importance, ct.importance, places=6)
                    self.assertEqual(ft.provenance, ct.provenance)
                    self.assertEqual(ft.version_status, ct.version_status)
                    self.assertEqual(ft.successors, ct.successors)
                    self.assertEqual(ft.current_version, ct.current_version)
                    self.assertEqual(ft.successor_of, ct.successor_of)
                    self.assertEqual(ft.collapsed_duplicates, ct.collapsed_duplicates)

    def test_scenarios_cover_what_compact_must_keep(self) -> None:
        seen = {"signed": 0, "superseded": 0, "contested": 0, "successor_of": 0,
                "collapsed": 0, "graph": 0, "relations": 0, "federated": 0}
        for name in scenarios():
            for hit in T.CompactRankedSearchResponse.from_dict(fixture(f"{name}_compact")).results:
                t = hit.thought
                seen["signed"] += bool(t.provenance and t.provenance.signature == "valid")
                seen["superseded"] += t.version_status == "superseded"
                seen["contested"] += t.version_status == "contested"
                seen["successor_of"] += bool(t.successor_of)
                seen["collapsed"] += bool(t.collapsed_duplicates)
                seen["graph"] += hit.graph_path is not None
                seen["relations"] += bool(t.relations)
                seen["federated"] += name == "federated"
        for key, count in seen.items():
            self.assertGreater(count, 0, f"no fixture covers {key}")


if __name__ == "__main__":
    unittest.main()
