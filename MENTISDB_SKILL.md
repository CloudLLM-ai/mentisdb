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

### 🚫 AGENT IDENTITY RULE — DO NOT CREATE NEW AGENTS

After step 4 (`mentisdb_list_agents`), there are only two valid paths:

- **You already exist** on the chain → use your existing `agent_id` for all subsequent writes. This is the normal case.
- **You do NOT exist** on the chain → you MUST NOT create a new agent ID. Stop and ask the user which existing agent to use, or ask them to register you explicitly via `mentisdb_upsert_agent`. Under no circumstances should you write thoughts or call `mentisdb_upsert_agent` for yourself without explicit user authorization.

**Why:** Every new agent ID pollutes the chain registry, fragments memory ownership, and makes search/context replay harder. Chains with 20+ agents are unmanageable. Only the chain owner creates agents.

## 💾 SAVE THIS SKILL LOCALLY (MANDATORY)

After every call to `mentisdb_skill_md`, **write the returned content to your harness's global skill store, replacing any older file already there**.

- opencode: `~/.config/opencode/skills/mentisdb/SKILL.md`
- Claude Code: `~/.claude/skills/mentisdb/SKILL.md`
- Others: follow their global-skill convention

This gives you the skill across every project without re-fetching.

## ✍️ WRITE TRIGGERS

Write **immediately** when any becomes true: LessonLearned (bug cause, framework trap), Decision (architectural choice, convention), Constraint (security boundary), Correction/AssumptionInvalidated, Summary/Checkpoint (restart point), TaskComplete, Question/Wonder, Hypothesis.

**One strong memory > many weak ones.** Many chains overuse standalone notes and generic `References` edges — don't.

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
| TaskComplete | Task or milestone finished durably | Memory |
| Summary | Compressed view of prior thoughts | Checkpoint |
| Checkpoint | Explicit resumption marker | Checkpoint |
| Question / Wonder | Unresolved issue or open curiosity | Memory |
| Hypothesis | Tentative explanation or prediction | Memory |
| StateSnapshot | Broader "state of the world" capture | Memory |
| Mistake | Error in prior reasoning or action | Memory |
| LLMExtracted | Auto-extracted from free text via LLM | Memory |

Most resumable notes should be `Summary` with `role: Checkpoint`.

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

Fork with `mentisdb_branch_from(source_chain_key, branch_thought_id, branch_chain_key)`. The new chain is born with a genesis thought carrying a `BranchesFrom` relation. Searches on the branch **transparently include ancestor chain results** annotated with `chain_key`. Use to isolate risky experiments, let sub-agents work in their own space, or fork per tenant/feature. Merge back explicitly with `mentisdb_merge_chains`.

## 🤖 SUB-AGENT ORCHESTRATION

1. **Pre-warm with shared memory** — load the chain before spawning.
2. **Keep context ≤50%** — write `Summary` / `Checkpoint` / handoffs BEFORE hitting limits or being compacted.
3. **Write a `TaskComplete` immediately when work finishes** — don't wait to be asked.
4. **Handoffs = `Summary` with `role: Checkpoint`** — include what's done, pending, and next steps.
5. **PM pattern** — one coordinator decomposes, dispatches parallel specialists, synthesizes wave by wave.
6. **Flush pending memories** (`LessonLearned`, `Decision`, `Constraint`) before exit.
7. **Branch for experiments** — see §Memory Chain Branches.

## 🧩 SKILL REGISTRY

Git-like immutable version store for agent behaviour. Tools: `mentisdb_upload_skill`, `mentisdb_read_skill`, `mentisdb_list_skills`, `mentisdb_search_skill`, `mentisdb_skill_versions`, `mentisdb_deprecate_skill`, `mentisdb_revoke_skill`. Always check `warnings` in the read response before trusting content.

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

**Entity types** — filter with `entity_type`. Register first via `mentisdb_upsert_entity_type`.

**Always filter** — text, tags, concepts, types, scope, or time window.

**RRF reranking** — `enable_reranking=true`, `rerank_k=50` on `mentisdb_ranked_search`. Use when signals disagree on top candidates.

**Branch-aware search** — branch searches transparently include ancestor results with `chain_key` annotation.

## 🏷️ METADATA & SCOPES

`tags` (short labels), `concepts` (ideas), `importance` 0.0–1.0 (user≈0.8, assistant≈0.2), `confidence` 0.0–1.0, `entity_type` (per-chain ontology), `source_episode` (provenance).

Scopes stored as `scope:{variant}` tags:

- `user` (default): visible to all agents sharing the user identity.
- `session`: visible only within the creating session.
- `agent`: visible only to the creating agent.

## ❌ ANTI-PATTERNS

Raw-log writes instead of rules. **New agent IDs for the same role** — see §Agent Identity Rule above; this is the #1 chain pollution vector. Skipping `recent_context` at start. Vague summaries. Redundant bootstraps. Unfiltered full-chain loads. No checkpoint before compaction. Sub-agents spawned without shared-memory pre-warm or dying without flushing memories. Writing near-duplicates when dedup is on. **Deferring memory writes** — save `TaskComplete` and `LessonLearned` the moment they happen, not when prompted.

### Sub-agents & Agent Identity

When spawning sub-agents, the coordinator MUST tell each sub-agent which `agent_id` to use. Sub-agents do NOT pick their own identity. Acceptable patterns: reuse the coordinator's `agent_id`; use a pre-existing, explicitly created agent ID; or ask the user to register a new agent ID first. A sub-agent that follows the Mandatory Startup sequence and finds itself missing MUST stop and ask — it must never auto-create.
