---
name: mentisdb
description: Durable semantic memory for AI agents — append-only, hash-chained, searchable.
triggers: [mentisdb, memory, remember, recall, chain, thought]
---

# MentisDB Skill

## ⚡ MANDATORY STARTUP (DO THIS FIRST)

1. `mentisdb_list_chains` → pick your project's chain (don't create a new one)
2. `mentisdb_bootstrap(chain_key="<found>")` → open it (safe on existing chains)
3. `mentisdb_skill_md` → load THIS skill before reading memory
4. `mentisdb_list_agents` → reuse your existing agent identity
5. `mentisdb_recent_context` → load where you left off
6. Write `Summary` with `role: Checkpoint`

**READ BEFORE WRITE.** Never skip steps 1–5.

- **Reads never create chains.** Search, head, agents, get-thought, etc. on an unknown `chain_key` fail with not found (HTTP 404). For a new chain call `mentisdb_bootstrap` first; only bootstrap, append, import and upserts create chains.
- **Chain-scoped bearer tokens cannot call `mentisdb_list_chains`** (global-only, like webhook list/delete and skill upload/deprecate/revoke/delete). With a scoped token, skip step 1 and use the `chain_key` you were given.

### 🚫 AGENT IDENTITY RULE — DO NOT CREATE NEW AGENTS

After step 4 (`mentisdb_list_agents`), there are only two valid paths:

- **You already exist** on the chain → use your existing `agent_id` for all subsequent writes. This is the normal case.
- **You do NOT exist** on the chain → you MUST NOT create a new agent ID. Stop and ask the user which existing agent to use, or ask them to register you explicitly via `mentisdb_upsert_agent`. Under no circumstances should you write thoughts or call `mentisdb_upsert_agent` for yourself without explicit user authorization.
  **Why:** Every new agent ID pollutes the chain registry, fragments memory ownership, and makes search/context replay harder. Chains with 20+ agents are unmanageable. Only the chain owner creates agents.

## 💾 SAVE THIS SKILL LOCALLY (MANDATORY)

After every call to `mentisdb_skill_md`, **write the returned content to your harness's global skill store, replacing any older file already there** (opencode: `~/.config/opencode/skills/mentisdb/SKILL.md`; Claude Code: `~/.claude/skills/mentisdb/SKILL.md`; others: their global-skill convention). This gives you the skill across every project without re-fetching.

## 🔎 SEARCH BEFORE YOU WRITE (READ THIS BEFORE EVERY APPEND)

Appending is the most expensive thing you do to a chain — every thought is a forever-record that pollutes retrieval forever after. So **before** you call `mentisdb_append`, take 5–15 seconds and search the chain for related memory. The point of searching first is **not** to find citations; the point is to make a better decision about whether and how to write.

Run this routine **every time** you are about to write, regardless of `thought_type`:
1. **Quick scan** — `mentisdb_recent_context(last_n=20)` to see what was just thought.
2. **Targeted search** — `mentisdb_ranked_search(text="<your proposed memory, paraphrased>", limit=10)`. Use the thesaurus-expanded default; it costs the same as lexical but finds more.
3. **Tighten if needed** — narrow with `tags_any`, `thought_types`, `concepts_any`, `since`, or `entity_type` to find the precise neighbourhood of the new memory.

Use what you find to make **three** decisions, in this order.

### Decision 1: Do I actually need to write at all?

Don't write when the answer is "I already know this." Common cases where the right action is **no append**:

- The fact / decision / lesson is already in the chain (perhaps in a recent `Summary` or `LessonLearned`). Add a `DerivedFrom` edge to a *new* thought only if you genuinely have something to add; otherwise stop.
- It is short-lived, ephemeral, or session-scoped (a transient variable value, a half-formed hunch, a debug breadcrumb that won't matter tomorrow). Skip it. Use `scope: session` only if you genuinely might need it within the same session.
- It is restated boilerplate ("user wants the project to work", "I should be careful") that adds no information beyond what every other thought already implies. Skip it.
- The chain already has a `Constraint` or `LessonLearned` covering this; you can `Corrects` or `Supersedes` it instead of writing a parallel thought.

**Rule of thumb:** if the new thought wouldn't change a future `mentisdb_ranked_search` answer for any plausible query, do not write it. A smaller, denser chain is dramatically more useful than a larger, noisy one.

### Decision 2: Do I need to update a previous assumption or lesson?

Real work invalidates prior memory. Search first to surface candidates:

- A new `Correction` or `AssumptionInvalidated` only makes sense if there is an existing thought to correct. Search for the original lesson / decision / assumption and link the new thought to it with a typed edge (`Corrects`, `Invalidates`, `Supersedes`).
- If a `LessonLearned` from months ago no longer holds (the framework changed, the bug was in our code not theirs, the workaround is now standard), prefer `AssumptionInvalidated` over a parallel new `LessonLearned` and link with `Invalidates` / `Corrects` — **default search hides the superseded target** so future agents see the current truth (use `include_invalidated=true` only for audit).
- If your new memory *narrows* an existing broad one (e.g. "X is fast" → "X is fast for read-heavy workloads but slow for batch writes"), use `Supersedes` with a short note about the narrower scope. Default ranked search then excludes the broader/outdated thought and surfaces the narrower one.

### Decision 3: Which existing memories should I link to?

The search above also produces the `refs` / `relations` for the new thought. This is what makes the chain a *graph* instead of a *log*:

- Pick **1–3** of the highest-signal neighbours (the ones a future reader of the new thought would most want to see). Use the typed edge that actually fits (`DerivedFrom`, `CausedBy`, `References`, `Supports`, `Corrects`, `ContinuesFrom`, …). Avoid 10+ weak edges — they dilute the signal.
- For a brand-new decision, prefer `DerivedFrom` to the `LessonLearned` / `Question` / `Wonder` that motivated it. Future readers can replay the reasoning.
- For a new task or milestone, link `ContinuesFrom` the most recent `Checkpoint` / `Summary` so a future traversal pulls both in one chunk.
- For a fresh insight, `Supports` one or two existing `Hypothesis` thoughts — you have just turned a hypothesis into a stronger signal.

**Why:** after a few hundred unconnected thoughts ranked search returns noise and agents re-derive the same lesson. Searching first is the highest-leverage habit here; `MENTISDB_DEDUP_THRESHOLD` auto-`Supersedes` is only the downstream half.

## ✍️ WRITE TRIGGERS

Write **immediately** when any becomes true: LessonLearned (bug cause, framework trap), Decision (architectural choice, convention), Constraint (security boundary), Correction/AssumptionInvalidated, Summary/Checkpoint (restart point), TaskComplete, Question/Wonder, Hypothesis.

**One strong memory > many weak ones.** Many chains overuse standalone notes and generic `References` edges — don't.

**Search-first rule (see §Search Before You Write above):** every append must be preceded by a `mentisdb_ranked_search` for related thoughts. Use the results to decide whether to write at all, whether to update a prior thought, and which `refs` / `relations` to set.

**Minimum graph rule:** if a thought is not a pure standalone observation, add at least one backlink. Prefer 1–3 high-signal `refs` or `relations` over many weak links. When `MENTISDB_DEDUP_THRESHOLD` is set, near-duplicate content auto-emits `Supersedes`.

## 📋 THOUGHT TYPES

| Type | Use for | Role |
|---|---|---|
| LessonLearned | Durable heuristic from failure/fix | Retrospective |
| Decision | Concrete choice made | Memory |
| Constraint | Requirement or hard limit | Memory |
| Correction | Fixed version of a prior mistake | Memory |
| AssumptionInvalidated | Trusted premise no longer holds | Memory |
| Plan / Subgoal | Future work shape, not decisions already made | Memory |
| Goal | High-level objective; broader than Plan (what, not how) | Memory |
| TaskComplete | Task or milestone finished durably | Memory |
| Summary | Compressed view of prior thoughts | Checkpoint |
| Checkpoint | Explicit resumption marker | Checkpoint |
| Question / Wonder | Unresolved issue or open curiosity | Memory |
| Hypothesis | Tentative explanation or prediction | Memory |
| StateSnapshot | Broader "state of the world" capture | Memory |
| Mistake | Error in prior reasoning or action | Memory |
| LLMExtracted | Auto-extracted from free text via LLM | Memory |

Most resumable notes should be `Summary` with `role: Checkpoint`.

`thought_type` names are matched case-insensitively and ignore separators, so `LessonLearned`, `lesson-learned`, and `lesson learned` are equivalent. The table lists the types you will reach for most; the enum is the full set.

## 🔗 THOUGHT GRAPH

Link via `refs: [index]` (intra-chain) or typed `relations` with `kind` and `target_id`:

| kind | Use when |
|---|---|
| CausedBy | This thought happened because of the target |
| Corrects | Fixes an earlier mistaken fact or claim |
| Invalidates | Prior assumption no longer valid |
| Supersedes | Replaces target without claiming it was wrong |
| DerivedFrom | Insight/decision came from the target |
| Summarizes | Compresses one or more earlier thoughts |
| References | General backlink when no stronger edge fits |
| Supports | Adds evidence for the target |
| ContinuesFrom | Resumes work from a checkpoint or handoff |
| BranchesFrom | Genesis of a branch diverging from target (cross-chain) |

Default to `References` only if no stronger fit. `DerivedFrom` for "I concluded X from Y". `Corrects` when old thought was wrong; `Supersedes` when reasonable then but replaced now. Set `chain_key` on a relation for cross-chain references.

## 🌿 MEMORY CHAIN BRANCHES

Fork with `mentisdb_branch_from(source_chain_key, branch_thought_id, branch_chain_key)`; `branch_chain_key` must name a new chain (an existing one is rejected; 409 over REST). The new chain is born with a genesis thought carrying a `BranchesFrom` relation. Searches on the branch **transparently include ancestor chain results** annotated with `chain_key`. Use to isolate risky experiments, let sub-agents work in their own space, or fork per tenant/feature. Merge back explicitly with `mentisdb_merge_chains`.

## 🤖 SUB-AGENT ORCHESTRATION

1. **Pre-warm with shared memory** — load the chain before spawning.
2. **Keep context ≤50%** — write `Summary` / `Checkpoint` / handoffs BEFORE hitting limits or being compacted.
3. **Write a `TaskComplete` immediately when work finishes** — don't wait to be asked.
4. **Handoffs = `Summary` with `role: Checkpoint`** — include what's done, pending, and next steps.
5. **PM pattern** — one coordinator decomposes, dispatches parallel specialists, synthesizes wave by wave.
6. **Flush pending memories** (`LessonLearned`, `Decision`, `Constraint`) before exit.
7. **Branch for experiments** — see §Memory Chain Branches.

## 🧩 SKILL REGISTRY

Git-like immutable version store for agent behaviour. Tools: `mentisdb_upload_skill`, `mentisdb_read_skill`, `mentisdb_list_skills`, `mentisdb_search_skill`, `mentisdb_skill_versions`, `mentisdb_deprecate_skill`, `mentisdb_revoke_skill`, `mentisdb_delete_skill`. Revoke keeps an audit row; delete removes the skill and all versions. Always check `warnings` in the read response before trusting content.

## 🔍 RETRIEVAL

| Need | Tool |
|---|---|
| **Most queries** (topical, semantic, multi-hop) | `mentisdb_ranked_search` (default) |
| Strict keyword match, no expansion | `mentisdb_lexical_search` |
| Recent context | `mentisdb_recent_context(last_n=N)` |
| One thought / first thought | `mentisdb_get_thought` / `mentisdb_get_genesis_thought` |
| Page history | `mentisdb_traverse_thoughts` |
| Grouped context | `mentisdb_context_bundles` |
| Cross-chain federated | `mentisdb_federated_search` |

`mentisdb_ranked_search` applies the built-in thesaurus (~900 headwords + 300+ lemmas, stemmed) on every query: better recall on synonyms and verb forms with no extra parameters.

**Entity types** — filter with `entity_type`. Register first via `mentisdb_upsert_entity_type`. **Always filter** — text, tags, concepts, types, scope, or time window.

**RRF reranking** — `enable_reranking=true`, `rerank_k=50` on `mentisdb_ranked_search` adds a consensus bonus when lexical, vector and graph signals agree. Use when signals disagree on top candidates.

**Branch-aware search** — branch searches transparently include ancestor results with `chain_key` annotation.

## 🏷️ METADATA & SCOPES

`tags` (short labels), `concepts` (ideas), `importance` 0.0–1.0 (user≈0.8, assistant≈0.2), `confidence` 0.0–1.0, `entity_type` (per-chain ontology), `source_episode` (provenance).

Scopes stored as `scope:{variant}` tags: `user` (default; all agents sharing the user identity), `session` (only the creating session), `agent` (only the creating agent).

## ❌ ANTI-PATTERNS

Raw-log writes instead of rules. **New agent IDs for the same role** — see §Agent Identity Rule above; this is the #1 chain pollution vector. Skipping `recent_context` at start. Vague summaries. Redundant bootstraps. Unfiltered full-chain loads. No checkpoint before compaction. Sub-agents spawned without shared-memory pre-warm or dying without flushing memories. Writing near-duplicates when dedup is on. **Deferring memory writes** — save `TaskComplete` and `LessonLearned` the moment they happen, not when prompted. **Blind appends** — writing a new thought without first running `mentisdb_ranked_search` to check whether the chain already says it, whether an old lesson needs to be invalidated, or which neighbours to link to (see §Search Before You Write above).

### Sub-agents & Agent Identity

When spawning sub-agents, the coordinator MUST tell each sub-agent which `agent_id` to use. Sub-agents do NOT pick their own identity. Acceptable patterns: reuse the coordinator's `agent_id`; use a pre-existing, explicitly created agent ID; or ask the user to register a new agent ID first. A sub-agent that follows the Mandatory Startup sequence and finds itself missing MUST stop and ask — it must never auto-create.

## 🐕 DOGFOODING — Use the Chain to Coordinate Your Own Work

Coordinators and multi-step agents coordinate through the chain, not markdown files or prompts:

1. **Plans in the chain** — write N `Plan` thoughts; sub-agents claim one with a `Subgoal` (`CausedBy` the plan) and finish with `TaskComplete`.
2. **Constraints persist across waves** — style guides, budgets, API pins as `Constraint` thoughts that new sub-agents search on spawn.
3. **Typed relations, not prose references** — `Corrects`, `Supersedes`, `DerivedFrom` let one graph query surface every dependent thought.
4. **Flag drift in `TaskComplete`** — record concerns and stale API names, not just completions.
5. **Fight API drift** — doc-tested examples, canonical API names as an early `Decision`, cite file:line for non-obvious choices.
6. **Forward references need graph integrity** — link with `refs` / cross-chain relations so one query verifies they resolve.
7. **A markdown file is a journal, not a memory** — the chain records what happened, why, what it relates to, and what supersedes it.

## 📖 COOKBOOK

Full patterns and copy-paste recipes: docs.mentisdb.com/cookbook (itself a worked example of every pattern in this skill).
