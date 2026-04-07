#!/usr/bin/env python3
"""
LongMemEval benchmark adapter for mentisdb.

Measures retrieval recall (R@k): for each of 500 questions, does the gold
evidence turn appear in the top-k thoughts returned by mentisdb ranked-search?

Dataset:
    git clone https://github.com/xiaowu0162/LongMemEval
    # Follow their README to download data/longmemeval_oracle.json

Reference scores (from MemPalace BENCHMARKS.md, 2026):
    ChromaDB raw baseline   96.6% R@5
    Hybrid + Haiku rerank  100.0% R@5

Usage:
    # Start mentisdbd first, then:
    python benchmarks/longmemeval_bench.py \\
        --data path/to/longmemeval_oracle.json \\
        --top-k 5 \\
        --chain longmemeval-$(date +%s) \\
        --workers 16

    # Re-use an already-ingested chain (skip ingestion):
    python benchmarks/longmemeval_bench.py \\
        --data path/to/longmemeval_oracle.json \\
        --chain longmemeval-1234567890 \\
        --skip-ingest
"""

import argparse
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

DEFAULT_BASE_URL = "http://127.0.0.1:9472"
DEFAULT_CHAIN = "longmemeval-bench"
DEFAULT_TOP_K = 5
DEFAULT_WORKERS = 16


# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

def _post(base_url: str, path: str, payload: dict, timeout: int = 15) -> dict:
    r = requests.post(f"{base_url}{path}", json=payload, timeout=timeout)
    r.raise_for_status()
    return r.json()


def append_turn(base_url: str, chain_key: str, content: str, role: str,
                session_id: str) -> None:
    _post(base_url, "/v1/append", {
        "chain_key": chain_key,
        "thought_type": "Observation",
        "content": content,
        "agent_id": role,          # "user" or "assistant"
        "importance": 0.5,
        "tags": [f"session:{session_id}", f"role:{role}"],
    })


def ranked_search(base_url: str, chain_key: str, query: str, limit: int) -> list[dict]:
    return _post(base_url, "/v1/ranked-search", {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
    }).get("thoughts", [])


# ---------------------------------------------------------------------------
# Evidence matching
# ---------------------------------------------------------------------------

def _hit(evidence_texts: list[str], thoughts: list[dict]) -> bool:
    """True if any gold evidence text is subsumed by any top-k thought content."""
    contents = [t.get("content", "").strip().lower() for t in thoughts]
    for ev in evidence_texts:
        ev_l = ev.strip().lower()
        for c in contents:
            if ev_l in c or c in ev_l:
                return True
    return False


def _collect_evidence(instance: dict) -> list[str]:
    """Collect the gold evidence turn texts for one LongMemEval instance."""
    answer_sids = set(instance.get("answer_session_ids", []))
    texts = []
    for session, sid in zip(instance["haystack_sessions"], instance["haystack_session_ids"]):
        if sid not in answer_sids:
            continue
        for turn in session:
            if turn.get("has_answer"):
                texts.append(turn["content"])
    # Fallback: if no has_answer turns marked, use the gold answer string.
    return texts if texts else [instance.get("answer", "")]


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

def ingest(base_url: str, chain_key: str, instances: list[dict],
           workers: int) -> int:
    """Ingest all unique session turns. Returns total turns ingested."""
    seen: set[str] = set()
    tasks: list[tuple[str, str, str]] = []  # (content, role, session_id)

    for inst in instances:
        for session, sid in zip(inst["haystack_sessions"], inst["haystack_session_ids"]):
            if sid in seen:
                continue
            seen.add(sid)
            for turn in session:
                tasks.append((turn.get("content", ""), turn.get("role", "unknown"), sid))

    total = len(tasks)
    print(f"  Ingesting {total} turns from {len(seen)} sessions …", flush=True)

    def _ingest_one(args):
        content, role, sid = args
        append_turn(base_url, chain_key, content, role, sid)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futs = {pool.submit(_ingest_one, t): i for i, t in enumerate(tasks)}
        done = 0
        for f in as_completed(futs):
            f.result()
            done += 1
            if done % 200 == 0 or done == total:
                print(f"    {done}/{total} turns", flush=True)

    return total


# ---------------------------------------------------------------------------
# Evaluation
# ---------------------------------------------------------------------------

def evaluate(base_url: str, chain_key: str, instances: list[dict],
             top_k: int) -> tuple[float, dict]:
    correct = 0
    by_type: dict[str, dict] = {}

    for i, inst in enumerate(instances):
        qtype = inst.get("question_type", "unknown")
        evidence = _collect_evidence(inst)
        thoughts = ranked_search(base_url, chain_key, inst["question"], top_k)
        hit = _hit(evidence, thoughts)

        if hit:
            correct += 1
        stats = by_type.setdefault(qtype, {"correct": 0, "total": 0})
        stats["total"] += 1
        if hit:
            stats["correct"] += 1

        if (i + 1) % 50 == 0:
            pct = correct / (i + 1) * 100
            print(f"  Q {i+1:>3}/{len(instances)} — R@{top_k} so far: {pct:.1f}%", flush=True)

    overall = correct / len(instances) * 100 if instances else 0.0
    return overall, by_type


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="LongMemEval benchmark for mentisdb")
    ap.add_argument("--data", required=True, help="Path to longmemeval_oracle.json")
    ap.add_argument("--top-k", type=int, default=DEFAULT_TOP_K)
    ap.add_argument("--limit", type=int, default=None,
                    help="Evaluate only first N instances (dev mode)")
    ap.add_argument("--chain", default=DEFAULT_CHAIN)
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS,
                    help="Parallel ingestion workers")
    ap.add_argument("--skip-ingest", action="store_true",
                    help="Skip ingestion (chain already populated)")
    ap.add_argument("--output", help="Write per-instance JSONL results to this path")
    args = ap.parse_args()

    with open(args.data) as f:
        instances = json.load(f)
    if args.limit:
        instances = instances[: args.limit]

    print(f"\nLongMemEval × mentisdb")
    print(f"  instances : {len(instances)}")
    print(f"  top-k     : {args.top_k}")
    print(f"  chain     : {args.chain}")
    print(f"  endpoint  : {args.base_url}\n")

    if not args.skip_ingest:
        t0 = time.monotonic()
        ingest(args.base_url, args.chain, instances, args.workers)
        print(f"  Ingestion done in {time.monotonic()-t0:.1f}s\n", flush=True)
        time.sleep(1)  # brief settle

    t0 = time.monotonic()
    overall, by_type = evaluate(args.base_url, args.chain, instances, args.top_k)
    elapsed = time.monotonic() - t0

    print(f"\n{'='*55}")
    print(f"LongMemEval  R@{args.top_k}: {overall:.1f}%"
          f"  ({sum(v['correct'] for v in by_type.values())}/{len(instances)})")
    print(f"Evaluation time: {elapsed:.1f}s\n")
    print("By question type:")
    for qtype, stats in sorted(by_type.items()):
        pct = stats["correct"] / stats["total"] * 100 if stats["total"] else 0
        bar = "█" * int(pct / 5)
        print(f"  {qtype:<38} {pct:5.1f}%  {bar}")

    if args.output:
        # Re-run to capture per-instance detail
        with open(args.output, "w") as out:
            for inst in instances:
                evidence = _collect_evidence(inst)
                thoughts = ranked_search(args.base_url, args.chain,
                                         inst["question"], args.top_k)
                hit = _hit(evidence, thoughts)
                out.write(json.dumps({
                    "question_id": inst.get("question_id"),
                    "question_type": inst.get("question_type"),
                    "hit": hit,
                    "top_k_contents": [t.get("content", "")[:200] for t in thoughts],
                }) + "\n")
        print(f"Per-instance results written to {args.output}")

    sys.exit(0 if overall >= 90.0 else 1)


if __name__ == "__main__":
    main()
