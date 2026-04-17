#!/usr/bin/env python3
"""
LoCoMo benchmark adapter for mentisdb.

Tests retrieval recall (R@k) across ~1,977 QA pairs from long social
conversations (up to ~300 turns) spanning weeks of interaction.

Question types (inferred from target count):
  single — single-hop (1 target turn)
  multi  — multi-hop (2+ target turns)

Dataset: Nithish2410/benchmark-locomo (snap-research/locomo compatible)
Reference scores (MemPalace BENCHMARKS.md, 2026):
    Hybrid v5, top-10, no rerank   88.9% R@10
    Hybrid + Sonnet rerank, top-50  100.0% R@5 (caveat: top-50 > session count)

Usage:
    pip install requests
    bash locomo-benches/run_locomo.sh

    # Or manually:
    python3 locomo-benches/locomo_bench.py --top-k 10 --limit 50

    # Dev run:
    python3 locomo-benches/locomo_bench.py --top-k 10 --limit 10

    # Force re-ingest:
    python3 locomo-benches/locomo_bench.py --top-k 10 --force-reingest
"""

import argparse
import json
import re
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

_tls = threading.local()

def _session() -> requests.Session:
    if not hasattr(_tls, "session"):
        _tls.session = requests.Session()
    return _tls.session


DEFAULT_BASE_URL  = "http://127.0.0.1:9472"
DEFAULT_TOP_K     = 10
DEFAULT_WORKERS   = 4
NEAR_MISS_K       = 50


# ---------------------------------------------------------------------------
# Dataset loading
# ---------------------------------------------------------------------------

_ID_RE = re.compile(r"^(q\d+)_(S\w+)_(\d+)_T(\d+)$")


def load_locomo(data_dir: str, limit: int | None) -> tuple[list[dict], dict[str, str], dict[str, list[tuple[int, str]]], list[dict]]:
    items_path = f"{data_dir}/locomo_items.jsonl"
    test_path  = f"{data_dir}/locomo_test.jsonl"

    with open(items_path) as f:
        items = json.loads(f.read())
    with open(test_path) as f:
        queries = json.loads(f.read())

    item_map = {it["id"]: it["text"] for it in items}

    session_map: dict[str, list[tuple[int, str]]] = {}
    for it in items:
        m = _ID_RE.match(it["id"])
        if m:
            sk = f"{m.group(1)}_{m.group(2)}_{m.group(3)}"
            session_map.setdefault(sk, []).append((int(m.group(4)), it["text"]))
    for sk in session_map:
        session_map[sk].sort()

    if limit:
        personas = sorted(set(it["id"].split("_")[0] for it in items))
        kept = set(personas[:limit])
        items = [it for it in items if it["id"].split("_")[0] in kept]
        queries = [q for q in queries if q["id"].split("_")[0] in kept]
        item_map = {it["id"]: it["text"] for it in items}
        # Rebuild session_map on the kept subset so evidence-neighbor
        # expansion in _expand_evidence() cannot pull turns from personas
        # that were filtered out (otherwise R@K scores on --limit runs
        # leak ground truth from the excluded personas).
        kept_ids = set(item_map.keys())
        session_map = {
            sk: [(idx, it_id) for idx, it_id in turns if it_id in kept_ids]
            for sk, turns in session_map.items()
        }
        session_map = {sk: turns for sk, turns in session_map.items() if turns}

    return items, item_map, session_map, queries


def _group_queries_by_persona(queries: list[dict]) -> dict[str, list]:
    groups: dict[str, list] = {}
    for q in queries:
        persona = q["id"].split("_")[0]
        groups.setdefault(persona, []).append(q)
    return groups


# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

def _post(base_url: str, path: str, payload: dict, timeout: int = 30) -> dict:
    r = _session().post(f"{base_url}{path}", json=payload, timeout=timeout)
    if not r.ok:
        raise requests.HTTPError(
            f"{r.status_code} {r.reason} — body: {r.text[:300]}",
            response=r,
        )
    return r.json()


def _get(base_url: str, path: str, timeout: int = 10) -> dict:
    r = _session().get(f"{base_url}{path}", timeout=timeout)
    r.raise_for_status()
    return r.json()


def chain_exists(base_url: str, chain_key: str) -> bool:
    try:
        data = _get(base_url, "/v1/chains")
        return chain_key in data.get("chain_keys", [])
    except Exception:
        return False


def chain_thought_count(base_url: str, chain_key: str) -> int:
    try:
        data = _get(base_url, "/v1/chains")
        for c in data.get("chains", []):
            if c.get("chain_key") == chain_key:
                return c.get("thought_count", 0)
    except Exception:
        pass
    return 0


def append_turn(base_url: str, chain_key: str, content: str, speaker: str,
                turn_index: int, prev_id: str | None = None,
                retries: int = 3) -> str:
    importance = 0.8 if speaker == "speaker_a" else 0.2
    payload = {
        "chain_key": chain_key,
        "thought_type": "FactLearned",
        "content": content,
        "agent_id": speaker,
        "importance": importance,
        "tags": [f"speaker:{speaker}", f"turn:{turn_index}"],
    }
    if prev_id:
        payload["relations"] = [{"kind": "ContinuesFrom", "target_id": prev_id}]
    for attempt in range(retries):
        try:
            resp = _post(base_url, "/v1/thoughts", payload)
            return resp["thought"]["id"]
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(0.3 * (attempt + 1))


def rebuild_vectors(base_url: str, chain_key: str) -> None:
    for provider_key in ["fastembed-minilm", "local-text-v1"]:
        try:
            resp = _post(base_url, "/v1/vectors/rebuild", {
                "chain_key": chain_key,
                "provider_key": provider_key,
            }, timeout=600)
            indexed = resp.get("status", {}).get("indexed_thought_count")
            print(f"  [{provider_key}] Vector sidecar rebuilt — {indexed} thoughts indexed.", flush=True)
        except Exception as e:
            print(f"  WARNING: vector rebuild failed ({provider_key}): {e}", flush=True)


def ranked_search(base_url: str, chain_key: str, query: str, limit: int,
                   enable_reranking: bool = False, rerank_k: int = 50) -> list[dict]:
    payload: dict = {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
        "graph": {
            "max_depth": 3,
            "max_visited": 200,
            "include_seeds": False,
        },
    }
    if enable_reranking:
        payload["enable_reranking"] = True
        payload["rerank_k"] = rerank_k
    resp = _post(base_url, "/v1/ranked-search", payload)
    return resp.get("results", [])


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

def ingest_persona(base_url: str, chain_key: str, items: list[dict],
                   persona_prefix: str, workers: int) -> int:
    turns = [it for it in items if it["id"].startswith(persona_prefix + "_")]
    turns.sort(key=lambda it: _sort_key(it["id"]))

    total = len(turns)
    if total == 0:
        return 0

    done_count = [0]
    lock = threading.Lock()

    def _ingest_session(session_turns):
        prev_id = None
        for it, idx in session_turns:
            text = it["text"]
            # Alternate speakers: even turns = speaker_a, odd = speaker_b
            speaker = "speaker_a" if idx % 2 == 0 else "speaker_b"
            prev_id = append_turn(base_url, chain_key, text, speaker, idx, prev_id=prev_id)
            with lock:
                done_count[0] += 1
                d = done_count[0]
                if d % 100 == 0 or d == total:
                    print(f"    {d}/{total} turns ingested", flush=True)
        return prev_id

    # Group turns by session for sequential ingestion within sessions
    sessions: dict[str, list] = {}
    for it in turns:
        m = _ID_RE.match(it["id"])
        if m:
            session_key = f"{m.group(1)}_{m.group(2)}_{m.group(3)}"
            turn_idx = int(m.group(4))
            sessions.setdefault(session_key, []).append((it, turn_idx))

    # Sort each session's turns
    for sk in sessions:
        sessions[sk].sort(key=lambda x: x[1])

    # Ingest sessions sequentially (to maintain ContinuesFrom within sessions)
    # Parallelize across sessions
    if workers <= 1:
        for sk in sorted(sessions.keys()):
            _ingest_session(sessions[sk])
    else:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            futs = [pool.submit(_ingest_session, sessions[sk]) for sk in sorted(sessions.keys())]
            for f in as_completed(futs):
                f.result()

    return total


def _sort_key(item_id: str) -> tuple:
    m = _ID_RE.match(item_id)
    if m:
        return (m.group(1), m.group(2), int(m.group(3)), int(m.group(4)))
    return (item_id,)


# ---------------------------------------------------------------------------
# Evidence matching
# ---------------------------------------------------------------------------

_NEIGHBOR_RADIUS = 2


def _expand_evidence(target_ids: list[str], item_map: dict[str, str],
                     session_map: dict[str, list[tuple[int, str]]]) -> list[str]:
    texts: list[str] = []
    seen: set[str] = set()
    for tid in target_ids:
        direct = item_map.get(tid, "")
        if direct and direct not in seen:
            texts.append(direct)
            seen.add(direct)
        m = _ID_RE.match(tid)
        if m:
            sk = f"{m.group(1)}_{m.group(2)}_{m.group(3)}"
            tnum = int(m.group(4))
            session_turns = session_map.get(sk, [])
            for tn, txt in session_turns:
                if abs(tn - tnum) <= _NEIGHBOR_RADIUS and txt not in seen:
                    texts.append(txt)
                    seen.add(txt)
    return texts


def _hit(evidence_texts: list[str], results: list[dict], k: int) -> bool:
    contents = [r["thought"].get("content", "").strip().lower() for r in results[:k]]
    for ev in evidence_texts:
        ev_l = ev.strip().lower()
        for c in contents:
            if ev_l in c or c in ev_l:
                return True
    return False


# ---------------------------------------------------------------------------
# Evaluation
# ---------------------------------------------------------------------------

def evaluate(base_url: str, chain_key: str, queries: list[dict],
             item_map: dict[str, str],
             session_map: dict[str, list[tuple[int, str]]],
             top_k: int) -> tuple[float, dict, list[dict]]:
    by_type: dict[str, dict] = {}
    misses: list[dict] = []
    fetch_k = max(top_k, NEAR_MISS_K)
    lock = threading.Lock()
    done_count = [0]
    correct_count = [0]

    def _eval_one(q: dict):
        n_targets = len(q["target_ids"])
        qtype = "single" if n_targets == 1 else "multi"

        question = q["query"]
        evidence_texts = _expand_evidence(q["target_ids"], item_map, session_map)
        if not evidence_texts:
            evidence_texts = [item_map.get(tid, "") for tid in q["target_ids"]]
            evidence_texts = [e for e in evidence_texts if e]

        raw = ranked_search(base_url, chain_key, question, fetch_k)

        hit_k  = _hit(evidence_texts, raw, top_k)
        hit_10 = _hit(evidence_texts, raw, 10)
        hit_20 = _hit(evidence_texts, raw, 20)
        hit_50 = _hit(evidence_texts, raw, 50)

        with lock:
            s = by_type.setdefault(qtype, {
                "correct": 0, "total": 0, "hit_10": 0, "hit_20": 0, "hit_50": 0,
            })
            s["total"] += 1
            if hit_k:
                s["correct"] += 1
                correct_count[0] += 1
            if hit_10:
                s["hit_10"] += 1
            if hit_20:
                s["hit_20"] += 1
            if hit_50:
                s["hit_50"] += 1

            if not hit_k:
                top_scores = [r.get("score", {}) for r in raw[:top_k]]
                misses.append({
                    "query_id":    q["id"],
                    "question":    question[:200],
                    "qtype":        qtype,
                    "n_targets":    n_targets,
                    "evidence":    [e[:200] for e in evidence_texts[:5]],
                    "retrieved":   [r["thought"].get("content", "")[:200] for r in raw[:top_k]],
                    "scores":      top_scores,
                    "near_10":     hit_10,
                    "near_20":     hit_20,
                    "near_50":     hit_50,
                })

            done_count[0] += 1
            d = done_count[0]
            if d % 50 == 0 or d == len(queries):
                pct = correct_count[0] / d * 100
                print(f"  {d}/{len(queries)} QAs — R@{top_k}: {pct:.1f}%", flush=True)

    with ThreadPoolExecutor(max_workers=8) as pool:
        futs = [pool.submit(_eval_one, q) for q in queries]
        for f in as_completed(futs):
            f.result()

    total_correct = sum(v["correct"] for v in by_type.values())
    total_q = sum(v["total"] for v in by_type.values())
    overall = total_correct / total_q * 100 if total_q else 0
    return overall, by_type, misses


# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

def print_diagnostics(by_type: dict, misses: list[dict], top_k: int) -> None:
    print(f"\nNear-miss analysis:")
    print(f"  {'type':<12} {'total':>5}  R@{top_k:<3}  R@10  R@20  R@50")
    print(f"  {'-'*55}")
    for qtype, s in sorted(by_type.items()):
        t = s["total"]
        r_k  = s["correct"] / t * 100
        r_10 = s["hit_10"]  / t * 100
        r_20 = s["hit_20"]  / t * 100
        r_50 = s["hit_50"]  / t * 100
        print(f"  {qtype:<12} {t:>5}  {r_k:5.1f}%  {r_10:5.1f}%  {r_20:5.1f}%  {r_50:5.1f}%")

    miss_scores = [m["scores"][0] for m in misses if m["scores"]]
    if miss_scores:
        keys = ["lexical", "vector", "graph", "relation", "seed_support", "recency", "total"]
        print(f"\nAvg top-1 score breakdown on MISSES ({len(miss_scores)} samples):")
        for k in keys:
            vals = [s.get(k, 0) for s in miss_scores if isinstance(s, dict)]
            if vals:
                print(f"  {k:<14} {sum(vals)/len(vals):.4f}")

    near_10 = sum(1 for m in misses if m["near_10"])
    near_20 = sum(1 for m in misses if m["near_20"])
    near_50 = sum(1 for m in misses if m["near_50"])
    total_miss = len(misses)
    if total_miss:
        print(f"\nOf {total_miss} misses:")
        print(f"  {near_10:3d} ({near_10/total_miss*100:5.1f}%) appear in top-10 (ranking problem)")
        print(f"  {near_20:3d} ({near_20/total_miss*100:5.1f}%) appear in top-20")
        print(f"  {near_50:3d} ({near_50/total_miss*100:5.1f}%) appear in top-50")
        print(f"  {total_miss-near_50:3d} ({(total_miss-near_50)/total_miss*100:5.1f}%) not in top-50 (lexical gap)")

    miss_by_type: dict[str, list] = {}
    for m in misses:
        miss_by_type.setdefault(m["qtype"], []).append(m)
    if miss_by_type:
        print(f"\nMiss counts by type:")
        for qtype, ms in sorted(miss_by_type.items(), key=lambda x: len(x[1]), reverse=True):
            total = by_type[qtype]["total"]
            near = sum(1 for m in ms if m["near_10"])
            print(f"  {qtype:<12} {len(ms):>3}/{total:<3} misses  ({near} near top-10)")

        worst_type, worst_misses = max(miss_by_type.items(), key=lambda x: len(x[1]))
        print(f"\nSample misses from '{worst_type}' (5 of {len(worst_misses)}):")
        for m in worst_misses[:5]:
            ev_snip  = (m["evidence"][0][:120] + "…") if m["evidence"] else "(none)"
            ret_snip = (m["retrieved"][0][:120] + "…") if m["retrieved"] else "(nothing)"
            print(f"\n  Q:         {m['question'][:130]}")
            print(f"  Evidence:  {ev_snip}")
            print(f"  Top-1 ret: {ret_snip}")
            if m["scores"]:
                s = m["scores"][0]
                if isinstance(s, dict):
                    print(f"  Score:     lexical={s.get('lexical',0):.3f}  "
                          f"vector={s.get('vector',0):.3f}  "
                          f"graph={s.get('graph',0):.3f}  "
                          f"recency={s.get('recency',0):.3f}  "
                          f"total={s.get('total',0):.3f}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="LoCoMo benchmark for mentisdb")
    ap.add_argument("--top-k", type=int, default=DEFAULT_TOP_K)
    ap.add_argument("--limit", type=int, default=None,
                    help="Ingest only first N persona groups (dev mode; full=10)")
    ap.add_argument("--chain", default=f"locomo-{int(time.time())}")
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS,
                    help="Ingestion workers (default 4)")
    ap.add_argument("--data-dir", default="data",
                    help="Directory containing locomo_items.jsonl and locomo_test.jsonl")
    ap.add_argument("--skip-vectors", action="store_true",
                    help="Skip vector sidecar rebuild after ingestion")
    ap.add_argument("--force-reingest", action="store_true",
                    help="Force re-ingest even if chain already exists")
    ap.add_argument("--reranking", action="store_true",
                    help="Enable RRF reranking in ranked search")
    ap.add_argument("--rerank-k", type=int, default=50,
                    help="Number of candidates to rerank (default 50)")
    ap.add_argument("--output", help="Write per-type JSON results here")
    args = ap.parse_args()

    print(f"\nLoCoMo × mentisdb")
    print(f"  top-k        : {args.top_k}  (also computing R@10, R@20, R@{NEAR_MISS_K})")
    print(f"  limit        : {args.limit or 'full'}")
    print(f"  chain        : {args.chain}")
    print(f"  data-dir     : {args.data_dir}")
    print(f"  endpoint     : {args.base_url}\n")

    items, item_map, session_map, queries = load_locomo(args.data_dir, args.limit)
    print(f"  Loaded {len(items)} conversation turns, {len(queries)} test queries\n")

    # Ingest: one chain per persona group
    personas = sorted(set(it["id"].split("_")[0] for it in items))

    do_ingest = args.force_reingest or not chain_exists(args.base_url, args.chain)

    if do_ingest:
        t0 = time.monotonic()
        total_ingested = 0
        for persona in personas:
            n = ingest_persona(args.base_url, args.chain, items, persona, args.workers)
            total_ingested += n
        print(f"  Ingested {total_ingested} turns in {time.monotonic()-t0:.1f}s\n", flush=True)
        time.sleep(1)

        if not args.skip_vectors:
            print("Building vector sidecar…", flush=True)
            rebuild_vectors(args.base_url, args.chain)
    else:
        count = chain_thought_count(args.base_url, args.chain)
        print(f"  Chain '{args.chain}' already exists ({count} thoughts) — skipping ingestion.\n")

    # Evaluate
    t0 = time.monotonic()
    overall, by_type, misses = evaluate(
        args.base_url, args.chain, queries, item_map, session_map, args.top_k
    )
    elapsed = time.monotonic() - t0

    total_correct = sum(v["correct"] for v in by_type.values())
    total_q = sum(v["total"] for v in by_type.values())

    print(f"\n{'='*55}")
    print(f"LoCoMo  R@{args.top_k}: {overall:.1f}%  ({total_correct}/{total_q})")
    print(f"Evaluation time: {elapsed:.0f}s  ({total_q/elapsed:.1f} q/s)\n")

    print("By question type:")
    for qtype, stats in sorted(by_type.items()):
        pct = stats["correct"] / stats["total"] * 100 if stats["total"] else 0
        bar = "█" * int(pct / 5)
        print(f"  {qtype:<12} {pct:5.1f}%  ({stats['correct']}/{stats['total']})  {bar}")

    print_diagnostics(by_type, misses, args.top_k)

    if args.output:
        with open(args.output, "w") as f:
            json.dump({
                "overall": overall,
                "top_k": args.top_k,
                "total_correct": total_correct,
                "total_queries": total_q,
                "by_type": {
                    k: {**v, "recall_pct": v["correct"] / v["total"] * 100 if v["total"] else 0}
                    for k, v in by_type.items()
                },
            }, f, indent=2)
        print(f"\nResults written to {args.output}")

    sys.exit(0 if overall >= 85.0 else 1)


if __name__ == "__main__":
    main()