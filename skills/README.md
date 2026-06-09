---
name: skills-index
description: Index of all skills in this folder — read this first to find the right skill for the task.
---

# MentisDB Project Skills

This folder holds project-local skills that agents and humans working on
MentisDB should read. Skills are versioned, immutable markdown files with
a YAML frontmatter (name, description, triggers) so they can be loaded
by both human operators and AI agents.

## How to use this folder

For humans: scan the list, read the frontmatter `description`, follow the
`triggers` to know when each skill applies.

For agents: read the frontmatter. If your task matches a `triggers`
phrase, load the full skill before proceeding.

## Skill list

### Process & engineering

- **`engineering-pipeline.md`** — release engineering pipeline (Phases 1-5).
  Triggers: "release pipeline", "ship mentisdb", "version bump", "release checklist".
- **`cookbook-as-test.md`** — pipeline for keeping the cookbook's code
  examples honest: HTML → extract → compile → fix. Triggers:
  "cookbook as test", "extract cookbook examples", "API drift in docs",
  "docs test fail".
- **`parallel-sub-agent-coordination.md`** — the 7 lessons from
  writing the cookbook with parallel sub-agents (Plans in chain,
  Constraint thoughts, typed relations, flag drift in TaskComplete,
  etc.). Triggers: "parallel sub-agents", "sub-agent coordination",
  "PM pattern", "agent memory discipline".
- **`mentisdb-public-api-reference.md`** — verified current public
  API surface (enum names, builder methods, return types). Triggers:
  "what is the current API for X", "verify API name", "API reference".

### Authoring & documentation

- **`search-first-discipline.md`** — project-local version of the
  search-first rule (always search the chain before writing). Triggers:
  "search first", "don't blind-append", "writing to chain".
- **`task-complete-with-flags.md`** — pattern for `TaskComplete`
  thoughts that include concerns and drift, not just completions.
  Triggers: "task complete", "wrap up task", "report work done".
- **`plans-in-chain-not-prompts.md`** — write `Plan` and `Subgoal`
  thoughts in the chain so sub-agents can find them, not as
  free-text instructions. Triggers: "decompose into plans",
  "spawn sub-agents", "sub-agent instructions".

### Domain patterns

- **`agent-memory-mental-model.md`** — chains, thoughts, agents,
  scopes, retrieval (the 6 core concepts). Triggers: "what is a thought",
  "mentisdb mental model", "agent memory primitives".
- **`hybrid-retrieval-pipeline.md`** — how lexical + vector + graph +
  RRF combine; when to use which; how to tune. Triggers: "RAG",
  "hybrid search", "ranked search", "RRF".

## How skills get added

New skills land here as markdown files. The same content can later be
uploaded to MentisDB's skill registry (chain-backed, versioned,
signed). For now this folder is the canonical local source.

When updating a skill, append a new version rather than rewriting
(history is valuable). Each skill's `## Version history` section
tracks changes.
