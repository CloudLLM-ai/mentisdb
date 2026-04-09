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

## ✍️ WRITE TRIGGERS

Write **immediately** when any becomes true:

| Trigger | Type | Role |
|---------|------|------|
| Non-obvious bug cause | LessonLearned | Retrospective |
| Architectural decision | Decision | Memory |
| Security boundary found | Constraint | Memory |
| Stable convention established | Decision | Memory |
| Dangerous assumption corrected | Correction | Memory |
| Restart point reached | Summary | Checkpoint |
| Framework/ecosystem trap | LessonLearned | Retrospective |
| Expensive operation ahead | Summary | Checkpoint |
| Tool call surprise | LessonLearned | Retrospective |
| Task finished durably | TaskComplete | Memory |
| Uncertain about direction | Wonder | Memory |
| Tentative explanation | Hypothesis | Memory |

**One strong memory > many weak ones.** Link to prior thoughts with `refs` or `relations`.

## 📋 THOUGHT TYPES

| Type | Use for | Role |
|------|---------|------|
| PreferenceUpdate | User/team preference changed or became explicit | Memory |
| UserTrait | Durable characteristic of the user learned | Memory |
| RelationshipUpdate | Agent's model of its relationship with the user changed | Memory |
| Finding | Concrete observation recorded | Memory |
| Insight | Higher-level synthesis or realization | Memory |
| FactLearned | Factual piece of information learned | Memory |
| PatternDetected | Recurring pattern across events or interactions | Memory |
| Hypothesis | Tentative explanation or prediction | Memory |
| Mistake | Error in prior reasoning or action | Memory |
| Correction | Corrected version of a prior mistake (replaces fact) | Memory |
| LessonLearned | Durable operating heuristic distilled from failure/fix | Retrospective |
| AssumptionInvalidated | Previously trusted assumption is now wrong | Memory |
| Constraint | Requirement or hard limit identified | Memory |
| Plan | Plan for future work created or updated | Memory |
| Subgoal | Smaller unit carved from a broader plan | Memory |
| Decision | Concrete choice made | Memory |
| StrategyShift | Agent changed its overall approach | Memory |
| Wonder | Open-ended curiosity or exploration | Memory |
| Question | Unresolved question preserved | Memory |
| Idea | Possible future direction proposed | Memory |
| Experiment | Experiment or trial proposed/executed | Memory |
| ActionTaken | Meaningful action performed | Memory |
| TaskComplete | Task or milestone finished durably | Memory |
| Checkpoint | Resumption point recorded | Checkpoint |
| StateSnapshot | Broader snapshot of current state | Memory |
| Handoff | Work/context explicitly handed to another actor | Memory |
| Summary | Compressed view of prior thoughts | Checkpoint |
| Surprise | Unexpected outcome or mismatch observed | Memory |
| Reframe | Prior thought was accurate but unhelpfully framed (Supersedes without invalidating) | Memory |
| Goal | High-level objective or desired outcome (what, not how — broader than Plan/Subgoal) | Memory |

## 🔗 BACK-REFERENCING & THOUGHT GRAPH

Every thought can link to prior thoughts via two mechanisms. **Always link when your new thought depends on, corrects, or derives from an earlier one.** A chain with explicit references is both searchable and navigable — it forms a thought graph that agents can traverse.

- **`refs: [index]`** — positional back-references (zero-based chain indices). Simple, compact, intra-chain only.
- **`relations`** — typed semantic edges with `kind` and `target_id` (UUID):

| kind | Use when |
|------|----------|
| CausedBy | This thought was caused by the target |
| Corrects | This thought corrects the target's fact |
| Supersedes | This thought replaces the target's framing (Reframe) |
| DerivedFrom | This insight was derived from the target |
| Summarizes | This thought summarizes the target |
| References | General reference to the target |
| Supports | This thought supports the target's claim |
| Contradicts | This thought contradicts the target |
| ContinuesFrom | This continues work from the target |
| RelatedTo | Loose semantic connection |

Set `chain_key` on a relation to create a **cross-chain reference**.

**Prefer 1–3 high-signal refs over many weak links.** Always reference the exact prior Decision, Mistake, or Checkpoint that gave rise to your new thought.

## 🤖 SUB-AGENT ORCHESTRATION

When dispatching sub-agents:

1. **Pre-warm with shared memory** — load the chain before spawning so each agent inherits project state
2. **Keep context ≤50%** — sub-agents must write `Summary` checkpoints, findings, and handoffs BEFORE hitting context limits or being killed/compacted
3. **Write a TaskComplete** when a leaf task finishes durably
4. **Write handoffs as Summary with role Checkpoint** — include what was done, what's pending, and what the next agent should pick up
5. **Use the PM pattern** — one project manager decomposes work, dispatches parallel specialists, and synthesizes results wave by wave
6. **Sub-agents must flush pending memories** (LessonLearned, Decision, Constraint) before exiting — if an agent dies without writing, its learnings are lost

## 🧩 SKILL REGISTRY

MentisDB includes a **skill manager** that works like git for agent behavior:

- **Upload** a skill → creates an immutable version (like a git commit)
- **Read** a skill → returns content + warnings + status (check warnings before trusting content!)
- **Version** → every upload creates a new version; old versions stay accessible for audit
- **Deprecate** → marks a skill as outdated (like a git tag, not deletion)
- **Revoke** → marks a skill as dangerous/compromised (like a git revert)
- **Search** → find skills by name, tag, trigger, or uploader

Tools: `mentisdb_upload_skill`, `mentisdb_read_skill`, `mentisdb_list_skills`, `mentisdb_search_skill`, `mentisdb_skill_versions`, `mentisdb_deprecate_skill`, `mentisdb_revoke_skill`, `mentisdb_skill_manifest`

**Self-improving agents:** After learning something new about your domain, upload an updated skill so the fleet's collective knowledge compounds over time. A skill checked in at the start of a project is better by the end of it.

## 🔍 RETRIEVAL

| Need | Tool |
|------|------|
| Topical search | `mentisdb_ranked_search` |
| Keyword match | `mentisdb_lexical_search` |
| Recent context | `mentisdb_recent_context(last_n=N)` |
| One thought | `mentisdb_get_thought` |
| First thought | `mentisdb_get_genesis_thought` |
| Page history | `mentisdb_traverse_thoughts` |
| Grouped context | `mentisdb_context_bundles` |
| Export markdown | `mentisdb_memory_markdown` |
| Import markdown | `mentisdb_import_memory_markdown` |

**Always filter** — supply text, tags, concepts, types, or time window.

## 🏷️ SEARCHABILITY

- `tags`: `rust`, `security`, `api-design`
- `concepts`: `hybrid-retrieval`, `session-bootstrap`
- `importance`: 0.0–1.0 (user=0.8, assistant=0.2)
- `confidence`: 0.0–1.0

## ❌ ANTI-PATTERNS

- Writing raw logs instead of rules
- Creating new agent IDs for same role
- Skipping `recent_context` at start
- Vague summaries ("worked on X")
- Polluting chains with redundant bootstrap
- Loading entire chains without filters
- Forgetting to write checkpoint before context compaction
- Dispatching sub-agents without pre-warming with shared memory
- Letting sub-agents die without flushing pending memories