# Agent Memory Usage Guide

This guide explains how agents should use MentisDB as their primary persistent
memory layer across sessions.

---

## Core Principle

The context window is ephemeral.  MentisDB is the source of truth.

Do not wait until the context is full before writing to MentisDB.  Write early,
write often, and always recover state from MentisDB at the start of every
session.

---

## Standard Session Lifecycle

### 1. Session Start — Recover State

Before any reasoning, call `mentisdb_recent_context` to reload prior state.
Then write a `Summary` checkpoint tagged `context-reload` to record what was
reloaded.

```json
{
  "tool": "mentisdb_recent_context",
  "chain_key": "my-project",
  "limit": 20
}
```

After reading the response, write a reload confirmation:

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "Summary",
  "role": "Checkpoint",
  "tags": ["context-reload", "session-start"],
  "content": "Reloaded context. Read thoughts #12–#31. Current task: implement cross-chain search. Open question: should pagination be cursor-based?"
}
```

### 2. During Work — Persist Continuously

Write a thought whenever a meaningful decision, discovery, plan, or error
occurs.  Do not batch — write immediately so the state is durable if the
session is interrupted.

**Decision example:**

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "Decision",
  "content": "Use cursor-based pagination for mentisdb_recent_context to avoid offset drift on large chains.",
  "tags": ["api-design", "pagination"]
}
```

**Mistake + lesson example:**

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "Mistake",
  "content": "Called mentisdb_append without setting chain_key. Thoughts went to the default chain instead of the project chain.",
  "tags": ["api-misuse"]
}
```

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "LessonLearned",
  "content": "Always pass chain_key explicitly. Never rely on the default chain for project work.",
  "tags": ["api-usage"],
  "refs": [42]
}
```

### 3. At ~40 % Context — Write Retrospective and Respawn

When your context usage reaches approximately 40 %, write a retrospective
before terminating the session.

```json
{
  "tool": "mentisdb_append_retrospective",
  "chain_key": "my-project",
  "content": "Completed: schema migration for chain V2. In progress: cross-chain search (phases 1 and 2 done). Blocked: need vector index support before phase 3. Next session should start with cross-chain search phase 3.",
  "tags": ["handoff", "session-end"]
}
```

Terminate the session immediately after writing the retrospective.  The next
agent instance will reload context using step 1.

### 4. Task Completion — Write TaskComplete

When a concrete deliverable is finished, record it before closing.

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "TaskComplete",
  "content": "Cross-chain search implemented. PR #88 merged. All existing tests pass.",
  "tags": ["milestone"]
}
```

---

## Memory Types Quick Reference

| `thought_type`     | When to use |
|--------------------|-------------|
| `Decision`         | A concrete design or implementation choice was made. |
| `Insight`          | A non-obvious technical lesson or realisation. |
| `Mistake`          | A wrong action or misdirection (not a factual error). |
| `Correction`       | An earlier factual belief was wrong; this replaces it. |
| `LessonLearned`    | A retrospective operating rule distilled from a failure. |
| `Plan`             | Future work shape that is more committed than an idea. |
| `Summary`          | Session snapshot, checkpoint, or retrospective. |
| `TaskComplete`     | A concrete deliverable was finished. |
| `Constraint`       | A hard boundary or rule that must not drift. |

---

## Rust Helper API

When using MentisDB directly as a Rust crate, the `mentisdb::helpers` module
provides pre-configured [`ThoughtInput`] constructors that reduce boilerplate.

```rust
use mentisdb::helpers::{
    thought_decision, thought_insight, thought_mistake,
    thought_lesson_learned, thought_retrospective, thought_task_complete,
    thought_checkpoint,
};
use mentisdb::{MentisDb, BinaryStorageAdapter};

// Record a decision
let input = thought_decision("Use binary storage for all new chains.")
    .with_agent_name("architect-agent")
    .with_tags(["storage", "architecture"]);

// Record an end-of-session retrospective
let retro = thought_retrospective("Session complete. Migrated 3 chains. Next: index rebuild.")
    .with_agent_name("architect-agent")
    .with_tags(["session-end", "handoff"]);

// Record a completion
let done = thought_task_complete("PR #42 merged: binary storage migration complete.")
    .with_agent_name("architect-agent");
```

---

## Multi-Agent Orchestration Pattern

When a Project Manager (PM) agent spawns sub-agents, each sub-agent should:

1. Receive the `chain_key` and an optional `session_id` from the PM.
2. Call `mentisdb_recent_context` immediately on start.
3. Write a bootstrap `Summary` if this is the agent's first time on the chain.
4. Append thoughts continuously during work.
5. Write a `Handoff` summary before terminating so the PM can confirm completion.

```json
{
  "tool": "mentisdb_append",
  "chain_key": "my-project",
  "thought_type": "Handoff",
  "role": "Handoff",
  "content": "Sub-agent search-specialist finished phase 1 of cross-chain search. Handed off result to PM. Next: phase 2 (ranking).",
  "tags": ["handoff", "sub-agent", "search-specialist"]
}
```

---

## Context Overflow Prevention Checklist

- [ ] Called `mentisdb_recent_context` at session start.
- [ ] Writing thoughts after each meaningful step (not batching).
- [ ] Context usage < 40 % — if not, write retrospective and respawn.
- [ ] All decisions, mistakes, and plans are persisted before session end.
- [ ] TaskComplete written for every finished deliverable.
- [ ] Handoff written before terminating if another agent will continue.
