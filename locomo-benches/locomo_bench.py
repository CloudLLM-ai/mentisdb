#!/usr/bin/env python3
"""
LoCoMo benchmark adapter for mentisdb.

Tests retrieval recall (R@k) across ~1,986 QA pairs from long social
conversations (up to ~300 turns) spanning weeks of interaction.

Question types:
  single   — single-hop fact retrieval
  multi    — multi-hop reasoning across 2+ turns
  adv      — adversarial (unanswerable; should not hallucinate)
  summary  — conversation summarization (skipped in retrieval eval)

Dataset (HuggingFace): snap-research/locomo
Reference scores (MemPalace BENCHMARKS.md, 2026):
    Hybrid v5, top-10, no rerank   88.9% R@10
    Hybrid + Sonnet rerank, top-50  100.0% R@5 (caveat: top-50 > session count)

Usage:
    pip install datasets requests
    bash locomo-benches/run_locomo.sh

    # Or manually:
    python locomo-benches/locomo_bench.py \\
        --top-k 10 \\
        --chain locomo-$(date +%s)

    # Dev run:
    python locomo-benches/locomo_bench.py --top-k 10 --limit 20

    # Force re-ingest:
    python locomo-benches/locomo_bench.py --top-k 10 --force-reingest
"""

import argparse
import json
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
DEFAULT_EVAL_W    = 8
NEAR_MISS_K       = 50


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
    importance = 0.8 if speaker.lower() == "user" else 0.2
    payload = {
        "chain_key": chain_key,
        "thought_type": "FactLearned",
        "content": content,
        "agent_id": speaker.lower(),
        "importance": importance,
        "tags": [f"speaker:{speaker.lower()}", f"turn:{turn_index}"],
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
            time.sleep(0.5 * (attempt + 1))


def rebuild_vectors(base_url: str, chain_key: str,
                    provider_key: str = "fastembed-minilm") -> None:
    try:
        resp = _post(base_url, "/v1/vectors/rebuild", {
            "chain_key": chain_key,
            "provider_key": provider_key,
        }, timeout=600)
        indexed = resp.get("status", {}).get("indexed_thought_count")
        print(f"  [{provider_key}] Vector sidecar rebuilt — {indexed} thoughts indexed.", flush=True)
    except Exception as e:
        print(f"  WARNING: vector rebuild failed ({provider_key}): {e}", flush=True)


def ranked_search(base_url: str, chain_key: str, query: str, limit: int) -> list[dict]:
    resp = _post(base_url, "/v1/ranked-search", {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
        "graph": {
            "max_depth": 3,
            "max_visited": 200,
            "include_seeds": False,
        },
    })
    return resp.get("results", [])


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

def ingest_conversation(base_url: str, chain_key: str,
                        conversation: list[dict], workers: int) -> int:
    turns = [(t.get("text", ""), t.get("speaker", "unknown")) for t in conversation if t.get("text", "").strip()]
    total = len(turns)
    if total == 0:
        return 0

    done_count = [0]
    lock = threading.Lock()

    def _ingest_sequential(turns_in_session):
        prev_id = None
        for text, speaker, idx in turns_in_session:
            prev_id = append_turn(base_url, chain_key, text, speaker, idx, prev_id=prev_id)
            with lock:
                done_count[0] += 1
                d = done_count[0]
                if d % 100 == 0 or d == total:
                    print(f"    {d}/{total} turns ingested", flush=True)
        return prev_id

    if workers <= 1:
        items = [(text, speaker, idx) for idx, (text, speaker) in enumerate(turns)]
        _ingest_sequential(items)
    else:
        chunk_size = max(1, len(turns) // workers)
        chunks = []
        for i in range(0, len(turns), chunk_size):
            chunk = [(text, speaker, idx) for idx, (text, speaker) in enumerate(turns[i:i+chunk_size])]
            chunks.append(chunk)

        with ThreadPoolExecutor(max_workers=1) as pool:
            futs = [pool.submit(_ingest_sequential, chunk) for chunk in chunks]
            for f in as_completed(futs):
                f.result()

    return total


# ---------------------------------------------------------------------------
# Evidence matching
# ---------------------------------------------------------------------------

def _hit(evidence_texts: list[str], results: list[dict], k: int) -> bool:
    contents = [r["thought"].get("content", "").strip().lower() for r in results[:k]]
    for ev in evidence_texts:
        ev_l = ev.strip().lower()
        for c in contents:
            if ev_l in c or c in ev_l:
                return True
    return False


def _collect_evidence(qa: dict, conversation: list[dict]) -> list[str]:
    evidence = qa.get("evidence", [])
    if not evidence:
        answer = qa.get("answer", "")
        return [answer] if answer else []

    texts = []
    for ev in evidence:
        if isinstance(ev, int):
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

def evaluate(base_url: str, chain_prefix: str, items: list,
             top_k: int, workers: int) -> tuple[float, dict, list[dict], list]:
    by_type: dict[str, dict] = {}
    misses: list[dict] = []
    fetch_k = max(top_k, NEAR_MISS_K)
    all_results: list = [None] * len(items)
    lock = threading.Lock()
    done_count = [0]
    correct_count = [0]

    for item_idx, item in enumerate(items):
        conversation = item.get("conversation", [])
        qa_pairs = item.get("qa", [])
        item_chain = f"{chain_prefix}-item{item_idx}"

        print(f"  Ingesting item {item_idx+1}/{len(items)} "
              f"({len(conversation)} turns, {len(qa_pairs)} QAs) …", flush=True)
        ingest_conversation(base_url, item_chain, conversation, workers)

        for qa_idx, qa in enumerate(qa_pairs):
            qtype_raw = qa.get("type", "single")
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
                continue

            question = qa.get("question", "")
            evidence = _collect_evidence(qa, conversation)
            raw = ranked_search(base_url, item_chain, question, fetch_k)

            hit_k  = _hit(evidence, raw, top_k)
            hit_10 = _hit(evidence, raw, 10)
            hit_20 = _hit(evidence, raw, 20)
            hit_50 = _hit(evidence, raw, 50)

            if qtype == "adv":
                hit_k  = not hit_k
                hit_10 = not hit_10
                hit_20 = not hit_20
                hit_50 = not hit_50

            s = by_type.setdefault(qtype, {
                "correct": 0, "total": 0, "hit_10": 0, "hit_20": 0, "hit_50": 0,
            })
            s["total"] += 1
            if hit_k:
                s["correct"] += 1
            if hit_10:
                s["hit_10"] += 1
            if hit_20:
                s["hit_20"] += 1
            if hit_50:
                s["hit_50"] += 1

            if not hit_k and qtype != "adv":
                top_scores = [r.get("score", {}) for r in raw[:top_k]]
                misses.append({
                    "item_idx":    item_idx,
                    "qa_idx":      qa_idx,
                    "question":    question[:200],
                    "qtype":        qtype,
                    "evidence":    [e[:200] for e in evidence[:3]],
                    "retrieved":   [r["thought"].get("content", "")[:200] for r in raw[:top_k]],
                    "scores":      top_scores,
                    "near_10":     hit_10,
                    "near_20":     hit_20,
                    "near_50":     hit_50,
                })

            all_results[item_idx * 1000 + qa_idx] = (
                qa, evidence, raw, hit_k, hit_10, hit_20, hit_50
            )

            with lock:
                done_count[0] += 1
                if hit_k:
                    correct_count[0] += 1
                d = done_count[0]
                if d % 50 == 0:
                    pct = correct_count[0] / d * 100
                    print(f"  {d} QAs evaluated — R@{top_k}: {pct:.1f}%", flush=True)

    total_correct = sum(v["correct"] for v in by_type.values())
    total_q = sum(v["total"] for v in by_type.values())
    overall = total_correct / total_q * 100 if total_q else 0
    return overall, by_type, misses, all_results


# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

def print_diagnostics(by_type: dict, misses: list[dict], top_k: int) -> None:
    print(f"\nNear-miss analysis (evidence found at wider k):")
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
        print(f"\nSample misses from '{worst_type}' (3 of {len(worst_misses)}):")
        for m in worst_misses[:3]:
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
                    help="Evaluate only first N persona-pairs (dev mode)")
    ap.add_argument("--chain", default=f"locomo-{int(time.time())}")
    ap.add_argument("--base-url", default=DEFAULT_BASE_URL)
    ap.add_argument("--workers", type=int, default=DEFAULT_WORKERS,
                    help="Ingestion workers (default 4)")
    ap.add_argument("--eval-workers", type=int, default=DEFAULT_EVAL_W,
                    help="Not used (LoCoMo evaluates sequentially per item)")
    ap.add_argument("--skip-vectors", action="store_true",
                    help="Skip vector sidecar rebuild after ingestion")
    ap.add_argument("--force-reingest", action="store_true",
                    help="Force re-ingest even if chains exist")
    ap.add_argument("--output", help="Write per-type JSON results here")
    args = ap.parse_args()

    chain_prefix = args.chain

    print(f"\nLoCoMo × mentisdb")
    print(f"  top-k        : {args.top_k}  (also computing R@10, R@20, R@{NEAR_MISS_K})")
    print(f"  limit        : {args.limit or 'full'}")
    print(f"  chain prefix : {chain_prefix}")
    print(f"  endpoint     : {args.base_url}\n")

    items = load_locomo(args.limit)
    print(f"  Loaded {len(items)} persona-pairs\n")

    t0 = time.monotonic()
    overall, by_type, misses, all_results = evaluate(
        args.base_url, chain_prefix, items, args.top_k, args.workers
    )
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

    print_diagnostics(by_type, misses, args.top_k)

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
        print(f"\nResults written to {args.output}")

    sys.exit(0 if overall >= 85.0 else 1)


if __name__ == "__main__":
    main()