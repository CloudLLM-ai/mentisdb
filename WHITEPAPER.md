# MentisDB White Paper

Note: the product is now named MentisDB. Some code artifacts in this repo may
still carry the legacy `thoughtchain` name during the transition.

**Author:** Angel Leon

## Abstract

Modern agent frameworks are still weak at long-term memory. In practice, memory is often reduced to ad hoc prompt stuffing, fragile `MEMORY.md` files, or proprietary session state that is hard to inspect, hard to transfer, and easy to lose or tamper with. MentisDB is a simple, durable alternative: an append-only, semantically typed memory ledger for agents and teams of agents.

MentisDB stores important thoughts, decisions, corrections, constraints, checkpoints, and handoffs as structured records in a hash-chained log. The chain model is storage-agnostic through a storage adapter layer, with binary storage as the current default backend and JSONL still supported. This makes memory replayable, queryable, portable, and auditable. It improves agent continuity across sessions, supports collaboration across specialized agents, and creates a clear foundation for future transparency, accountability, and regulatory compliance.

## Problem Statement

Today’s agent memory systems are messy.

- Long-term memory is often just another prompt.
- Durable memory is often a mutable text file.
- Context handoff between agents is brittle and lossy.
- Memory is rarely semantic enough for precise retrieval.
- Auditability and provenance are usually missing.

This creates operational and governance problems.

- Agents forget important constraints.
- Teams of agents repeat mistakes.
- Supervisors cannot easily inspect how a decision evolved.
- A malicious or faulty agent can rewrite or erase context.
- Future regulation will likely require stronger traceability than current frameworks provide.

## MentisDB

MentisDB is a lightweight memory primitive for agents.

Each memory record, or thought, is:

- append-only
- timestamped
- semantically typed
- attributable to an agent
- linkable to previous thoughts
- hashed into a chain for tamper detection

Rather than storing raw chain-of-thought, MentisDB stores durable cognitive checkpoints: facts learned, plans, insights, corrections, constraints, summaries, handoffs, and execution state.

## Core Design

MentisDB combines five ideas.

### 1. Semantic Memory

Thoughts are explicitly typed. This makes memory retrieval much more useful than searching free-form logs or transcripts.

Examples include:

- preferences
- user traits
- insights
- lessons learned
- facts learned
- hypotheses
- mistakes
- corrections
- constraints
- decisions
- plans
- questions
- ideas
- experiments
- checkpoints
- handoffs
- summaries

### 2. Hash-Chained Integrity

Thoughts are stored in an append-only hash chain, effectively a small blockchain for agent memory. Each record includes the previous hash and its own hash. This makes offline tampering detectable and gives the chain an auditable history.

This is not presented as a public cryptocurrency system. It is a practical blockchain-style ledger for memory integrity.

### 3. Shared Multi-Agent Memory

MentisDB supports multiple agents writing to the same chain. Each thought carries a stable:

- `agent_id`

Agent profile metadata such as display name, owner, aliases, descriptions, and public keys live in a per-chain agent registry rather than being duplicated inside every thought record.

This allows a single chain to represent the work of a team, a workflow, a tenant, or a project. Memory can then be searched not only by content and type, but also by who produced it, while keeping the durable thought records smaller and the identity model more consistent.

The agent registry is no longer just passive metadata inferred from old thoughts. It can now be administered directly through library calls, MCP tools, and REST endpoints. That means agents can be pre-registered, documented, disabled, aliased, or provisioned with public keys even before they start writing memories.

### 4. Query, Replay, and Export

The chain can be:

- discovered
- searched
- filtered
- traversed in append order
- inspected by stable id, index, or hash
- replayed
- summarized
- exported as `MEMORY.md`
- served over MCP
- served over REST

This makes MentisDB usable by agents, services, dashboards, CLIs, and orchestration systems.

In practice, that also means a daemon can tell a caller:

- which chain keys already exist
- which distinct agents are writing to a shared chain
- what the full registry metadata says about those agents
- which schema version each chain uses
- which storage adapter each chain uses

That makes shared brains easier to inspect and safer to reuse across teams of
agents.

Replay is now more explicit than a generic "read everything again" model.
Operators and agents can distinguish:

- `head`: the newest thought at the current chain tip
- `genesis`: the first thought in the append-only ledger
- direct lookup: resolve one thought by stable `id`, `index`, or `hash`
- ordered traversal: move `forward` or `backward` in chunks from an anchor
- graph/context traversal: follow `refs` and typed relations to connected thoughts

That distinction matters. Sequential chain traversal answers "what came before
or after this thought in append order?" Graph/context traversal answers "what
other thoughts are semantically or causally linked to this one?"

### 5. Swappable Storage

MentisDB now separates the chain model from the storage backend.

- A `StorageAdapter` interface handles persistence.
- A `BinaryStorageAdapter` provides the current default implementation.
- A `JsonlStorageAdapter` remains available as a line-oriented, inspectable format.
- Additional adapters can be added without changing the core memory model.

This keeps the system simple today while allowing more efficient storage engines in the future.

### 6. Versioned Schemas And Migration

MentisDB schemas are versioned.

- schema version `0` was the original format
- schema version `1` adds explicit versioning and optional signing metadata
- daemon startup can migrate discovered legacy chains before serving traffic
- startup can reconcile older active files into the configured default storage adapter
- startup can attempt repair when the expected active file is missing or invalid but another valid local source exists

This matters because append-only memory still evolves. A durable memory system needs a way to add fields, change attribution strategy, and improve integrity without abandoning existing chains.

The daemon also maintains a MentisDB registry so callers and operators can quickly inspect:

- what chains exist
- which schema version each chain uses
- which storage adapter each chain uses
- where each chain is stored
- how many thoughts and registered agents each chain currently has

### 7. Skill Registry

Beyond remembering what happened, agents benefit from remembering how to act. MentisDB ships a versioned skill registry as a first-class primitive, not an afterthought.

A skill is a structured document — authored in Markdown or JSON — that describes a reusable capability: how to use a tool, operate a protocol, or apply a domain pattern. Skills are uploaded by agents, assigned a stable `skill_id`, and versioned immutably. The registry exposes full lifecycle operations: upload, list, search, read, deprecate, and revoke — all available through the library API, the MCP surface, and the REST surface.

#### Delta Versioning

AI agents iteratively improve their skills over time. Storing a full copy of every skill version is wasteful at scale; the natural model is delta storage — recording only what changed between consecutive versions, much as version control systems do.

The first version of any skill is always stored in full. Every subsequent upload produces a unified diff patch via `diffy::create_patch` that captures only the changed lines. The content hash for each version is computed over the full reconstructed content, so integrity checks are independent of the storage representation. A caller verifying a version hash does not need to know whether the underlying record is a full snapshot or a patch.

To read an older version, MentisDB applies the patch chain sequentially from v0 forward. Every historical version is equally accessible; the cost is O(n) patch applications to reach version n.

This design makes a deliberate tradeoff: reconstruction cost grows linearly with version history depth. A practical future optimization is to introduce periodic full-content snapshots at configurable intervals — for example, every ten versions — so that reconstruction always starts from a nearby anchor rather than the original. The interface remains unchanged; the snapshot strategy is an internal storage concern.

The result is an audit-friendly record of how a skill evolved: nothing is silently rewritten, the full provenance of any version is recoverable, and the storage footprint for frequently revised skills stays proportional to the change surface rather than the total content.

## Data Model

MentisDB deliberately separates memory creation, memory storage, and memory retrieval.

### ThoughtInput

`ThoughtInput` is the caller-authored memory proposal.

It contains the semantic payload:

- the thought content
- the thought type
- the thought role
- tags and concepts
- confidence and importance
- references and semantic relations
- optional session metadata
- optional agent profile hints used to populate or update the registry
- optional signing metadata

It does not contain the final chain-managed fields such as index, timestamp, or hashes.

This is important because an agent should be able to say what memory it wants to record, but it should not directly forge the chain mechanics that make the ledger trustworthy.

### Thought

`Thought` is the committed durable record written into the chain.

MentisDB derives it from a `ThoughtInput` and adds the system-managed fields:

- `schema_version`
- `id`
- `index`
- `timestamp`
- `agent_id`
- optional `signing_key_id`
- optional `thought_signature`
- `prev_hash`
- `hash`

This prevents confusion between proposed memory content and accepted memory state.

Those same fields are also the stable anchors for retrieval and replay:

- `id` is the durable identity for direct lookup
- `index` is the total append-order position in the chain
- `hash` is the integrity fingerprint and an alternate lookup anchor

### ThoughtType And ThoughtRole

These two concepts are intentionally different.

- `ThoughtType` describes what the memory means
- `ThoughtRole` describes how the system is using that memory

For example:

- `Decision` is a thought type
- `Checkpoint` is usually a thought role
- `LessonLearned` is a thought type
- `Retrospective` is a thought role

That separation avoids mixing semantics with workflow mechanics.

This distinction is especially useful for reflective agent loops. A hard-won
fix might be stored as:

- `Mistake`
- `Correction`
- `LessonLearned`

with the final distilled guidance marked using the `Retrospective` role. That
lets future agents retrieve not just what happened, but what they should do
differently next time.

### ThoughtQuery

`ThoughtQuery` is the read-side filter over committed thoughts.

It does not create memories and it does not modify the chain. It simply retrieves relevant thoughts by type, role, agent identity, text, tags, concepts, importance, confidence, and time range.

`ThoughtQuery` is about filtering, not pagination. Ordered replay is a separate
operation: traversal walks the append-only ledger forward or backward, in
chunks, while optionally applying the same filters that a query uses.

## Use Cases

### Long-Term Agent Memory

A persistent agent can return days or weeks later and recover the important facts, preferences, constraints, and ongoing plans that matter for continuing work.

### Multi-Agent Handoff

One agent can shut down and hand work to another. A planning agent can hand off to an implementation agent. A coding agent can hand off to a debugging agent. A generalist can hand off to a specialist with different tools or cognitive strengths.

The receiving agent does not need the full conversation transcript. It can reconstruct the relevant state from the MentisDB.

### Team Coordination

When multiple agents collaborate, MentisDB provides a shared memory surface for:

- discoveries
- decisions
- mistakes
- lessons learned
- checkpoints
- handoff markers

This reduces repeated work and allows agents to build on each other’s progress.

### Human Oversight

Operators can inspect a chain directly, query it, traverse it in chunks, browse the agent registry, or export it as Markdown. This makes it easier to understand what happened and why.

The current daemon startup output also leans into operability. It prints a readable catalog of every HTTP endpoint it serves, followed by a summary of every registered chain and the known agents in each chain, including per-agent thought counts and descriptions. That is a small but important step toward a future ThoughtExplorer-style web interface.

## Transparency, Traceability, and Regulation

As agent systems become more powerful, regulation is likely to require stronger accountability. Governments and enterprises will increasingly ask:

- What did the agent know at the time?
- What constraints did it receive?
- Why was a decision made?
- What was learned after a failure?
- Who or what changed the memory state?

MentisDB is a strong primitive for answering those questions. It does not solve every governance problem, but it gives systems a durable and inspectable memory record instead of an opaque prompt history.

This is useful for:

- internal audits
- incident review
- compliance workflows
- model behavior analysis
- regulated industries that need traceability

## Anti-Tamper and Future Signing

The current hash chain makes memory rewrites detectable, but a sufficiently privileged malicious actor could still rewrite the full chain and recompute hashes.

For that reason, the thought format now includes optional signing hooks:

- `signing_key_id`
- `thought_signature`

Those fields allow a thought to carry a detached signature over the signable payload, while public verification keys can live in the agent registry.

This is still an early foundation rather than a full trust model. The current implementation does not yet require signatures or enforce a public-key policy on thoughts, but the schema is now shaped to support Ed25519-style agent identity and stronger provenance controls.

### Cryptographic Skill Authorship

The signing foundation described above extends naturally to the skill registry. In multi-agent systems, provenance matters: when an agent uploads a skill, that upload should be attributable not just by `agent_id` (a string) but by cryptographic proof.

The agent registry serves as the PKI anchor. Agents register Ed25519 public keys via `add_agent_key`. On skill upload, if the uploading agent has active registered keys, the upload request must include a detached signature over the raw skill content along with the `key_id` identifying which key was used. The server verifies the signature against the registered public key before accepting the upload. If verification fails, the upload is rejected.

The trust model is progressive:

- **No registered keys** — signature is not required. Legacy uploads and simple integrations continue to work without modification.
- **Active registered keys** — signature is mandatory. The agent has opted into signed provenance, and the system enforces it.
- **Revoked keys** — upload is rejected even if the signature is technically valid. Key revocation is authoritative.

The `key_id` and signature are stored durably on the `SkillVersion` record, enabling offline verification at any future point without querying a live server. An auditor with a copy of the agent's public key can verify the authorship of any signed skill version independently.

This means a signed skill upload is a statement of authorship and intent — not merely attributed by a string identifier, but bound to a cryptographic identity the agent controls. A signed version cannot later be disavowed, and a revoked key cannot be used to quietly replace a legitimate version.

Stronger controls could include signatures from a human-controlled or centrally controlled authority that agents themselves cannot control.

That authority could:

- sign checkpoints
- anchor chain heads externally
- validate approved memory states
- make unauthorized rewrites detectable even if an agent has local write access

This is an important future direction for environments where agents may attempt to cover their tracks.

## Why MentisDB Matters

MentisDB turns agent memory from an informal prompt trick into durable infrastructure.

It helps solve:

- long-term memory
- semantic retrieval
- context handoff
- multi-agent collaboration
- transparency
- traceability
- tamper detection
- durable, versioned skill provenance

In short, MentisDB is designed to be a practical memory ledger for real agent systems.

## Conclusion

Agent systems need a better memory foundation than mutable text files, prompt stuffing, and framework-specific hidden state. MentisDB provides a simple and durable alternative: semantic memory records stored in an append-only blockchain-style chain, queryable across time and across agents, with a storage layer that can evolve without rewriting the memory model.

It is useful today for persistent agents and multi-agent teams, and it points toward a future where agent systems can be both more capable and more accountable.

\
**Angel Leon**
