# mentisdb Standards

`mentisdb` follows the same engineering bar as `cloudllm`.

## Scope

- `mentisdb` is a standalone crate.
- `cloudllm` may depend on `mentisdb`.
- `mentisdb` must not depend on `cloudllm`.

## Standards

- Public API changes require rustdoc on exported types and functions.
- New behavior requires tests.
- Serialization and persistence changes must preserve explicit integrity checks.
- Retrieval and export features should be generic enough to support direct crate use, MCP services, REST services, and CLI tools.
- Crate metadata must remain suitable for crates.io publishing.

## Design Bias

- Prefer semantic memory primitives over agent-framework-specific abstractions.
- Keep storage durable and append-only.
- Keep query and export APIs first-class, not bolted on as prompt helpers.

---

# MentisDB Memory Policy

MentisDB is the **primary persistent memory system** for all agents operating in this repository.
The context window is non-authoritative and ephemeral. MentisDB is the source of truth.

## Rules

1. Never rely solely on the context window for state across steps or sessions.
2. Persist important information using `mentisdb_append` after every meaningful reasoning step.
3. At approximately 40 % context usage:
   - Write a retrospective (`mentisdb_append_retrospective`) summarising work so far.
   - Terminate the current session and respawn.
4. On every new task or session start:
   - Call `mentisdb_recent_context` **before** reasoning to restore prior state.

## Memory Types

| Type | When to use |
|------|-------------|
| `Decision` | A concrete design or implementation choice was made. |
| `Insight` | A non-obvious technical lesson or useful realisation. |
| `Mistake` | A wrong action or misdirection — distinct from a factual error. |
| `Plan` | Future work shape that is more committed than an idea. |
| `LessonLearned` | A retrospective operating rule distilled from a failure or expensive fix. |
| `Correction` | An earlier factual belief was wrong; this replaces it. |
| `Retrospective` | End-of-session summary written before termination or handoff. |
| `TaskComplete` | A concrete deliverable was finished. |

## Required Behaviours

- Persist after meaningful reasoning steps (decisions, plans, discoveries).
- Persist before task completion or session termination.
- Persist when switching subtasks or handing off to another agent.
- Use `mentisdb_recent_context` to recover state at the start of every session.

## Standard Operating Loop

1. **On start** — call `mentisdb_recent_context`; load and acknowledge prior state.
2. **During work** — append decisions, insights, plans, and corrections continuously.
3. **At ~40 % context** — call `mentisdb_append_retrospective` (a `Summary` with role `Retrospective`), then end the session.
4. **On resume** — reload memory via `mentisdb_recent_context`; continue seamlessly.

## Memory Triggers

You **MUST** persist a thought when:

- You make a decision that downstream work depends on.
- You discover a non-obvious bug cause or systemic risk.
- You encounter an error or costly misdirection.
- You define or revise a plan.
- You complete a subtask or deliverable.
- You are about to terminate or hand off to another agent.
