#!/usr/bin/env python3
"""
ConvoMem benchmark adapter for mentisdb.

Tests retrieval recall across 75,336 QA pairs in 6 evidence categories:
  user          — facts stated by the user
  asst          — facts stated by the assistant
  pref          — user preferences
  change        — information that updates over time
  implicit      — multi-hop / inferential connections
  abstain       — unanswerable questions (system should not hallucinate)

Dataset (HuggingFace): Salesforce/ConvoMem
Scoring: R@k — does the gold evidence message appear in the top-k results?
         For abstention: correct if NO result's content resembles any evidence.

Reference scores (MemPalace BENCHMARKS.md, 2026):
    Overall  92.9%     Mem0  ~43%
    asst    100.0%
    user     98.0%
    pref     86.0%

Usage:
    pip install datasets requests
    python lme-benches/convomem_bench.py \\
        --limit 500 \\
        --top-k 5 \\
        --chain convomem-$(date +%s)

    # Full run (75k pairs, takes ~30 min):
    python lme-benches/convomem_bench.py --top-k 5
"""

import argparse
import json
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

DEFAULT_BASE_URL = "http://127.0.0.1:9472"
DEFAULT_TOP_K = 5
DEFAULT_WORKERS = 16

CATEGORIES = {
    "user":     "user_evidence",
    "asst":     "assistant_facts_evidence",
    "pref":     "preference_evidence",
    "change":   "changing_evidence",
    "implicit": "implicit_connection_evidence",
    "abstain":  "abstention_evidence",
}


# ---------------------------------------------------------------------------
# Dataset loading
# ---------------------------------------------------------------------------

def load_convomem(categories: list[str], limit_per_cat: int | None) -> dict[str, list]:
    """Download and return {alias: [item, ...]} from HuggingFace Salesforce/ConvoMem."""
    try:
        from datasets import load_dataset
    except ImportError:
        print("ERROR: pip install datasets", file=sys.stderr)
        sys.exit(1)

    result = {}
    for alias in categories:
        hf_name = CATEGORIES[alias]
        print(f"  Loading {alias} ({hf_name}) …", flush=True)
        ds = load_dataset("Salesforce/ConvoMem", hf_name, split="test",
                          trust_remote_code=True)
        items = list(ds)
        if limit_per_cat:
            items = items[:limit_per_cat]
        result[alias] = items
    return result


# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

def _post(base_url: str, path: str, payload: dict, timeout: int = 15) -> dict:
    r = requests.post(f"{base_url}{path}", json=payload, timeout=timeout)
    r.raise_for_status()
    return r.json()


def append_turn(base_url: str, chain_key: str, content: str, speaker: str) -> None:
    _post(base_url, "/v1/thoughts", {
        "chain_key": chain_key,
        "thought_type": "FactLearned",
        "content": content,
        "agent_id": speaker.lower(),
        "importance": 0.5,
        "tags": [f"speaker:{speaker.lower()}"],
    })


def ranked_search(base_url: str, chain_key: str, query: str, limit: int) -> list[dict]:
    resp = _post(base_url, "/v1/ranked-search", {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
    })
    return [r["thought"] for r in resp.get("results", [])]


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


def _abstain_hit(evidence_texts: list[str], thoughts: list[dict]) -> bool:
    """For abstention: correct when the evidence is NOT surfaced (model should say 'I don't know')."""
    return not _hit(evidence_texts, thoughts)


# ---------------------------------------------------------------------------
# Evaluation for one category
# ---------------------------------------------------------------------------

def evaluate_category(alias: str, items: list, base_url: str,
                      top_k: int, workers: int) -> dict:
    # Use a fresh chain per category to avoid cross-contamination.
    chain_key = f"convomem-{alias}-{int(time.time())}"
    correct = 0
    total = len(items)

    for i, item in enumerate(items):
        # Ingest the conversation for this item
        conversations = item.get("conversations", [])
        ingest_tasks = []
        for conv in conversations:
            for msg in conv.get("messages", []):
                ingest_tasks.append((msg.get("text", ""), msg.get("speaker", "unknown")))

        with ThreadPoolExecutor(max_workers=workers) as pool:
            futs = [pool.submit(append_turn, base_url, chain_key, text, speaker)
                    for text, speaker in ingest_tasks]
            for f in as_completed(futs):
                f.result()

        # Retrieve and score
        question = item.get("question", "")
        evidence_texts = [e.get("text", "") for e in item.get("message_evidences", [])]
        thoughts = ranked_search(base_url, chain_key, question, top_k)

        if alias == "abstain":
            hit = _abstain_hit(evidence_texts, thoughts)
        else:
            hit = _hit(evidence_texts, thoughts)

        if hit:
            correct += 1

        if (i + 1) % 100 == 0 or (i + 1) == total:
            pct = correct / (i + 1) * 100
            print(f"    [{alias}] {i+1}/{total} — R@{top_k}: {pct:.1f}%", flush=True)

        # Clear chain after each item to avoid retrieval bleed between items
        # (ConvoMem evaluates each item independently over its own conversation)
        # We use a new chain key per item to simulate isolation.
        chain_key = f"convomem-{alias}-{int(time.time())}-{i+1}"

    return {"correct": correct, "total": total}


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="ConvoMem benchmark for mentisdb")
    ap.add_argument("--top-k", type=int, default=DEFAULT_TOP_K)
    ap.add_argument("--limit", type=int, default=None,
                    help="Items per category (dev mode; full run = 75k+)")
    ap.add_argument("--categories", nargs="+",
                    choices=list(CATEGORIES.keys()) + ["all"],
                    default=["all"])
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS)
    ap.add_argument("--output", help="Write per-category JSON results here")
    args = ap.parse_args()

    cats = list(CATEGORIES.keys()) if "all" in args.categories else args.categories

    print(f"\nConvoMem × mentisdb")
    print(f"  categories : {', '.join(cats)}")
    print(f"  top-k      : {args.top_k}")
    print(f"  limit/cat  : {args.limit or 'full'}")
    print(f"  endpoint   : {args.base_url}\n")

    data = load_convomem(cats, args.limit)

    all_results: dict[str, dict] = {}
    for alias in cats:
        items = data[alias]
        print(f"\n--- {alias} ({len(items)} items) ---", flush=True)
        t0 = time.monotonic()
        result = evaluate_category(alias, items, args.base_url, args.top_k, args.workers)
        elapsed = time.monotonic() - t0
        pct = result["correct"] / result["total"] * 100 if result["total"] else 0
        print(f"  {alias}  R@{args.top_k}: {pct:.1f}%  ({result['correct']}/{result['total']})"
              f"  [{elapsed:.0f}s]")
        all_results[alias] = result

    # Summary
    total_correct = sum(v["correct"] for v in all_results.values())
    total_items = sum(v["total"] for v in all_results.values())
    overall = total_correct / total_items * 100 if total_items else 0

    print(f"\n{'='*55}")
    print(f"ConvoMem  R@{args.top_k}: {overall:.1f}%  ({total_correct}/{total_items})\n")
    print("By category:")
    for alias, stats in all_results.items():
        pct = stats["correct"] / stats["total"] * 100 if stats["total"] else 0
        bar = "█" * int(pct / 5)
        print(f"  {alias:<12} {pct:5.1f}%  {bar}")

    if args.output:
        with open(args.output, "w") as f:
            json.dump({
                "overall": overall,
                "top_k": args.top_k,
                "by_category": {
                    k: {**v, "recall_pct": v["correct"] / v["total"] * 100 if v["total"] else 0}
                    for k, v in all_results.items()
                },
            }, f, indent=2)
        print(f"\nResults written to {args.output}")

    sys.exit(0 if overall >= 90.0 else 1)


if __name__ == "__main__":
    main()
