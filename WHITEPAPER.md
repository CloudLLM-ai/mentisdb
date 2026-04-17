# MentisDB: A Hash-Chained Semantic Memory Substrate for Agentic Systems

**Author:** Angel Leon
Universidad Católica Andrés Bello, Venezuela
**Version:** 0.8.9
**Date:** 2026-04-17

## Abstract

Contemporary agent frameworks treat long-term memory as an afterthought, relying on ad hoc prompt stuffing, unstructured Markdown files, or proprietary session state that is opaque, non-transferable, and easily lost. We introduce **MentisDB**, a durable, semantically typed memory engine that formalizes agent memory as an append-only, hash-chained ledger of structured *thoughts*.

Formally, a chain is a sequence $\chi = (t_0, t_1, \ldots, t_{n-1})$ of typed records satisfying a cryptographic integrity invariant $t_k.h = H(\sigma(t_k \setminus \{h\}))$ and $t_k.h_{\mathrm{prev}} = t_{k-1}.h$, where $H$ is SHA-256 and $\sigma$ is canonical bincode serialization. On top of $\chi$ we define a retrieval function $R: (\chi, Q) \to \mathcal{P}(\chi)$ that composes BM25 lexical scoring with per-field document-frequency gating, smooth exponential vector-lexical fusion, bidirectional graph expansion over typed relation edges, temporal edge validity predicates, session cohesion, and rank-based fusion via Reciprocal Rank Fusion (RRF). Deduplication is implemented as a Jaccard-similarity test over normalized token sets, emitting $\mathsf{Supersedes}$ edges that are consulted in constant time via a precomputed invalidation set.

On canonical long-term memory benchmarks, MentisDB attains $R@10 = 88.7\%$ on LoCoMo-2P, $R@10 = 71.9\%$ on LoCoMo-10P, and $R@5 = 66.8\%$ / $R@10 = 72.2\%$ / $R@20 = 78.0\%$ on LongMemEval (v0.8.9, fresh chain, default retrieval settings). Results are deterministic and reproducible across independent runs. The implementation ships as a single Rust crate with an optional daemon exposing MCP, REST, and HTTPS surfaces, requires no external database, and operates without cloud or LLM dependencies in its core ingestion and retrieval path.

**Keywords:** agent memory, hash-chained ledger, BM25, reciprocal rank fusion, graph expansion, temporal knowledge graphs, retrieval-augmented generation.

---

## 1. Introduction

The proliferation of large-language-model (LLM) agents has exposed a fundamental gap in the systems that support them: the absence of a durable, queryable, and tamper-evident memory substrate. Ephemeral context windows, hand-rolled Markdown files, and provider-specific key-value stores fail to provide the properties that multi-agent coordination demands — namely (i) *integrity* under adversarial or accidental mutation, (ii) *semantic typing* to distinguish decisions from observations from corrections, (iii) *temporal validity* to support point-in-time queries, (iv) *hybrid retrieval* combining lexical, semantic, and graph signals, and (v) *portability* across harnesses (Claude Code, Codex, Copilot, Cursor, Qwen, and beyond).

### 1.1 Contributions

This paper makes the following contributions:

1. **Formal model.** We define agent memory as an append-only, hash-chained ledger of *thoughts* — structured, typed, attributable records — with a precise integrity invariant and explicit schema evolution semantics (Sections 2, 3).
2. **Semantic typing.** We introduce a 30-variant $\mathsf{ThoughtType}$ algebra and an 8-variant $\mathsf{ThoughtRole}$ algebra, separating content semantics from workflow mechanics (Section 4).
3. **Temporal edges.** We extend typed graph relations with a validity interval $[\mathtt{valid\_at}, \mathtt{invalid\_at}]$ enabling point-in-time queries via a predicate $\pi_\tau$ (Section 4.4).
4. **Hybrid retrieval.** We describe a composable retrieval pipeline — per-field BM25 with DF gating, smooth exponential vector-lexical fusion, bounded graph BFS with typed edge weights, session cohesion, importance weighting, and RRF — and characterize each signal mathematically (Section 6).
5. **Deduplication.** We give a Jaccard-threshold algorithm that auto-emits $\mathsf{Supersedes}$ edges, with constant-time consultation via a precomputed invalidation set $\mathcal{I}(\chi)$ (Section 7).
6. **Empirical evaluation.** We report results on LoCoMo and LongMemEval, and provide a near-miss analysis characterizing the residual lexical ceiling (Section 9).

### 1.2 Paper Organization

Section 2 presents the core data model and integrity invariant. Section 3 formalizes schema evolution. Section 4 defines the semantic memory algebra. Section 5 describes the storage layer. Section 6 develops the retrieval pipeline. Section 7 gives the deduplication algorithm. Section 8 describes the operational surfaces (CLI, MCP). Section 9 reports empirical results. Section 10 compares against related systems. Section 11 concludes.

---

## 2. System Model and Core Data

### 2.1 Thought Record

**Definition 1 (Thought).** Let $\mathsf{UUID}$ denote the set of RFC 4122 universally unique identifiers, $\mathcal{T}$ the set of UTC timestamps, $\Sigma^\star$ the set of finite UTF-8 strings, and $\mathcal{H} = \{0,1\}^{256}$ the codomain of SHA-256. A *thought* is a tuple
$$ t = \bigl(v, \mathrm{id}, i, \tau, a, \kappa, \sigma, \varphi, \rho, c, \mathbf{T}, \mathbf{C}, f_{\mathrm{conf}}, f_{\mathrm{imp}}, s, \mathbf{R}, \mathbf{E}, h_{\mathrm{prev}}, h\bigr) $$
with components:

| Symbol | Domain | Role |
|---|---|---|
| $v$ | $\mathbb{N}$ | schema version (see §3) |
| $\mathrm{id}$ | $\mathsf{UUID}$ | stable identity |
| $i$ | $\mathbb{N}$ | append-order index |
| $\tau$ | $\mathcal{T}$ | commit timestamp |
| $a$ | $\Sigma^\star$ | agent identifier |
| $\kappa$ | $\Sigma^\star \cup \{\bot\}$ | signing key identifier |
| $\sigma$ | $\{0,1\}^\star \cup \{\bot\}$ | Ed25519 signature |
| $\varphi$ | $\mathsf{ThoughtType}$ | semantic class (§4.1) |
| $\rho$ | $\mathsf{ThoughtRole}$ | workflow role (§4.2) |
| $c$ | $\Sigma^\star$ | content |
| $\mathbf{T}, \mathbf{C}$ | $\mathcal{P}(\Sigma^\star)$ | tags, concepts |
| $f_{\mathrm{conf}}, f_{\mathrm{imp}}$ | $[0,1]$ | confidence, importance |
| $s$ | $\mathsf{Scope} \cup \{\bot\}$ | visibility scope |
| $\mathbf{R}$ | $\mathcal{P}(\mathbb{N})$ | positional back-references |
| $\mathbf{E}$ | $\mathcal{P}(\mathsf{Relation})$ | typed edges (§2.3) |
| $h_{\mathrm{prev}}, h$ | $\mathcal{H}$ | chain-integrity hashes |

A *thought input* $t^\circ$ is the caller-authored subset $(v, a, \varphi, \rho, c, \mathbf{T}, \mathbf{C}, f_{\mathrm{conf}}, f_{\mathrm{imp}}, s, \mathbf{R}, \mathbf{E}^\circ)$. The chain-managed fields $\{\mathrm{id}, i, \tau, h_{\mathrm{prev}}, h\}$ are assigned on commit; this asymmetry prevents agents from forging chain mechanics.

### 2.2 Hash-Chained Ledger

**Definition 2 (Chain).** Let $\sigma: \mathsf{Thought} \to \{0,1\}^\star$ denote canonical bincode serialization of the tuple $t \setminus \{h\}$. A *chain* is a sequence $\chi = (t_0, t_1, \ldots, t_{n-1})$ satisfying the *integrity invariant*
$$
\forall k \in \{0, \ldots, n-1\}: \quad t_k.h = H\bigl(\sigma(t_k)\bigr),
\qquad
\forall k \geq 1: \quad t_k.h_{\mathrm{prev}} = t_{k-1}.h,
$$
where $H$ is SHA-256. For $t_0$, $t_0.h_{\mathrm{prev}}$ is the empty string.

**Proposition 1 (Tamper Evidence).** Let $\chi' = (t'_0, \ldots, t'_{n-1})$ differ from $\chi$ at index $j$, i.e., $t'_j \neq t_j$, with all other components unchanged. Then either $t'_j.h \neq H(\sigma(t'_j))$ (local inconsistency detectable at index $j$) or $t'_{j+1}.h_{\mathrm{prev}} \neq t'_j.h$ (cascading inconsistency detectable at index $j+1$). Consequently, forging a modification requires recomputing every subsequent hash, touching $n - j$ records.

*Proof sketch.* By collision resistance of $H$, $t'_j \neq t_j \Rightarrow H(\sigma(t'_j)) \neq H(\sigma(t_j))$ with overwhelming probability. Either the adversary preserves $t'_j.h = H(\sigma(t'_j))$ (then $t'_j.h \neq t_j.h$, breaking $t'_{j+1}.h_{\mathrm{prev}}$), or leaves $t'_j.h = t_j.h$ (then $t'_j.h \neq H(\sigma(t'_j))$, local check fails). $\square$

This is a practical integrity mechanism for agent memory; it does not imply a consensus protocol or distributed-ledger guarantees. Optional Ed25519 signatures $(\kappa, \sigma)$ are layered on individual thoughts for stronger provenance when the producing agent has registered a public key in the agent registry.

### 2.3 Typed Relation Edges

**Definition 3 (Relation).** A relation is a tuple $e = (\kappa, \mathrm{id}^\ast, \chi^\ast, \mathtt{v}_\ast, \mathtt{v}^\ast)$ where $\kappa \in \mathsf{ThoughtRelationKind}$ (§4.3), $\mathrm{id}^\ast \in \mathsf{UUID}$ is the target thought identifier, $\chi^\ast \in \Sigma^\star \cup \{\bot\}$ is an optional cross-chain key, and $\mathtt{v}_\ast, \mathtt{v}^\ast \in \mathcal{T} \cup \{\bot\}$ bound the edge's validity interval.

The adjacency structure $A(\chi)$ induced by $\chi$ is a directed multigraph whose nodes are thought locators and whose edges derive from $\mathbf{R}$ (positional refs) and $\mathbf{E}$ (typed relations). We denote outgoing and incoming neighborhoods by $N^+(v)$ and $N^-(v)$ respectively; bidirectional expansion considers $N^+(v) \cup N^-(v)$.

### 2.4 Agent Registry

To avoid duplicating identity metadata inside every record, MentisDB maintains a per-chain registry $\mathcal{A}(\chi)$ mapping $a \mapsto (\mathtt{display\_name}, \mathtt{owner}, \mathtt{description}, \mathtt{aliases}, \mathtt{status}, \mathtt{public\_keys}, \mathtt{counters})$. Thoughts carry only the stable $a$; the registry is resolved at read time. The registry is itself administrable through library calls, MCP tools, and REST endpoints, permitting pre-registration, documentation, revocation, or key rotation prior to any appended thought.

---

## 3. Schema Evolution

### 3.1 Version Lattice

MentisDB exposes a linearly ordered schema version space $\mathcal{V} = \{V_0, V_1, V_2, V_3\}$:

| Version | Additions relative to predecessor |
|---|---|
| $V_0$ | original format; no explicit $v$ field |
| $V_1$ | explicit $v$, optional $(\kappa, \sigma)$, agent registry sidecar |
| $V_2$ | $\varphi := \varphi \cup \{\mathsf{Reframe}\}$, $\kappa := \kappa \cup \{\mathsf{Supersedes}\}$, optional cross-chain $\chi^\ast$ |
| $V_3$ | edge validity fields $\mathtt{valid\_at}, \mathtt{invalid\_at}$ |

The current constant is $V_\mathrm{cur} = V_3$.

### 3.2 Migration as Idempotent Transformation

Each migration $\mu_{V_k \to V_{k+1}}: \chi_{V_k} \to \chi_{V_{k+1}}$ satisfies
$$
\mu_{V_k \to V_{k+1}} \circ \mu_{V_k \to V_{k+1}} = \mu_{V_k \to V_{k+1}} \quad \text{(idempotence)}
$$
and is composable: $\chi_{V_0}$ is upgraded via $\mu_{V_2 \to V_3} \circ \mu_{V_1 \to V_2} \circ \mu_{V_0 \to V_1}$. Because bincode encodes enum variants by integer tag, new variants are appended to the end of an enum to preserve binary compatibility; mid-enum insertion would violate the injectivity of the serialization map. After each migration the hash chain is rebuilt under $V_\mathrm{cur}$ and persisted in native format, so subsequent opens incur no migration cost.

### 3.3 Version Detection

Version is inferred by peeking the first record's $v$ field. A subtle edge case arises for $V_0$ chains that lack the field entirely: the bincode "empty-Vec fast path" for $V_0$ reads $v = 0$, which is disambiguated from a non-empty $V_0$ chain by the residual byte-length check. Practically, this provides reliable version detection in $O(1)$ regardless of chain length.

---

## 4. Semantic Memory Algebra

### 4.1 ThoughtType

$\mathsf{ThoughtType}$ partitions the semantic space into 30 disjoint classes across seven categories:

| Category | Variants |
|---|---|
| User / relationship | $\mathsf{PreferenceUpdate}, \mathsf{UserTrait}, \mathsf{RelationshipUpdate}$ |
| Observation | $\mathsf{Finding}, \mathsf{Insight}, \mathsf{FactLearned}, \mathsf{PatternDetected}, \mathsf{Hypothesis}, \mathsf{Surprise}$ |
| Error / correction | $\mathsf{Mistake}, \mathsf{Correction}, \mathsf{LessonLearned}, \mathsf{AssumptionInvalidated}, \mathsf{Reframe}$ |
| Planning | $\mathsf{Constraint}, \mathsf{Plan}, \mathsf{Subgoal}, \mathsf{Goal}, \mathsf{Decision}, \mathsf{StrategyShift}$ |
| Exploration | $\mathsf{Wonder}, \mathsf{Question}, \mathsf{Idea}, \mathsf{Experiment}$ |
| Execution | $\mathsf{ActionTaken}, \mathsf{TaskComplete}$ |
| State | $\mathsf{Checkpoint}, \mathsf{StateSnapshot}, \mathsf{Handoff}, \mathsf{Summary}$ |

### 4.2 ThoughtRole

$\mathsf{ThoughtRole}$ is orthogonal to $\mathsf{ThoughtType}$, specifying *how* the system uses a memory:
$$
\mathsf{ThoughtRole} = \{\mathsf{Memory}, \mathsf{WorkingMemory}, \mathsf{Summary}, \mathsf{Compression}, \mathsf{Checkpoint}, \mathsf{Handoff}, \mathsf{Audit}, \mathsf{Retrospective}\}.
$$

The product $\mathsf{ThoughtType} \times \mathsf{ThoughtRole}$ yields 240 distinguishable semantic positions. A retrospective lesson is encoded as, e.g., $(\mathsf{LessonLearned}, \mathsf{Retrospective})$.

### 4.3 ThoughtRelationKind

The twelve-element relation algebra:
$$
\mathsf{ThoughtRelationKind} = \{\mathsf{References}, \mathsf{Summarizes}, \mathsf{Corrects}, \mathsf{Invalidates}, \mathsf{CausedBy},
$$
$$
\mathsf{Supports}, \mathsf{Contradicts}, \mathsf{DerivedFrom}, \mathsf{ContinuesFrom}, \mathsf{BranchesFrom}, \mathsf{RelatedTo}, \mathsf{Supersedes}\}.
$$

$\mathsf{Supersedes}$ is distinguished from $\mathsf{Corrects}$: the former replaces a prior framing without asserting an error, while the latter asserts a factual correction.

### 4.4 Temporal Edge Validity

**Definition 4 (As-Of Predicate).** For a relation $e$ with validity interval $[\mathtt{v}_\ast, \mathtt{v}^\ast]$ (treating $\bot$ on either bound as $-\infty$ and $+\infty$ respectively), define
$$
\pi_\tau(e) \equiv \bigl(\mathtt{v}_\ast = \bot \lor \mathtt{v}_\ast \le \tau\bigr) \land \bigl(\mathtt{v}^\ast = \bot \lor \tau < \mathtt{v}^\ast\bigr).
$$
Graph expansion restricted by $\tau$ considers only edges satisfying $\pi_\tau$. Combined with the append-ordering invariant ($i(t) < n$), this yields point-in-time retrieval semantics: "what did the agent know at $\tau$?"

### 4.5 Invalidation Set

**Definition 5 (Invalidation Set).** Given $\chi$,
$$
\mathcal{I}(\chi) = \bigl\{ e.\mathrm{id}^\ast : e \in \bigcup_{t \in \chi} \mathbf{E}(t),\; \kappa(e) \in \{\mathsf{Supersedes}, \mathsf{Corrects}, \mathsf{Invalidates}\} \bigr\}.
$$
$\mathcal{I}(\chi)$ is precomputed at chain open time as a $\mathsf{HashSet}\langle\mathsf{UUID}\rangle$, enabling $O(1)$ superseded-thought detection during retrieval.

---

## 5. Storage Layer

### 5.1 Storage Adapter Abstraction

The trait $\mathsf{StorageAdapter}$ abstracts persistence:
```
load_thoughts : ∅ → Vec<Thought>
append_thought : Thought → ()
flush : ∅ → ()
set_auto_flush : Bool → ()
```
This allows the chain semantics to remain invariant under backend substitution.

### 5.2 BinaryStorageAdapter

The default backend serializes each thought as a length-prefixed bincode record:
$$
\underbrace{\ell_0}_{\text{4-byte LE}}\underbrace{\sigma(t_0)}_{\ell_0\text{ bytes}} \| \underbrace{\ell_1}_{\text{4-byte LE}}\underbrace{\sigma(t_1)}_{\ell_1\text{ bytes}} \| \cdots
$$
with file extension `.tcbin`.

Two durability modes are supported:

- **Strict** ($\mathtt{auto\_flush} = \mathrm{true}$): appends are routed through a dedicated writer thread; callers block until the flush acknowledgment returns. A group-commit window of $\Delta_{\mathrm{gc}}$ (default 2 ms, configurable via `MENTISDB_GROUP_COMMIT_MS`) amortizes flush cost across concurrent writers.
- **Buffered** ($\mathtt{auto\_flush} = \mathrm{false}$): records are queued and batched; the writer flushes every $\Phi = 16$ records. Up to $\Phi - 1 = 15$ records may be lost on a hard crash, with a corresponding throughput gain for multi-agent hubs.

### 5.3 Legacy Adapters

`LegacyJsonlReadAdapter` is a read-only compatibility shim for migrating $V_0$ `.jsonl` chains; it cannot be used for new writes.

### 5.4 File Layout

```
~/.mentisdb/
  mentisdb-registry.json
  mentisdb-skills.bin
  <chain-key>.tcbin
  <chain-key>.agents.json
  <chain-key>.vectors.bin
  tls/{cert.pem, key.pem}
```

---

## 6. Retrieval

Retrieval separates a deterministic filter-first baseline from a scored ranked pipeline.

### 6.1 Baseline Filter

The baseline $R_\mathrm{base}(\chi, Q)$ narrows candidates by indexed fields $(\varphi, \rho, a, \mathbf{T}, \mathbf{C})$ and applies a case-insensitive substring predicate over $(c, \mathtt{agent\_meta}, \mathbf{T}, \mathbf{C})$. Results return in append order. This path is explainable and has no BM25, vector, or graph component.

### 6.2 Ranked Pipeline

Ranked search $R_\mathrm{rank}$ selects a backend based on query features:

| Query features | Backend |
|---|---|
| non-empty text, no vector sidecar | $\mathsf{Lexical}$ |
| non-empty text, vector sidecar | $\mathsf{Hybrid}$ |
| non-empty text, graph enabled, no vector | $\mathsf{LexicalGraph}$ |
| non-empty text, graph enabled, vector sidecar | $\mathsf{HybridGraph}$ |
| empty or absent text | $\mathsf{Heuristic}$ |

### 6.3 BM25 with Per-Field DF Gating

**Definition 6 (BM25 Field Score).** Let $N = |\chi|$ be corpus size, $\mathrm{df}(q)$ the document frequency of term $q$, and for field $f \in \{\mathrm{content}, \mathrm{tags}, \mathrm{concepts}, \mathrm{agent\_id}, \mathrm{agent\_registry}\}$ let $\mathrm{tf}_f(d, q)$ and $|d|_f$ denote term frequency and field length respectively, with $\overline{|d|_f}$ the corpus mean. With $k_1 = 1.2$, $b = 0.75$:
$$
\mathrm{idf}(q) = \ln\!\left(\frac{N - \mathrm{df}(q) + 0.5}{\mathrm{df}(q) + 0.5} + 1\right),
$$
$$
\mathrm{score}_f(d, q) = \mathrm{idf}(q) \cdot \frac{\mathrm{tf}_f(d, q)\,(k_1 + 1)}{\mathrm{tf}_f(d, q) + k_1\!\left(1 - b + b\,\dfrac{|d|_f}{\overline{|d|_f}}\right)}.
$$

**Definition 7 (DF Gate).** For per-field cutoff $\tau_f \in [0, 1]$:
$$
\gamma_f(q) = \mathbb{1}\!\left[\frac{\mathrm{df}(q)}{N} \le \tau_f \;\lor\; N < 20\right].
$$
Defaults: $\tau_{\mathrm{content}} = \tau_{\mathrm{tags}} = \tau_{\mathrm{concepts}} = 0.30$, $\tau_{\mathrm{agent\_registry}} = 0.60$, $\tau_{\mathrm{agent\_id}} = 0.70$.

**Definition 8 (Lexical Score).** With per-field weights $w_f$ (defaults $w_{\mathrm{content}} = 1.0$, $w_{\mathrm{tags}} = 1.6$, $w_{\mathrm{concepts}} = 1.4$, $w_{\mathrm{agent\_id}} = 1.5$, $w_{\mathrm{agent\_registry}} = 1.1$):
$$
S_\ell(d, Q) = \sum_{q \in Q} \sum_{f} \gamma_f(q) \cdot w_f \cdot \mathrm{score}_f(d, q).
$$

A term violating $\gamma_f$ in one field may still contribute through other fields whose cutoffs it respects. The 20-document threshold suppresses DF filtering on small corpora where statistics are not yet meaningful.

Normalization applies Porter stemming [Porter, 1980] before indexing and querying. An irregular-verb lemma table of approximately 170 entries expands query-time tokens (e.g., `went` $\to$ `go`, `saw` $\to$ `see`), because Porter stemming cannot normalize suppletive forms.

### 6.4 Smooth Vector-Lexical Fusion

When a managed vector sidecar provides cosine similarity $s_v(d, Q) \in [-1, 1]$ (e.g., via ONNX-embedded `fastembed-minilm`), the hybrid contribution is
$$
S_\mathrm{fuse}(d, Q) = s_v(d, Q) \cdot \Bigl(1 + \alpha \exp\!\bigl(-S_\ell(d, Q)/\beta\bigr)\Bigr),
$$
with $\alpha = 35$ and $\beta = 3$. This yields $\sim 36\times$ amplification for pure-semantic matches ($S_\ell = 0$), decays to $\sim 12\times$ at $S_\ell = 3$, and approaches additive composition for $S_\ell \ge 6$. The smooth exponential eliminates the discontinuities that step-function boost tiers introduce at bin boundaries.

### 6.5 Graph-Aware Expansion

**Definition 9 (Bounded BFS Expansion).** Given seed set $\Sigma_0 \subseteq \chi$ with $|\Sigma_0| \le 20$, adjacency index $A(\chi)$, and traversal mode $M \in \{\mathsf{Out}, \mathsf{In}, \mathsf{Bi}\}$, graph expansion is the BFS
$$
\mathrm{Expand}_{d_\max, V_\max, M}(\Sigma_0) = \bigl\{(v, d, \pi) : v \in \chi,\; d \le d_\max,\; \mathrm{path}(\pi) \subseteq \chi\bigr\}
$$
bounded by maximum depth $d_\max$ and visit budget $V_\max$, with edges optionally filtered by $\pi_\tau$ (Definition 4).

**Edge weights.** For traversals along a typed relation of kind $\kappa$, the edge contributes $b_\mathrm{rel}(\kappa)$:

| $\kappa$ | $b_\mathrm{rel}$ | $\kappa$ | $b_\mathrm{rel}$ |
|---|---|---|---|
| $\mathsf{ContinuesFrom}$ | 0.60 | $\mathsf{Summarizes}$ | 0.20 |
| $\mathsf{BranchesFrom}$ | 0.55 | $\mathsf{CausedBy}$ | 0.20 |
| $\mathsf{Corrects}, \mathsf{Invalidates}$ | 0.50 | $\mathsf{Supports}, \mathsf{Contradicts}$ | 0.15 |
| $\mathsf{Supersedes}$ | 0.45 | $\mathsf{RelatedTo}$ | 0.08 |
| $\mathsf{DerivedFrom}$ | 0.40 | $\mathsf{References}$ | 0.06 |

**Graph proximity.** For a hit at depth $d \ge 1$, $S_\mathrm{graph}(d) = 1/d$.

### 6.6 Session Cohesion

**Definition 10 (Session Cohesion Boost).** For a seed $\sigma \in \Sigma_0$ with lexical score $S_\ell(\sigma) \in [\theta_\mathrm{seed}, \theta_\mathrm{solo}) = [3, 5)$, and a candidate $d$ with $|i(d) - i(\sigma)| \le 8$,
$$
S_\mathrm{coh}(d) = \max_{\sigma \in \Sigma_0} \max\!\Bigl(0,\; 0.8 \cdot \bigl(1 - |i(d) - i(\sigma)|/8\bigr) \cdot \mathbb{1}[\theta_\mathrm{seed} \le S_\ell(\sigma) < \theta_\mathrm{solo}]\Bigr).
$$
Seeds above $\theta_\mathrm{solo}$ are excluded because they are strong enough to stand on their own; the cohesion boost is meant to surface *evidence turns* adjacent to a match that share no direct lexical terms.

### 6.7 Importance Weighting

**Definition 11 (Importance Boost).**
$$
S_\mathrm{imp}(d, Q) = S_\ell(d, Q) \cdot (f_\mathrm{imp}(d) - 0.5) \cdot 0.3.
$$
User-originated thoughts ($f_\mathrm{imp} \approx 0.8$) outrank verbose assistant responses ($f_\mathrm{imp} \approx 0.2$) in close BM25 races. The differential structure prevents flat multipliers from overwhelming lexical signal.

### 6.8 Reciprocal Rank Fusion

When $\mathtt{enable\_reranking}$ is set, the top $K = \mathtt{rerank\_k}$ candidates (default 50) are reranked via RRF [Cormack et al., 2009].

**Definition 12 (Reciprocal Rank Fusion).** Given $m$ ranked lists $L_1, \ldots, L_m$ of candidate documents, with $\mathrm{rank}_i(d)$ the 1-indexed position of $d$ in $L_i$ (or $+\infty$ if absent), and damping constant $k = 60$,
$$
S_\mathrm{RRF}(d) = \sum_{i=1}^{m} \frac{1}{k + \mathrm{rank}_i(d)}.
$$

MentisDB produces three single-signal rankings — lexical-only ($S_\ell$), vector-only ($s_v$), and graph-only ($S_\mathrm{graph} + b_\mathrm{rel} + S_\mathrm{seed}$) — and fuses them via $S_\mathrm{RRF}$. The RRF total replaces the additive blend. Non-rankable signals ($S_\mathrm{imp}, S_\mathrm{coh}, f_\mathrm{conf}$, recency) are then added as small tie-breaking adjustments. RRF is pure arithmetic: no LLM, no external service, no network round trip.

### 6.9 Memory Scopes

$\mathsf{Scope} = \{\mathsf{User}, \mathsf{Session}, \mathsf{Agent}\}$, stored as tag markers `scope:{variant}`. A query with scope $s$ filters hits such that $s(d) = s$; absence of a scope filter returns all scopes.

### 6.10 Context Bundles

$\mathrm{Bundle}(\chi, Q) = \{(\sigma, N^\pm(\sigma) \cap R_\mathrm{rank}(\chi, Q)) : \sigma \in \Sigma_0\}$: each bundle pairs a lexical seed with its graph-expanded neighbors, presented in deterministic provenance order so agents can interpret *why* supporting thoughts surfaced.

### 6.11 Vector Sidecars

Vector state lives in rebuildable per-chain sidecars, partitioned by $(\chi, \mathrm{id}, h, \mathrm{model\_id}, \dim, \mathrm{version})$. Model or version changes invalidate old sidecars rather than silently mixing incompatible embeddings. Managed sidecars remain synchronized on append; the daemon defaults to local ONNX inference via `fastembed-minilm`.

### 6.12 Decomposed Scores

Each ranked hit exposes a score vector
$$
\mathbf{s}(d) = (S_\ell, s_v, S_\mathrm{graph}, b_\mathrm{rel}, S_\mathrm{seed}, S_\mathrm{imp}, f_\mathrm{conf}, S_\mathrm{rec}, S_\mathrm{coh}, S_\mathrm{tot})
$$
together with $\mathrm{matched\_terms}$ and $\mathrm{match\_sources}$, preserving auditability.

---

## 7. Deduplication

### 7.1 Jaccard-Supersedes Algorithm

Let $\mathcal{N}(t) \subseteq \Sigma^\star$ denote the normalized token set of thought $t$ (Porter-stemmed, lemma-expanded). Given threshold $\theta \in [0, 1]$ and scan window $w \in \mathbb{N}$, on append of a new thought $t_n$ with token set $\mathcal{N}(t_n) \neq \emptyset$, MentisDB computes

$$
t^\ast = \arg\max_{t \in \{t_{n-w}, \ldots, t_{n-1}\}} J\bigl(\mathcal{N}(t_n), \mathcal{N}(t)\bigr),
\qquad
J(A, B) = \frac{|A \cap B|}{|A \cup B|}.
$$

If $J(\mathcal{N}(t_n), \mathcal{N}(t^\ast)) \ge \theta$, it auto-emits a relation $e = (\mathsf{Supersedes}, t^\ast.\mathrm{id}, \bot, \tau_\mathrm{now}, \bot)$ on $t_n$, and updates $\mathcal{I}(\chi)$ to include $t^\ast.\mathrm{id}$ for $O(1)$ skipping in subsequent retrieval.

**Complexity.** Token normalization is $O(|c|)$ per record. Jaccard over the window is $O(w \cdot \overline{|\mathcal{N}|})$. With default $w = 64$ and typical tokens per thought $\le 200$, dedup cost is bounded by a constant factor of append cost.

### 7.2 Configuration

```
MENTISDB_DEDUP_THRESHOLD = θ   ∈ [0,1]
MENTISDB_DEDUP_SCAN_WINDOW = w ∈ ℕ (default 64)
```

Library API: `MentisDb::with_dedup_threshold`, `with_dedup_scan_window`. The superseded thought is retained for audit; no content is deleted. Ranked search deprioritizes it via $\mathcal{I}(\chi)$.

---

## 8. Operational Surfaces

### 8.1 CLI

The daemon `mentisdbd` exposes three subcommands that RPC over REST to a running daemon at `http://127.0.0.1:9472`: `add`, `search`, `agents`. They use synchronous HTTP (`ureq`) to avoid pulling in an async runtime for the client path.

### 8.2 MCP Server

`mentisdbd` exposes a streamable HTTP MCP endpoint at `POST /` (port 9471) with 35 tools covering bootstrap, append, search, read, export/import, agent registry, chain management, and a skill registry. Legacy REST endpoints `POST /tools/list` and `POST /tools/execute` remain available for compatibility.

### 8.3 Skill Registry

The skill registry is a git-like immutable version store for agent instruction bundles. An upload to an existing $\mathtt{skill\_id}$ creates a new immutable version: the first is stored as full content, subsequent versions as unified diff patches. Version reconstruction replays patches from $v_0$ forward. Content hashes are computed over reconstructed content, decoupling integrity from storage representation. Agents with registered Ed25519 keys must cryptographically sign uploads; signature verification is server-side before acceptance.

### 8.4 Bootstrap Protocol

Modern MCP clients bootstrap from the handshake:

1. $\mathtt{initialize.instructions}$ directs the agent to read $\mathtt{mentisdb://skill/core}$.
2. $\mathtt{resources/read}$ returns the embedded operating skill.
3. $\mathtt{mentisdb\_bootstrap}$ opens or creates the chain; if empty, writes a genesis checkpoint.
4. $\mathtt{mentisdb\_recent\_context}$ loads prior state.

---

## 9. Empirical Evaluation

### 9.1 Benchmarks

We evaluate MentisDB on two standard long-term memory benchmarks.

| Benchmark | Metric | v0.8.1 | v0.8.5 | v0.8.9 |
|---|---|---|---|---|
| LoCoMo-2P | $R@10$ | **88.7%** | — | — |
| LoCoMo-2P single-hop | $R@10$ | 90.7% | — | — |
| LoCoMo-10P (1977 queries) | $R@10$ | 74.2% | **74.6%** | **71.9%** |
| LoCoMo-10P single-hop | $R@10$ | — | 79.0% | 75.8% |
| LoCoMo-10P multi-hop | $R@10$ | — | 58.4% | 57.4% |
| LoCoMo-10P | $R@20$ | — | — | 79.1% |
| LongMemEval (fresh chain) | $R@5$ | 67.6% | — | **66.8%** |
| LongMemEval (fresh chain) | $R@10$ | 73.2% | — | **72.2%** |
| LongMemEval (fresh chain) | $R@20$ | — | — | 78.0% |

All v0.8.9 numbers were reproduced deterministically across independent full-scale benchmark runs on 2026-04-14 and 2026-04-17 against fresh chains with the default retrieval configuration (`fastembed-minilm` vector sidecar, graph expansion enabled, RRF reranking disabled).

The v0.8.5 LoCoMo-10P improvement derives from three changes:

1. Session cohesion tuning: radius $8 \to 12$, boost $0.8 \to 1.2$.
2. Doubled edge weights $b_\mathrm{rel}$ across all $\mathsf{ThoughtRelationKind}$ variants.
3. FastEmbed MiniLM sentence embeddings replacing text-only hashing when the `local-embeddings` feature is compiled.

### 9.2 Scoring Evolution

| Version | Change | LongMemEval $R@5$ | LoCoMo-10P $R@10$ |
|---|---|---|---|
| 0.8.0 baseline | — | 57.2% | — |
| 0.8.0 + Porter stemming | token normalization | 61.6% | — |
| 0.8.0 + tiered fusion + importance | vector/lexical balance | 65.0% | — |
| 0.8.1 + cohesion + smooth fusion + DF cutoff | retrieval quality | 67.6% | 74.2% |
| 0.8.5 + cohesion tuning + $b_\mathrm{rel}\times 2$ + fastembed | session/graph boost | — | 74.6% |
| 0.8.9 + irregular lemmas + webhooks | lemma expansion + events | 66.8% | 71.9% |

### 9.3 Near-Miss Analysis (LoCoMo-10P, v0.8.5)

Of 503 misses (gold answer absent from top-10):

| Bucket | Count | Fraction | Interpretation |
|---|---|---|---|
| $R@20$ hit | 130 | 25.8% | close ranking error |
| $R@50$ hit | 285 | 56.7% | moderate signal gap |
| $R > 50$ | 218 | 43.3% | lexical gap (query terms absent from evidence) |

The 43.3% figure represents a hard ceiling for BM25-only retrieval on this benchmark. Closing it requires larger embedding models, LLM-driven query expansion, or external knowledge retrieval — mechanisms orthogonal to the scoring pipeline formalized here.

### 9.4 Micro-Benchmarks

Criterion micro-benchmarks span five domains: append throughput (`thought_chain`), baseline search (`search_baseline`), ranked retrieval (`search_ranked`), skill registry lifecycle (`skill_registry`), and HTTP concurrency at $\{100, 10^3, 10^4\}$ concurrent Tokio tasks with $p_{50}/p_{95}/p_{99}$ reporting (`http_concurrency`). A `DashMap`-based concurrent chain lookup delivers 750–930 read req/s at $10^4$ concurrent tasks, versus the previous $\mathsf{RwLock}\langle\mathsf{HashMap}\rangle$ bottleneck.

---

## 10. Related Work and Positioning

| Feature | **MentisDB** | Mem0 | Graphiti / Zep | Letta / MemGPT |
|---|---|---|---|---|
| Implementation language | Rust | Python | Python | Python / TS |
| Storage | embedded file | external DB | Neo4j / FalkorDB | external DB |
| LLM required for core | **No** | Yes | Yes | Yes |
| Cryptographic integrity | **SHA-256 hash chain** | — | — | — |
| Hybrid retrieval | BM25 + vector + graph | vector + keyword | semantic + keyword + graph | — |
| Temporal facts | $[\mathtt{v}_\ast, \mathtt{v}^\ast]$ (0.8.2+) | update-only | $[\mathtt{v}_\ast, \mathtt{v}^\ast]$ | — |
| Deduplication | **Jaccard + $\mathsf{Supersedes}$** | LLM-based | merge | — |
| Agent registry | Yes | — | — | Yes |
| MCP server | **Built-in** | — | Yes | — |

MentisDB is, to our knowledge, the only system combining (i) embedded storage, (ii) zero LLM dependency in the core path, (iii) cryptographic chain integrity, and (iv) hybrid BM25 + vector + graph retrieval in a single static binary. Identified gaps relative to competitors: custom per-chain entity/relation ontologies (as in Graphiti's Pydantic models), LLM-driven memory extraction, a browser extension, and per-thought token accounting.

---

## 11. Discussion, Limitations, and Future Work

### 11.1 Limitations

- **Ceiling of sparse retrieval.** The near-miss analysis quantifies an irreducible 43% lexical gap on LoCoMo-10P for BM25-only retrieval. Dense embeddings mitigate but do not eliminate this.
- **Local-only integrity model.** The hash chain provides tamper evidence, not Byzantine fault tolerance or distributed consensus; cross-chain consistency is not enforced cryptographically.
- **Schema churn discipline.** Because bincode tags enum variants by ordinal, schema evolution is append-only at the enum level — reordering or renaming variants would silently corrupt persisted data.

### 11.2 Future Work

- **Per-chain entity / relation ontologies** enabling typed domain-specific facts beyond the fixed $\mathsf{ThoughtRelationKind}$.
- **Episode provenance**: tracing derived facts back to source conversations.
- **Cross-chain federated retrieval** with result reconciliation across distributed ledgers.
- **Optional LLM-extracted memories** as a layered, auditable transform.
- **Self-improving skill registry**: agents committing updated skill versions as they learn, with signed provenance.

### 11.3 Conclusion

MentisDB formalizes agent memory as an append-only, hash-chained ledger of semantically typed thoughts, and couples that ledger with a composable retrieval pipeline — BM25 with per-field DF gating, smooth vector-lexical fusion, bounded typed-edge graph expansion, RRF, session cohesion, and Jaccard-based deduplication — in a single embedded Rust substrate. Empirical results on LoCoMo and LongMemEval demonstrate competitive retrieval quality without reliance on external databases or LLM services for the core ingestion path. The system is released as open source and exposes MCP, REST, and HTTPS surfaces for interoperation with contemporary agentic harnesses.

---

## References

- Robertson, S. and Zaragoza, H. *The Probabilistic Relevance Framework: BM25 and Beyond*. Foundations and Trends in Information Retrieval, 3(4), 2009.
- Cormack, G. V., Clarke, C. L. A., and Büttcher, S. *Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods*. SIGIR, 2009.
- Porter, M. F. *An algorithm for suffix stripping*. Program, 14(3), 1980.
- Jaccard, P. *Étude comparative de la distribution florale dans une portion des Alpes et des Jura*. Bulletin de la Société Vaudoise des Sciences Naturelles, 1901.
- Bernstein, D. J., Duif, N., Lange, T., Schwabe, P., and Yang, B.-Y. *High-speed high-security signatures*. Journal of Cryptographic Engineering, 2012.
- FIPS PUB 180-4. *Secure Hash Standard (SHS)*. NIST, 2015.
- Maharana, A. et al. *Evaluating Very Long-Term Conversational Memory of LLM Agents (LoCoMo)*. 2024.
- Wu, D. et al. *LongMemEval: Benchmarking Chat Assistants on Long-Term Interactive Memory*. 2024.
- Anthropic. *Model Context Protocol (MCP) Specification*. 2024.

---

**Angel Leon**
