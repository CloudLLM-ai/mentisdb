#!/usr/bin/env python3
"""
LongMemEval benchmark adapter for mentisdb.

Measures retrieval recall (R@k): for each of 500 questions, does the gold
evidence turn appear in the top-k thoughts returned by mentisdb ranked-search?

Reference scores (MemPalace BENCHMARKS.md, 2026):
    ChromaDB raw baseline   96.6% R@5
    Hybrid + Haiku rerank  100.0% R@5

Usage:
    # Start mentisdbd, then run the shell wrapper (handles everything):
    bash lme-benches/run_longmemeval.sh

    # Or manually — chain existence auto-detected, ingestion skipped if present:
    python lme-benches/longmemeval_bench.py \\
        --data data/longmemeval_oracle.json \\
        --chain lme-1234567890

    # Force re-ingest an existing chain:
    python lme-benches/longmemeval_bench.py \\
        --data data/longmemeval_oracle.json \\
        --chain lme-1234567890 \\
        --force-reingest
"""

import argparse
import json
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import requests

DEFAULT_BASE_URL  = "http://127.0.0.1:9472"
DEFAULT_CHAIN     = "longmemeval-bench"
DEFAULT_TOP_K     = 5
DEFAULT_WORKERS   = 4   # ingestion workers (write-heavy; conservative)
DEFAULT_EVAL_W    = 8   # evaluation workers (read-only; safe to push higher)
NEAR_MISS_K       = 20  # also compute R@10 and R@20 for near-miss analysis


# ---------------------------------------------------------------------------
# REST helpers
# ---------------------------------------------------------------------------

def _post(base_url: str, path: str, payload: dict, timeout: int = 15) -> dict:
    r = requests.post(f"{base_url}{path}", json=payload, timeout=timeout)
    if not r.ok:
        raise requests.HTTPError(
            f"{r.status_code} {r.reason} — body: {r.text[:300]}",
            response=r,
        )
    return r.json()


def chain_exists(base_url: str, chain_key: str) -> bool:
    """Return True if chain_key is already known to the daemon."""
    try:
        r = requests.get(f"{base_url}/v1/chains", timeout=5)
        r.raise_for_status()
        return chain_key in r.json().get("chain_keys", [])
    except Exception:
        return False


def chain_thought_count(base_url: str, chain_key: str) -> int:
    """Return the number of thoughts in chain_key, or 0 on error."""
    try:
        r = requests.get(f"{base_url}/v1/chains", timeout=5)
        r.raise_for_status()
        for c in r.json().get("chains", []):
            if c.get("chain_key") == chain_key:
                return c.get("thought_count", 0)
    except Exception:
        pass
    return 0


def append_turn(base_url: str, chain_key: str, content: str, role: str,
                session_id: str, retries: int = 3) -> None:
    # User turns get higher importance: preferences, facts, and personal statements
    # are always from the user role. Assistant turns tend to be verbose and dominate
    # BM25 scoring; downweighting them improves retrieval of user-originated evidence.
    importance = 0.8 if role == "user" else 0.2
    for attempt in range(retries):
        try:
            _post(base_url, "/v1/thoughts", {
                "chain_key": chain_key,
                "thought_type": "FactLearned",
                "content": content,
                "agent_id": role,
                "importance": importance,
                "tags": [f"session:{session_id}", f"role:{role}"],
            })
            return
        except Exception:
            if attempt == retries - 1:
                raise
            time.sleep(0.5 * (attempt + 1))


def rebuild_vectors(base_url: str, chain_key: str) -> None:
    """Trigger vector sidecar rebuild (hash-based, not semantic — opt-in only)."""
    try:
        resp = _post(base_url, "/v1/vectors/rebuild", {
            "chain_key": chain_key,
            "provider_key": "local-text-v1",
        }, timeout=300)
        indexed = resp.get("status", {}).get("indexed_thought_count")
        print(f"  Vector sidecar rebuilt — {indexed} thoughts indexed.", flush=True)
    except Exception as e:
        print(f"  WARNING: vector rebuild failed: {e}", flush=True)


def ranked_search(base_url: str, chain_key: str, query: str, limit: int) -> list[dict]:
    resp = _post(base_url, "/v1/ranked-search", {
        "chain_key": chain_key,
        "text": query,
        "limit": limit,
    })
    # Each element: {"thought": {...}, "score": {lexical, vector, graph, ...}}
    return resp.get("results", [])


# ---------------------------------------------------------------------------
# Evidence matching
# ---------------------------------------------------------------------------

def _hit(evidence_texts: list[str], results: list[dict], k: int) -> bool:
    """True if any gold evidence text is subsumed by any of the first k results."""
    contents = [r["thought"].get("content", "").strip().lower() for r in results[:k]]
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
    return texts if texts else [instance.get("answer", "")]


# ---------------------------------------------------------------------------
# Ingestion
# ---------------------------------------------------------------------------

def ingest(base_url: str, chain_key: str, instances: list[dict], workers: int) -> int:
    """Ingest all unique session turns. Returns total turns ingested."""
    seen: set[str] = set()
    tasks: list[tuple[str, str, str]] = []

    for inst in instances:
        for session, sid in zip(inst["haystack_sessions"], inst["haystack_session_ids"]):
            if sid in seen:
                continue
            seen.add(sid)
            for turn in session:
                tasks.append((turn.get("content", ""), turn.get("role", "unknown"), sid))

    total = len(tasks)
    print(f"  Ingesting {total} turns from {len(seen)} sessions …", flush=True)
    t0 = time.monotonic()

    def _ingest_one(args):
        content, role, sid = args
        append_turn(base_url, chain_key, content, role, sid)

    with ThreadPoolExecutor(max_workers=workers) as pool:
        futs = {pool.submit(_ingest_one, t): i for i, t in enumerate(tasks)}
        done = 0
        for f in as_completed(futs):
            f.result()
            done += 1
            if done % 500 == 0 or done == total:
                elapsed = time.monotonic() - t0
                rate = done / elapsed if elapsed > 0 else 0
                eta = (total - done) / rate if rate > 0 else 0
                print(f"    {done}/{total} turns  ({rate:.0f}/s  ETA {eta:.0f}s)", flush=True)

    return total


# ---------------------------------------------------------------------------
# Parallel evaluation
# ---------------------------------------------------------------------------

def evaluate(
    base_url: str,
    chain_key: str,
    instances: list[dict],
    top_k: int,
    eval_workers: int,
) -> tuple[float, dict, list[dict], list]:
    """
    Evaluate retrieval recall in parallel.

    Returns:
        overall    — R@top_k as a percentage
        by_type    — per-question-type stats dict
        misses     — list of miss detail dicts (for diagnostics)
        all_results — ordered list of per-instance result tuples
    """
    fetch_k = max(top_k, NEAR_MISS_K)
    ordered = [None] * len(instances)
    lock = threading.Lock()
    done_count = [0]
    correct_count = [0]

    def _eval_one(idx: int, inst: dict):
        evidence = _collect_evidence(inst)
        raw = ranked_search(base_url, chain_key, inst["question"], fetch_k)
        hit_k  = _hit(evidence, raw, top_k)
        hit_10 = _hit(evidence, raw, 10)
        hit_20 = _hit(evidence, raw, NEAR_MISS_K)
        return idx, inst, evidence, raw, hit_k, hit_10, hit_20

    with ThreadPoolExecutor(max_workers=eval_workers) as pool:
        futs = [pool.submit(_eval_one, i, inst) for i, inst in enumerate(instances)]
        for f in as_completed(futs):
            idx, inst, evidence, raw, hit_k, hit_10, hit_20 = f.result()
            ordered[idx] = (inst, evidence, raw, hit_k, hit_10, hit_20)
            with lock:
                done_count[0] += 1
                if hit_k:
                    correct_count[0] += 1
                d = done_count[0]
                if d % 50 == 0 or d == len(instances):
                    pct = correct_count[0] / d * 100
                    print(f"  {d:>3}/{len(instances)} — R@{top_k} so far: {pct:.1f}%",
                          flush=True)

    # Aggregate
    by_type: dict[str, dict] = {}
    misses: list[dict] = []

    for inst, evidence, raw, hit_k, hit_10, hit_20 in ordered:
        qtype = inst.get("question_type", "unknown")
        s = by_type.setdefault(qtype, {
            "correct": 0, "total": 0, "hit_10": 0, "hit_20": 0,
        })
        s["total"] += 1
        if hit_k:
            s["correct"] += 1
        if hit_10:
            s["hit_10"] += 1
        if hit_20:
            s["hit_20"] += 1
        if not hit_k:
            top_scores = [r.get("score", {}) for r in raw[:top_k]]
            misses.append({
                "question":   inst.get("question", ""),
                "qtype":      qtype,
                "evidence":   evidence,
                "retrieved":  [r["thought"].get("content", "")[:200] for r in raw[:top_k]],
                "scores":     top_scores,
                "near_10":    hit_10,
                "near_20":    hit_20,
            })

    overall = correct_count[0] / len(instances) * 100 if instances else 0.0
    return overall, by_type, misses, ordered


# ---------------------------------------------------------------------------
# Diagnostics
# ---------------------------------------------------------------------------

def print_diagnostics(
    by_type: dict,
    misses: list[dict],
    top_k: int,
    miss_samples: int = 3,
) -> None:
    print("\nNear-miss analysis (evidence found at wider k):")
    print(f"  {'type':<40} R@{top_k:<3}   R@10   R@20")
    print(f"  {'-'*62}")
    for qtype, s in sorted(by_type.items()):
        t = s["total"]
        r_k  = s["correct"] / t * 100
        r_10 = s["hit_10"]  / t * 100
        r_20 = s["hit_20"]  / t * 100
        print(f"  {qtype:<40} {r_k:5.1f}%  {r_10:5.1f}%  {r_20:5.1f}%")

    # Score signal breakdown for misses vs hits
    miss_scores = [m["scores"][0] for m in misses if m["scores"]]
    if miss_scores:
        keys = ["lexical", "vector", "graph", "relation", "seed_support", "recency", "total"]
        print("\nAvg top-1 score breakdown on MISSES:")
        for k in keys:
            vals = [s.get(k, 0) for s in miss_scores if isinstance(s, dict)]
            if vals:
                print(f"  {k:<14} {sum(vals)/len(vals):.4f}")

    # Which misses are near-misses?
    near_10 = sum(1 for m in misses if m["near_10"])
    near_20 = sum(1 for m in misses if m["near_20"])
    total_miss = len(misses)
    if total_miss:
        print(f"\nOf {total_miss} misses:")
        print(f"  {near_10} ({near_10/total_miss*100:.1f}%) appear in top-10 (ranking problem, not retrieval)")
        print(f"  {near_20} ({near_20/total_miss*100:.1f}%) appear in top-20")
        print(f"  {total_miss-near_20} ({(total_miss-near_20)/total_miss*100:.1f}%) not in top-20 (lexical gap)")

    # Worst category sample misses
    miss_by_type: dict[str, list] = {}
    for m in misses:
        miss_by_type.setdefault(m["qtype"], []).append(m)

    worst = sorted(miss_by_type.items(), key=lambda x: len(x[1]), reverse=True)
    if worst:
        print(f"\nMiss counts by type:")
        for qtype, ms in worst:
            total = by_type[qtype]["total"]
            near = sum(1 for m in ms if m["near_10"])
            print(f"  {qtype:<40} {len(ms)}/{total} misses  ({near} near top-10)")

        worst_type, worst_misses = worst[0]
        print(f"\nSample misses from '{worst_type}' ({min(miss_samples, len(worst_misses))} of {len(worst_misses)}):")
        for m in worst_misses[:miss_samples]:
            ev_snip  = (m["evidence"][0][:120] + "…") if m["evidence"] else "(none)"
            ret_snip = (m["retrieved"][0][:120] + "…") if m["retrieved"] else "(nothing retrieved)"
            print(f"\n  Q:         {m['question'][:130]}")
            print(f"  Evidence:  {ev_snip}")
            print(f"  Top-1 ret: {ret_snip}")
            if m["scores"]:
                s = m["scores"][0]
                if isinstance(s, dict):
                    print(f"  Score:     lexical={s.get('lexical',0):.3f}  "
                          f"graph={s.get('graph',0):.3f}  "
                          f"recency={s.get('recency',0):.3f}  "
                          f"total={s.get('total',0):.3f}")

    # Evidence length stats per type (helps detect why short/implicit evidence is hard)
    ev_len_by_type: dict[str, list[int]] = {}
    for m in misses:
        for ev in m["evidence"]:
            ev_len_by_type.setdefault(m["qtype"], []).append(len(ev))
    if ev_len_by_type:
        print(f"\nMiss evidence length (chars) by type — short evidence = harder substring match:")
        for qtype, lengths in sorted(ev_len_by_type.items()):
            avg = sum(lengths) / len(lengths)
            print(f"  {qtype:<40} avg={avg:6.0f}  min={min(lengths)}  max={max(lengths)}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description="LongMemEval benchmark for mentisdb")
    ap.add_argument("--data", required=True, help="Path to longmemeval_oracle.json")
    ap.add_argument("--top-k",       type=int, default=DEFAULT_TOP_K)
    ap.add_argument("--limit",       type=int, default=None,
                    help="Evaluate only first N instances (dev mode)")
    ap.add_argument("--chain",       default=DEFAULT_CHAIN)
    ap.add_argument("--base-url",    default=DEFAULT_BASE_URL)
    ap.add_argument("--workers",     type=int, default=DEFAULT_WORKERS,
                    help="Parallel ingestion workers (write-heavy; default 4)")
    ap.add_argument("--eval-workers", type=int, default=DEFAULT_EVAL_W,
                    help="Parallel evaluation workers (read-only; default 8)")
    ap.add_argument("--skip-ingest", action="store_true",
                    help="Force-skip ingestion even if chain looks empty")
    ap.add_argument("--force-reingest", action="store_true",
                    help="Re-ingest even if chain already exists")
    ap.add_argument("--rebuild-vectors", action="store_true",
                    help="Build vector sidecar after ingestion (hash-based, not semantic)")
    ap.add_argument("--output", help="Write per-instance JSONL results to this path")
    args = ap.parse_args()

    with open(args.data) as f:
        instances = json.load(f)
    if args.limit:
        instances = instances[: args.limit]

    print(f"\nLongMemEval × mentisdb")
    print(f"  instances    : {len(instances)}")
    print(f"  top-k        : {args.top_k}  (also computing R@10, R@{NEAR_MISS_K})")
    print(f"  chain        : {args.chain}")
    print(f"  endpoint     : {args.base_url}")
    print(f"  eval-workers : {args.eval_workers}\n")

    # Auto-detect whether ingestion is needed
    do_ingest = not args.skip_ingest
    if do_ingest and not args.force_reingest:
        if chain_exists(args.base_url, args.chain):
            count = chain_thought_count(args.base_url, args.chain)
            print(f"  Chain '{args.chain}' already exists ({count} thoughts) — skipping ingestion.")
            print(f"  Use --force-reingest to re-ingest.\n")
            do_ingest = False

    if do_ingest:
        t0 = time.monotonic()
        ingest(args.base_url, args.chain, instances, args.workers)
        print(f"  Ingestion done in {time.monotonic()-t0:.1f}s\n", flush=True)
        time.sleep(1)  # brief settle
        if args.rebuild_vectors:
            print("Building vector sidecar…", flush=True)
            rebuild_vectors(args.base_url, args.chain)

    t0 = time.monotonic()
    overall, by_type, misses, all_results = evaluate(
        args.base_url, args.chain, instances, args.top_k, args.eval_workers
    )
    elapsed = time.monotonic() - t0

    total_correct = sum(v["correct"] for v in by_type.values())
    print(f"\n{'='*55}")
    print(f"LongMemEval  R@{args.top_k}: {overall:.1f}%  ({total_correct}/{len(instances)})")
    print(f"Evaluation time: {elapsed:.1f}s  ({len(instances)/elapsed:.1f} q/s)\n")

    print("By question type:")
    for qtype, stats in sorted(by_type.items()):
        pct = stats["correct"] / stats["total"] * 100 if stats["total"] else 0
        bar = "█" * int(pct / 5)
        print(f"  {qtype:<40} {pct:5.1f}%  {bar}")

    print_diagnostics(by_type, misses, args.top_k)

    if args.output:
        print(f"\nWriting per-instance JSONL to {args.output} …", flush=True)
        with open(args.output, "w") as out:
            for i, (inst, evidence, raw, hit_k, hit_10, hit_20) in enumerate(all_results, 1):
                out.write(json.dumps({
                    "question_id":    inst.get("question_id"),
                    "question_type":  inst.get("question_type"),
                    "question":       inst.get("question", "")[:200],
                    "hit":            hit_k,
                    "hit_10":         hit_10,
                    "hit_20":         hit_20,
                    "evidence_snip":  evidence[0][:200] if evidence else "",
                    "top_k_contents": [r["thought"].get("content", "")[:200] for r in raw[:args.top_k]],
                    "top_k_scores":   [r.get("score", {}) for r in raw[:args.top_k]],
                }) + "\n")
                if i % 100 == 0 or i == len(all_results):
                    print(f"  {i}/{len(all_results)} written", flush=True)
        print(f"Per-instance results written to {args.output}")

    sys.exit(0 if overall >= 90.0 else 1)


if __name__ == "__main__":
    main()
