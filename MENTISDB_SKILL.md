---
name: mentisdb
description: Durable semantic memory for AI agents — append-only, hash-chained, searchable.
triggers:
  - mentisdb
  - memory
  - remember
  - recall
  - chain
  - thought
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

## 💾 SAVE THIS SKILL LOCALLY (MANDATORY)

After every call to `mentisdb_skill_md`, **write the returned content to your harness's global skill store, replacing any older file that is already there**. The on-disk copy must always be the latest version you fetched from the server.

- opencode: `~/.config/opencode/skills/mentisdb/SKILL.md`
- Claude Code: `~/.claude/skills/mentisdb/SKILL.md`
- Cursor / Codex / other MCP harnesses: follow their global-skill convention

This gives you the skill across every project without re-fetching, and ensures old advice never shadows the current release.

## ✍️ WRITE TRIGGERS

Write **immediately** when any becomes true:

| Trigger | Type | Role | Link |
|---|---|---|---|
| Non-obvious bug cause | LessonLearned | Retrospective | `refs` the Mistake/Checkpoint that exposed it |
| Architectural decision | Decision | Memory | `refs` or `DerivedFrom` the evidence/Plan |
| Security boundary found | Constraint | Memory | `refs` the code path, audit, or incident |
| Stable convention | Decision | Memory | `refs` the motivating example |
| Dangerous assumption corrected | Correction / AssumptionInvalidated | Memory | `Corrects` / `Invalidates` the prior thought |
| Restart point reached | Summary | Checkpoint | `ContinuesFrom` the prior Checkpoint |
| Framework/ecosystem trap | LessonLearned | Retrospective | `refs` the failed attempt |
| Expensive op ahead | Plan or Summary | Checkpoint | `refs` the current state |
| Tool call surprise | Surprise / LessonLearned | Retrospective | `refs` the triggering action |
| Task finished durably | TaskComplete | Memory | `refs` the Plan/Subgoal/Decision closed |
| Open question / uncertain | Question or Wonder | Memory | `refs` the blocking context |
| Tentative explanation | Hypothesis | Memory | `refs` the observations |

**One strong memory > many weak ones.** Many chains overuse standalone notes and generic `References` edges — don't.

**Minimum graph rule:** if a thought is not a pure standalone observation, add at least one backlink. Prefer 1–3 high-signal refs over many weak links.

- `refs` for lightweight lineage inside one chain.
- `relations` when the edge meaning matters at retrieval / replay / correction / handoff.

When `MENTISDB_DEDUP_THRESHOLD` is set, near-duplicate content auto-emits `Supersedes` — no manual dedup link needed.

## 📋 THOUGHT TYPES

| Type | Use for | Role |
|---|---|---|
| PreferenceUpdate | User/team preference changed or became explicit | Memory |
| UserTrait | Durable characteristic of the user | Memory |
| RelationshipUpdate | Agent's model of its user relationship changed | Memory |
| Finding | Concrete observation recorded | Memory |
| Insight | Higher-level synthesis or realization | Memory |
| FactLearned | Factual piece of information learned | Memory |
| PatternDetected | Recurring pattern across events | Memory |
| Hypothesis | Tentative explanation or prediction | Memory |
| Mistake | Error in prior reasoning or action | Memory |
| Correction | Corrected version of a prior mistake | Memory |
| LessonLearned | Durable operating heuristic from failure/fix | Retrospective |
| AssumptionInvalidated | Trusted premise no longer holds | Memory |
| Constraint | Requirement or hard limit identified | Memory |
| Plan | Plan for future work created or updated | Memory |
| Subgoal | Smaller unit carved from a broader plan | Memory |
| Decision | Concrete choice made | Memory |
| StrategyShift | Agent changed its overall approach | Memory |
| Wonder | Open-ended curiosity | Memory |
| Question | Unresolved question preserved | Memory |
| Idea | Possible future direction | Memory |
| Experiment | Experiment or trial proposed/executed | Memory |
| ActionTaken | Meaningful action performed | Memory |
| TaskComplete | Task or milestone finished durably | Memory |
| Checkpoint | Explicit resumption marker | Checkpoint |
| StateSnapshot | Broader "state of the world" capture | Memory |
| Handoff | Work/context handed to another actor | Memory |
| Summary | Compressed view of prior thoughts | Checkpoint |
| Surprise | Unexpected outcome or mismatch | Memory |
| Reframe | Prior thought was fine but poorly framed | Memory |
| Goal | High-level objective (broader than Plan/Subgoal) | Memory |
| LLMExtracted | Memory auto-extracted from free text via LLM | Memory |

**Commonly skipped but high-value:** `AssumptionInvalidated` (premise collapsed), `Question` (durable open issue), `StateSnapshot` (pre-refactor capture), `Mistake` (pair with `LessonLearned` or `Correction`), `Plan` / `Subgoal` (describe future shape, not decisions already made). Most resumable notes should be `Summary` with `role: Checkpoint` — reserve `Checkpoint` as a type for explicit resume markers.

## 🔗 THOUGHT GRAPH

Every thought can link via **`refs: [index]`** (positional, intra-chain) or typed **`relations`** with `kind` and `target_id`:

| kind | Use when |
|---|---|
| CausedBy | This thought happened because of the target |
| Corrects | Fixes an earlier mistaken fact or claim |
| Invalidates | Prior assumption, premise, or result is no longer valid |
| Supersedes | Replaces the target without claiming it was wrong |
| DerivedFrom | Insight/decision/plan came from the target |
| Summarizes | Compresses one or more earlier thoughts |
| References | General backlink when no stronger edge fits |
| Supports | Adds evidence for the target |
| Contradicts | Conflicts with the target |
| ContinuesFrom | Resumes work from a checkpoint, handoff, or branch point |
| BranchesFrom | Genesis of a branch diverging from the target (cross-chain) |
| RelatedTo | Loose semantic connection |

Heuristics:

- Default to `References` only if no stronger relationship fits.
- `DerivedFrom` for "I concluded X from Y" — usually better than `References`.
- `ContinuesFrom` for resumptions, handoffs, follow-up checkpoints.
- `Corrects` when the old thought was actually wrong; `Supersedes` when it was reasonable then but replaced now.
- `Invalidates` when reality changed or an assumption collapsed.
- `Summarizes` when a checkpoint or summary condenses earlier work.

Relations support optional `valid_at` / `invalid_at` timestamps for time-bounded facts; `append_thought` auto-sets `valid_at` to now if omitted. Set `chain_key` on a relation for a **cross-chain reference**.

## 🌿 MEMORY CHAIN BRANCHES

Chains can be forked. `mentisdb_branch_from` creates a new chain that diverges from a specific thought on an existing chain:

```
mentisdb_branch_from(
  source_chain_key="main-project",
  branch_thought_id="<uuid>",
  branch_chain_key="experiment-1"
)
```

The new chain is born with one genesis thought carrying a `BranchesFrom` relation pointing at the fork point. Searches on the branch **transparently include ancestor chain results** (ancestors are walked transitively: parent → grandparent → …), annotated with a `chain_key` field so you know where each hit came from. Use branching to:

- Isolate risky **experiments** from the main chain.
- Let sub-agents work in their own space while still reading shared context.
- Try alternative approaches without polluting the primary memory stream.
- Fork per tenant, per feature, or per hypothesis.

Merging a branch back in is explicit: use `mentisdb_merge_chains`.

## 🤖 SUB-AGENT ORCHESTRATION

1. **Pre-warm with shared memory** — load the chain before spawning.
2. **Keep context ≤50%** — write `Summary` / `Checkpoint` / handoffs BEFORE hitting limits or being compacted.
3. **Write a `TaskComplete` immediately when work finishes** — don't wait to be asked. Save it the moment a task completes, not at the end of the conversation.
4. **Handoffs = `Summary` with `role: Checkpoint`** — include what's done, pending, and next steps.
5. **PM pattern** — one coordinator decomposes, dispatches parallel specialists, synthesizes wave by wave.
6. **Flush pending memories** (`LessonLearned`, `Decision`, `Constraint`) before exit — unsaved learnings die with the agent.
7. **Branch for experiments** — see §Memory Chain Branches above.

## 🧩 SKILL REGISTRY

Git-like immutable version store for agent behaviour. Every upload is a new version; old versions stay accessible. Tools: `mentisdb_upload_skill`, `mentisdb_read_skill`, `mentisdb_list_skills`, `mentisdb_search_skill`, `mentisdb_skill_versions`, `mentisdb_deprecate_skill`, `mentisdb_revoke_skill`, `mentisdb_skill_manifest`. Always check `warnings` in the read response before trusting content. Upload an improved skill after learning something durable — the fleet's collective knowledge compounds.

## 🔍 RETRIEVAL

| Need | Tool |
|---|---|
| Topical search | `mentisdb_ranked_search` |
| Keyword match | `mentisdb_lexical_search` |
| Recent context | `mentisdb_recent_context(last_n=N)` |
| One thought | `mentisdb_get_thought` |
| First thought | `mentisdb_get_genesis_thought` |
| Page history | `mentisdb_traverse_thoughts` |
| Grouped context | `mentisdb_context_bundles` |
| Cross-chain federated | `mentisdb_federated_search` |

**Entity types** — filter any search with the `entity_type` parameter. Register a label first via `mentisdb_upsert_entity_type`.

**Always filter** — text, tags, concepts, types, scope, or time window.

**RRF reranking** — `enable_reranking=true`, `rerank_k=50` on `mentisdb_ranked_search`. Produces lexical-only / vector-only / graph-only rankings, merges via `1/(k + rank)`. Use when signals disagree on top candidates (helps multi-hop queries).

**Branch-aware search** — searching a branch chain transparently includes ancestor results with `chain_key` annotation.

## 🏷️ METADATA & SCOPES

`tags` (short labels), `concepts` (ideas), `importance` 0.0–1.0 (user≈0.8, assistant≈0.2 — tips close BM25 races), `confidence` 0.0–1.0 (tie-break), `entity_type` (per-chain ontology label), `source_episode` (provenance string).

Scopes stored as `scope:{variant}` tags — set on append, filter in search:

- `user` (default): visible to all agents sharing the user identity.
- `session`: visible only within the creating session.
- `agent`: visible only to the creating agent.

## ❌ ANTI-PATTERNS

Raw-log writes instead of rules. New agent IDs for the same role. Skipping `recent_context` at start. Vague summaries. Redundant bootstraps. Unfiltered full-chain loads. No checkpoint before compaction. Sub-agents spawned without shared-memory pre-warm or dying without flushing memories. Writing near-duplicates when dedup is on (auto-superseded anyway). **Deferring memory writes** — save `TaskComplete` and `LessonLearned` the moment they happen, not when prompted.
