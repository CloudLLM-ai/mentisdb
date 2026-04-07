#!/usr/bin/env python3
"""
LoCoMo benchmark adapter for mentisdb.

Tests retrieval recall (R@k) across 1,986 QA pairs from long social
conversations (up to ~300 turns) spanning weeks of interaction.

Question types:
  single   — single-hop fact retrieval
  multi    — multi-hop reasoning across 2+ turns
  adv      — adversarial (unanswerable; should not hallucinate)
  summary  — conversation summarization (skipped in retrieval eval)

Dataset (HuggingFace): snap-research/locomo
Reference scores (MemPalace BENCHMARKS.md, 2026):
    Hybrid v5, top-10, no rerank   88.9% R@10
    Hybrid + Sonnet rerank, top-50 100.0% R@5 (caveat: top-50 > session count)

Usage:
    pip install datasets requests
    python benchmarks/locomo_bench.py \\
        --top-k 10 \\
        --chain locomo-$(date +%s) \\
        --limit 200

    # Full run:
    python benchmarks/locomo_bench.py --top-k 10
"""

import argparse
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

DEFAULT_BASE_URL = "http://127.0.0.1:9472"
DEFAULT_TOP_K = 10
DEFAULT_WORKERS = 16


# ---------------------------------------------------------------------------
# Dataset loading
# ---------------------------------------------------------------------------

def load_locomo(limit: int | None) -> list:
    try:
        from datasets import load_dataset
    except ImportError:
        print("ERROR: pip install datasets", file=sys.stderr)
        sys.exit(1)

    print("  Loading snap-research/locomo …", flush=True)
    ds = load_dataset("snap-research/locomo", split="test", trust_remote_code=True)
    items = list(ds)
    if limit:
        items = items[:limit]
    return items


# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

def _post(base_url: str, path: str, payload: dict, timeout: int = 15) -> dict:
    r = requests.post(f"{base_url}{path}", json=payload, timeout=timeout)
    r.raise_for_status()
    return r.json()


def append_turn(base_url: str, chain_key: str, content: str, speaker: str,
                turn_index: int) -> None:
    _post(base_url, "/v1/append", {
        "chain_key": chain_key,
        "thought_type": "Observation",
        "content": content,
        "agent_id": speaker.lower(),
        "importance": 0.5,
        "tags": [f"speaker:{speaker.lower()}", f"turn:{turn_index}"],
    })


def ranked_search(base_url: str, chain_key: str, query: str, limit: int) -> list[dict]:
    return _post(base_url, "/v1/ranked-search", {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
    }).get("thoughts", [])


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

def ingest_conversation(base_url: str, chain_key: str,
                        conversation: list[dict], workers: int) -> int:
    tasks = []
    for i, turn in enumerate(conversation):
        speaker = turn.get("speaker", "unknown")
        text = turn.get("text", "")
        if text.strip():
            tasks.append((text, speaker, i))

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futs = [pool.submit(append_turn, base_url, chain_key, text, speaker, idx)
                for text, speaker, idx in tasks]
        for f in as_completed(futs):
            f.result()
    return len(tasks)


# ---------------------------------------------------------------------------
# Evidence matching
# ---------------------------------------------------------------------------

def _hit(evidence_texts: list[str], thoughts: list[dict]) -> bool:
    contents = [t.get("content", "").strip().lower() for t in thoughts]
    for ev in evidence_texts:
        ev_l = ev.strip().lower()
        for c in contents:
            if ev_l in c or c in ev_l:
                return True
    return False


def _collect_evidence(qa: dict, conversation: list[dict]) -> list[str]:
    """Return the gold evidence turn texts for a LoCoMo QA pair."""
    # LoCoMo QAs have 'evidence' field: list of turn indices or texts
    evidence = qa.get("evidence", [])
    if not evidence:
        return [qa.get("answer", "")]

    texts = []
    for ev in evidence:
        if isinstance(ev, int):
            # Index into conversation turns
            if 0 <= ev < len(conversation):
                texts.append(conversation[ev].get("text", ""))
        elif isinstance(ev, str):
            texts.append(ev)
        elif isinstance(ev, dict):
            texts.append(ev.get("text", ev.get("content", "")))
    return texts if texts else [qa.get("answer", "")]


# ---------------------------------------------------------------------------
# Evaluation
# ---------------------------------------------------------------------------

def evaluate(base_url: str, chain_key: str, items: list,
             top_k: int, workers: int) -> tuple[float, dict]:
    by_type: dict[str, dict] = {}
    total_turns = 0

    # LoCoMo: each item is one persona pair with its full conversation + QAs.
    # We ingest the full conversation into a fresh chain per item.
    for item_idx, item in enumerate(items):
        conversation = item.get("conversation", [])
        qa_pairs = item.get("qa", [])

        item_chain = f"{chain_key}-item{item_idx}"
        n = ingest_conversation(base_url, item_chain, conversation, workers)
        total_turns += n

        for qa in qa_pairs:
            qtype_raw = qa.get("type", "single")
            # Normalize: "single_hop" → "single", "multi_hop" → "multi", etc.
            if "single" in qtype_raw:
                qtype = "single"
            elif "multi" in qtype_raw:
                qtype = "multi"
            elif "advers" in qtype_raw or "adv" in qtype_raw:
                qtype = "adv"
            elif "summar" in qtype_raw:
                qtype = "summary"
            else:
                qtype = qtype_raw

            if qtype == "summary":
                continue  # Skip summarization tasks (generation, not retrieval)

            question = qa.get("question", "")
            evidence = _collect_evidence(qa, conversation)
            thoughts = ranked_search(base_url, item_chain, question, top_k)

            if qtype == "adv":
                hit = not _hit(evidence, thoughts)  # correct = NOT hallucinating evidence
            else:
                hit = _hit(evidence, thoughts)

            stats = by_type.setdefault(qtype, {"correct": 0, "total": 0})
            stats["total"] += 1
            if hit:
                stats["correct"] += 1

        if (item_idx + 1) % 10 == 0 or (item_idx + 1) == len(items):
            total_c = sum(v["correct"] for v in by_type.values())
            total_t = sum(v["total"] for v in by_type.values())
            pct = total_c / total_t * 100 if total_t else 0
            print(f"  Item {item_idx+1}/{len(items)}"
                  f"  {total_turns} turns ingested"
                  f"  R@{top_k}: {pct:.1f}%", flush=True)

    total_correct = sum(v["correct"] for v in by_type.values())
    total_q = sum(v["total"] for v in by_type.values())
    overall = total_correct / total_q * 100 if total_q else 0
    return overall, by_type


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="LoCoMo benchmark for mentisdb")
    ap.add_argument("--top-k", type=int, default=DEFAULT_TOP_K)
    ap.add_argument("--limit", type=int, default=None,
                    help="Evaluate only first N persona-pairs (dev mode)")
    ap.add_argument("--chain", default=f"locomo-{int(time.time())}")
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    ap.add_argument("--output", help="Write per-type JSON results here")
    args = ap.parse_args()

    print(f"\nLoCoMo × mentisdb")
    print(f"  top-k    : {args.top_k}")
    print(f"  limit    : {args.limit or 'full'}")
    print(f"  chain    : {args.chain}")
    print(f"  endpoint : {args.base_url}\n")

    items = load_locomo(args.limit)
    print(f"  Loaded {len(items)} persona-pairs\n")

    t0 = time.monotonic()
    overall, by_type = evaluate(args.base_url, args.chain, items, args.top_k, args.workers)
    elapsed = time.monotonic() - t0

    total_correct = sum(v["correct"] for v in by_type.values())
    total_q = sum(v["total"] for v in by_type.values())

    print(f"\n{'='*55}")
    print(f"LoCoMo  R@{args.top_k}: {overall:.1f}%  ({total_correct}/{total_q})")
    print(f"Evaluation time: {elapsed:.0f}s\n")
    print("By question type:")
    for qtype, stats in sorted(by_type.items()):
        pct = stats["correct"] / stats["total"] * 100 if stats["total"] else 0
        bar = "█" * int(pct / 5)
        print(f"  {qtype:<12} {pct:5.1f}%  ({stats['correct']}/{stats['total']})  {bar}")

    if args.output:
        with open(args.output, "w") as f:
            json.dump({
                "overall": overall,
                "top_k": args.top_k,
                "by_type": {
                    k: {**v, "recall_pct": v["correct"] / v["total"] * 100 if v["total"] else 0}
                    for k, v in by_type.items()
                },
            }, f, indent=2)
        print(f"Results written to {args.output}")

    sys.exit(0 if overall >= 85.0 else 1)


if __name__ == "__main__":
    main()
