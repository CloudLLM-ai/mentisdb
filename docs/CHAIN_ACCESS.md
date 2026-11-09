# Public and private chains

Status: implemented (see the changelog `UNRELEASED` block).

A MentisDB chain is either **public** or **private**.

* **Public**: no keys are needed. Anyone who can reach the server may write
  unsigned and anyone may read.
* **Private**: writes need a valid Ed25519 signature by an authorized key, and
  the thoughts written while the chain is private can be read only by a reader
  who proves possession of an authorized key.

The mode is not a flag. It is the history of the chain's *governance records*
(access-policy thoughts, below), so every change of mode is a tamper-evident
entry in the chain itself that opens a new **segment**. Each thought belongs to
exactly one segment, decided by its position:

```text
index:    0 ........ 9 | 10 ......... 49 50 | 51 ...........
entry:    (none)       | seq 0 private  seq 1 public
segment:  public #0    | private #1 ........ | public #2
```

* An **authorized reader** (a key in the chain's current policy, proved per
  request) and the **operator** (dashboard behind its PIN, or the local
  command line) see everything.
* **Everyone else** sees only thoughts of public segments, on *every* read
  surface: ranked, lexical and vector search, recent and standing context,
  traversal, get by id, hash or index, head, genesis, context bundles,
  summaries, federated search, branch-ancestor search, Markdown export, chain
  and agent listings, counts, policy and verification reports.

Who may *change* the allowed keys, the registry of agent keys or an agent's
status is **only the operator**. REST and MCP, with any bearer token, global or
chain-scoped, cannot.

Existing chains need no migration: a chain with no governance record is public
from its first thought; a chain whose policy was already `signed` or
`allowlist` is private from the position of that record. The segment list is
recomputed from the chain whenever it opens, so nothing new is stored.

## Threat model

Assets: the content of private segments (confidentiality), the integrity of
the chain and of its authorship claims, and the set of keys allowed to write
and read.

Actors: an anonymous network client; a holder of a stolen bearer token
(global or chain-scoped); an agent that has been prompt-injected by data it
read; a hostile web page driving a local browser; a registered agent whose key
is not allowed on the chain; a former member whose key was removed; someone
with read access to a backup; someone with write access to the data directory.

| STRIDE | What is protected | How |
|---|---|---|
| Spoofing | Authorship; reader identity | Ed25519 signatures bound to chain key and head (thought v2); a read proof signs each request; registering keys is operator-only so nobody can add a key and sign as an agent |
| Tampering | Chain, policy, key set | Hash chain; governance records are signed, linked (`prev_policy`) and committed (`keys_root`); an invalid record is ignored and reported |
| Repudiation | Who changed access | Every change is a signed record in the chain; the operator signs with a key kept on the host |
| Information disclosure | Private segments | Audience filter at the library (queries, ranking statistics, graph, lineage, advice, export) and at every server route; see *What a public reader can still learn* |
| Denial of service | Read proofs, nonce cache, bodies | `MENTISDB_MAX_BODY_BYTES` cap (64 MiB default; 1 MiB for a proof without a content digest), replay cache written only for authorized keys, bounded per key and in total, failing closed to the public view; constant-work header grammar |
| Elevation of privilege | Key administration | Operator-only; REST/MCP routes answer 403; the operator key is always a governor so a policy can never lock itself out |

Out of scope, documented limits:

* **Transport.** A read proof authenticates the reader; it does not encrypt
  the response. Serve private chains over HTTPS. A proof is bound to method,
  path, query and body, not to a host name, so do not terminate TLS in front of
  a rewriting proxy.
* **Filesystem access.** Whoever can read the data directory can read every
  thought, the operator key included. Confidentiality is against API callers,
  not against the host. The data directory is created `0700` and the chain,
  registry, sidecar and backup files `0600`, so other local users cannot read
  them; an existing directory is never changed (the daemon warns when one is
  group- or world-accessible). `mentisdb backup` excludes
  `operator/operator.key` on purpose: back it up separately.
* **Private-key custody** is the agent's. `mentisdb key generate` never prints
  the seed: it saves it to an owner-only (`0600`) file (`--out FILE`, else
  `$MENTISDB_KEY_DIR`, default `~/.mentisdb/keys/<public key prefix>.key`) and
  prints only the public key and the path, so a key is never copied into
  terminal scrollback, a shell log or the context of an AI agent that ran the
  command. Seeds come straight from the operating system's random generator.
  `mentisdb agent key generate` does the same and registers the public key in one
  step; the dashboard's "Generate key pair" makes the pair in the browser
  (WebCrypto), registers only the public key with a proof of possession and shows
  the seed once, so the server never holds it.
  Loading a seed file that group or other users can read (`--private-key-file`,
  `--read-key-file`, `MENTISDB_READ_KEY_FILE`, the operator key) works but
  prints a warning with the `chmod 600` fix; MentisDB never changes the mode of
  a file it did not create. Do not hand a seed file to an LLM or paste it into a
  chat.
* **Clock.** Proofs are accepted within 60 seconds of the server clock.
* An attacker with write access to the chain *files* can append well-formed
  records; the hash chain detects edits to existing records, and policy
  records must still verify under the rules below.

## Read authentication

A reader proves possession of an authorized key on each request. The private
key never travels over the wire and never has to enter a language model's
context: the process that talks to the server (the `mentisdb` CLI, the stdio
MCP proxy, an SDK) holds the key file and signs.

```text
X-Mentis-Auth: v1 agent=alice;key=alice-202610;pk=<64 hex>;ts=1767225600;nonce=<32 hex>;sig=<128 hex>
```

`agent` and `key` are percent-encoded (`%XX` uppercase for anything outside
`A-Za-z0-9._~-`). The header is parsed strictly: exactly those six fields, in
that order, lowercase hex, at most 1024 bytes, and it is accepted only if
re-rendering it reproduces it byte for byte. The signature (strict Ed25519)
covers:

```
"mentisdb/read-auth/v1\0"
str METHOD   str path_and_query   bytes[32] SHA-256(body)
str agent_id   str key_id   bytes[32] public_key   i64 ts   str nonce
```

`METHOD` is upper-case; `path_and_query` is exactly what the request line
carries; the body is the complete request body (empty for `GET`). The chain
key travels in the body, so it is covered. A proof therefore authorizes one
method, one URL and one body, once.

The server accepts a proof when

1. the header parses and the signature verifies;
2. `ts` is within 60 seconds of its clock;
3. its `(public key, nonce)` pair has not been spent in the last 120 seconds
   (a read-only lookup);

and then asks **per chain** whether `(agent_id, key_id, public key)` is in that
chain's current key set *and* is an active key (agent not disabled, key not
revoked) of the chain's registry. Only then is the request an *authorized
reader* of that chain. A valid proof by a key that is not allowed gains
nothing, and a request without the header is a public reader. A header that
is present but fails steps 1 to 3 is refused with 401 so a client with a
stale clock learns why.

### Large bodies: `X-Mentis-Content-Sha256`

Because the signature covers `SHA-256(body)`, a server holding only the proof must read the whole body
to check it, which it does only up to 1 MiB. A client may add an optional second header,

```text
X-Mentis-Content-Sha256: <64 lowercase hex>
```

carrying the digest the proof signed. The signed bytes are unchanged, so every existing client (including
`mentischat` and the interop vectors) verifies as before. A server that knows the header verifies the
signature against the declared digest *without reading the body*, then hashes the body as it streams and
fails the request at its end (`400`) if the bytes are not the signed ones; the body is limited only by the
route's cap (2 MiB, or `MENTISDB_MAX_BODY_BYTES`, default 64 MiB, on the import routes) and the in-flight body
budget (`MENTISDB_MAX_INFLIGHT_BODY_BYTES`). A proof is checked per chain and the chain is named inside the body,
so a proof from an unregistered key cannot be told apart from a real one before the body has been read; that is
why proof requests are bounded by the budgets rather than a smaller cap. The header is strict (exactly 64 lowercase hex characters, given
once; anything else is `400`) and a digest that is not the signed body's fails the signature (`401`).

The digest is confirmed only at the end of the body. MentisDB's handlers collect the whole body before they
run, so a mismatching body never reaches one and nothing is committed. Anything that streams a body must buffer
or spool it and commit after the end.

A proof is verified against the public key written in its own header, so
*anyone* can make a valid proof with a fresh keypair. For that reason the
nonce is not recorded by step 3. It is *spent* later, only when the proof is
about to grant access to a private segment: the first time, per request, that a
chain with a private segment finds the key authorized. A key that no chain authorizes, and a request that touches
no private chain, never write to the cache. The cache is bounded (100 000 live
entries, at most 25 000 per key); when it is full, or a concurrent request used
the same proof first, the request is not failed: it reads the **public view**,
as if it carried no proof. Expired entries are dropped as time advances, so
refusals cost constant work.

Clients:

* `mentisdb ... --read-key-file FILE --read-agent ID --read-key-id ID` (or
  `MENTISDB_READ_KEY_FILE`, `MENTISDB_READ_AGENT_ID`, `MENTISDB_READ_KEY_ID`)
  signs every REST request. Commands that sign thoughts use their own signing
  key for the head lookup.
* `mentisdb --mode stdio` reads the same environment variables once and signs
  every request it forwards to the daemon.
* The CLI and the stdio proxy **never follow redirects**: the request carries
  the proof (and the bearer token or dashboard PIN), and following a redirect
  would hand them to another host. A `3xx` from `--url` is an error naming the
  target; point `--url` at the final address. (The proof header is bound to
  method, path, query and body, not to a host, see *Out of scope* above.)
* Library: `mentisdb::read_auth::{sign_request, ReadSigner}`.
* Python: `pymentisdb.ReadSigner` (`pip install pymentisdb[signing]`) or the same environment variables; `MentisDbClient(read_signer=...)` signs every request. A pinned test vector is shared with the Rust tests.

Range of the proof: key sets are per chain, so one signature never reads a
chain the key is not allowed on, even in a federated search; each chain is
checked on its own.

## What each surface shows a public reader

| Surface | Public view |
|---|---|
| `search`, `lexical-search`, `ranked-search`, `federated-search`, `context-bundles`, `summary-candidates` | Candidates are filtered **before** ranking. `total` and counts are of visible thoughts. BM25 corpus statistics (document count, document frequency, length averages) and the recency anchor are computed over the visible set, so scores do not move when private thoughts are added. Graph expansion never enters a hidden thought: paths cannot pass *through* one. |
| `recent-context`, `standing-context`, `memory-markdown` | Visible thoughts only. |
| `thought`, `thoughts/genesis`, `thoughts/traverse`, `head` | A hidden thought is reported exactly like one that does not exist. Genesis and head mean the first and last *public* thought. A hidden traversal anchor yields an empty page. `head` counts only public thoughts. |
| Superseding, contesting, successor redirection, `current_version`, `version_status`, append advice | Computed from visible thoughts only. A public thought that only a private thought supersedes still reads as current. Advice returned to a writer who is not an authorized reader never quotes private thoughts. |
| `refs`, `relations` on every returned thought | Pointers into hidden segments, and relations that point into other chains, are removed. |
| `chains`, `agents`, `agent`, `agent-registry` | Counts and activity (`thought_count`, first/last seen, timestamps) are recomputed over visible thoughts; an agent that only wrote privately is not listed; storage paths are blank for a chain with private segments. |
| `access/policy`, `access/verify` | Mode, `seq`, `keys_root` and segment boundaries. The key list, record hashes and the list of rejected records are withheld, and the verification report lists only visible thoughts. |
| `chains/branch` from a private thought | Not found, as for an unknown id. |
| Errors | A refused write reveals only that the policy did not accept it; it does not say whether an agent or key exists, was revoked or is not allowed. |
| MCP resources | The only resource is the static skill text. |
| Webhooks | A thought of a private segment, and every governance record, is never pushed to a webhook: registering one needs no proof of read access, so the event would hand private text to whoever registered. Public thoughts are delivered as before. |
| SSE event stream (`GET /`) | One `tool_called` event per tool call (tool name, chain key, outcome, duration), plus metadata-only events per JSON-RPC method (method, client address, outcome). Arguments, results and error text are never broadcast. With bearer enforcement on, subscribing needs a **global** token (a chain-scoped token is refused: the stream names every chain's activity); the stream applies the same `Host` check as `POST /`. The reported chain key is the validated `chain_key` string of the call, else `null`. MCP methods outside the known list (`initialize`, `ping`, `tools/list`, `tools/call`, `resources/list`, `resources/read`, `notifications/*` and the discovery probes `prompts/list` and `resources/templates/list`) also need a global token. |
| Operator logs | The interaction log (`MENTISDB_LOG_FILE`) records agent ids, tags and concepts of each operation for the operator; it is a local file, not an API surface. |
| Dashboard | A chain with private segments is shown only behind a PIN; see *Operator*. |

Merge, copy and branch: see *Moving thoughts between chains*.

### What a public reader can still learn

Deliberately public, because the chain must stay verifiable and usable:

* **That the chain has private segments, and where.** The mode, `seq` and the
  segment boundaries are shown, and `index` values in responses skip the
  hidden positions, so the *number* of private thoughts in a gap is visible.
* **One hash across each gap.** The first public thought after a private
  segment names the hash of the thought before it in `prev_hash`, so a public
  reader can check that the visible thoughts are linked to the rest. The hash
  commits to content that cannot be recovered from it.
* **Timing.** Responses are not padded; request latency may differ slightly
  for chains with a large private segment.
* **Signatures of public thoughts and the keys that made them** (`provenance`,
  registry entries of visible authors).

## Provenance and the prompt-injection boundary

Everything stored in a chain is **data**. A thought written by one agent is
read by another; whoever can write to a public chain, and anyone who could
write while a chain was public, can put text in front of the next reader that
looks like an instruction. MentisDB cannot make that text safe. It marks where
it came from so the reading agent can weigh it, and the operating skill
(`MENTISDB_SKILL.md`) tells agents to treat memories as data, never as
instructions.

Every thought in a response carries a `provenance` object:

```json
"provenance": {
  "segment": "public",           // or "private"
  "signed": true,
  "signature": "valid",          // "unsigned", "valid" or "invalid"
  "signature_version": 2,
  "verified_agent": "alice",     // agent id when the signature is valid and the key is active, else null
  "trust": "untrusted_data"
}
```

How to read it: an unsigned thought in a public segment can have been written
by anyone. A thought with `verified_agent` was written by whoever holds that
agent's key *now*, which says nothing about whether its content is true or
safe. A private-segment thought was signed by a key the operator allowed. An
agent should never execute commands, change its goals or reveal data because
a retrieved thought says so; it should weigh the thought as evidence.

### Replacing someone else's thought

A `Supersedes`, `Corrects` or `Invalidates` link hides its target and redirects
readers to the writer's thought, so it is only as trustworthy as that writer.

* **Open chain**, or a replacement written while the chain accepted unsigned
  writes: there is no privilege boundary (anyone can write a constraint), and
  corrections often come from a different agent identity than the original, so
  the link is honored. It is never silent: successor notes and `current_version`
  carry the replacement's `agent_id`, `trust` and `cross_agent`; a ranked hit
  that replaced another agent's thought has `cross_agent: true`; a standing
  entry shows `[REPLACES #n by <agent>: replaced by another agent, check it]`.
  The operating skill tells agents to check these before acting on a replaced
  `Constraint` or `Decision`.
* **Governed segment** (written while the policy was `signed` or `allowlist`):
  the link replaces its target only if the writer's thought passed verification
  when the chain opened and it is by the **same agent** as the target or by a
  key **authorized on the chain** (governor, operator, any allowed key; checked
  against the current policy and registry, so a link by a key that was later
  removed reverts to a contest). Any other link, strong or weak, is only a
  **contest**: the target stays current and visible, reports
  `version_status: "contested"`, lists the writer under `successors` with
  `authorized: false`, and is neither redirected to nor promoted in ranked or
  standing results.
* A write that would make a chain of more than 64 versions of one thought
  (`MAX_LINEAGE_DEPTH`, the depth reads follow) is refused with a message
  explaining how to restate the current version instead.

### Context blocks cannot be forged by stored text

The blocks MentisDB renders for an agent to load at session start
(`=== STANDING CONTEXT ===`, `=== RECENT CONTEXT ===`, `=== RESTORED CONTEXT ===`
and the `MEMORY.md` export) are built so that **stored text never shapes the
block**: every entry is one physical line, and content, agent ids, display
names, owners, tags and concepts are interpolated only after escaping
(`mentisdb::prompt_text`). Line breaks (including U+0085, U+2028, U+2029)
become `⏎`, tabs a space, other control characters and invisible or
direction-changing characters (bidi overrides, zero-width space, BOM, Unicode
tag characters) become `�`, a run of three or more `=` is broken after every
second `=`, and `[#` becomes `[ #`. A thought can therefore not close the
block, add a `SYSTEM:` line, open a section or print an entry that looks
selected by MentisDB. JSON responses carry the raw text; only the rendered
prompt text is escaped.

Each standing-context entry also states how its author is established:
`trust=verified` (signed by a registered, active key of the agent),
`trust=unsigned` (the agent id is only a claim) or `trust=unverified` (a
signature that does not check out). `StandingEntry.trust` carries the same
value.

Ranking uses the writer-supplied `importance`. On a chain whose policy
requires signatures (`signed` or `allowlist`), a thought that is not
`verified` ranks with at most `UNVERIFIED_IMPORTANCE_CAP` (0.5), so history
written before the policy, or a forged signature, cannot claim `1.0` to sit
above verified entries; re-record such a thought with a signature to restore
its rank. On an **open** chain there is no privilege boundary (anyone who can
append can already write a constraint), so the importance is used as written
and `trust` / `agent_id` are what a reader weighs.

## Operator

The operator is whoever can reach one of two places:

* the **dashboard behind its PIN** (`MENTISDB_DASHBOARD_PIN`); without a PIN the
  dashboard cannot tell the operator from any other local process, so it
  neither administers access nor shows chains that have private segments;
* the **local `mentisdb` command line**, which can run any operator command with
  `--offline` against the data directory (filesystem access is the boundary:
  whoever has it already holds every byte). `--offline` refuses to run while a
  daemon holds the data directory, because two processes must not append to
  one chain.

Operator commands (dashboard route `POST /dashboard/api/chains/{chain}/access`,
CLI below, library `mentisdb::operator::apply`):

| Command | Effect |
|---|---|
| `chain access set public\|private\|signed` | Switch mode. Going private on a chain with no policy bootstraps governance and opens a private segment; going public closes it and opens a public one (earlier private thoughts stay private). |
| `chain access allow AGENT KEY_ID PUBKEY` | Register the key if needed and add it to the allowed set. |
| `chain access deny AGENT KEY_ID` | Remove it. The operator key and the last governor cannot be removed. |
| `agent key add / revoke`, `agent disable / activate` | Registry administration. Registering a key needs no proof of possession: the operator vouches for it. |
| `chain access admin` | The operator's view: keys, segments, the operator key id. |

The operator signs governance records with an Ed25519 key kept in
`<data dir>/operator/operator.key` (directory `0700`, file `0600`, created on
first use, never overwritten). That key is a governor of every policy the
operator writes. It is the **recovery path**: if every agent governor key is
lost, the operator can still change the policy. A record that would leave no
governor is refused, so a policy cannot lock itself out. Back the key file up
**separately from `mentisdb backup`** (archives intentionally exclude it, so a
stolen archive cannot also change who may read or write; `backup` and
`restore` print a reminder); without it, and without a governor key, the key
list of a private chain can no longer be changed.

What REST and MCP do instead: `POST /v1/agents/keys`, `/v1/agents/keys/revoke`,
`/v1/agents/disable`, `/v1/access/policy/set` and status changes through
`/v1/agents/upsert` answer **403** (`operator-only`) with any token; the
matching MCP tools are not offered. Editing an agent's name, description or
aliases on a private chain needs a read proof, because the registry is what
labels authors in every response.

Chains governed before the operator model keep working: governor-signed
records (`MentisDb::set_access_policy`) are still valid, and a governor can add
the operator key once so the chain becomes operator-managed.

## Key encoding and identity (shared with mentischat)

* **Public key:** the raw 32-byte Ed25519 public key. Textual form in the CLI,
  policy records and new REST fields is **lowercase hex, 64 characters**. The
  existing REST/MCP `public_key_bytes` array-of-integers form stays accepted.
* **Signature:** the raw 64-byte Ed25519 signature (RFC 8032, pure Ed25519, no
  prehash, no context). Textual form is lowercase hex, 128 characters.
* **Private key (CLI key files):** the 32-byte seed as 64 lowercase hex
  characters plus a newline.
* **Key id:** a free-form, case-sensitive label, trimmed, non-empty, unique per
  agent. mentischat uses the same id for the same key. Convention:
  `<agent-id>-<yyyymm>` or the first 8 hex characters of the public key.
* **Validity of a public key:** exactly 32 bytes, decompresses to a curve
  point, and is not a small-order (weak) point. Anything else is rejected at
  registration.
* An agent identity is the pair `(agent_id, key_id)` plus the public key bytes.

## Canonical payloads and domain separation

All preimages start with an ASCII domain tag ending in a NUL byte, so a
signature for one payload type can never be valid for another (nor for the
other project, which uses `mentischat/...` tags).

Primitive encodings (all integers big-endian):

| Name | Encoding |
|---|---|
| `str` | `u32 length` then the UTF-8 bytes |
| `opt<T>` | `0x00` for none, `0x01` then `T` |
| `list<T>` | `u32 count` then each `T` |
| `u8`, `u64` | fixed width |
| `f32` | the 4 bytes of the IEEE-754 bits of the value clamped to `0.0..=1.0` |
| `uuid` | the 16 raw bytes |
| `time` | `i64` seconds since the Unix epoch, UTC, then `u32` nanoseconds of the second |

### `mentisdb/key-registration/v1` (proof of possession)

```
"mentisdb/key-registration/v1\0"
str chain_key  str agent_id  str key_id  bytes[32] public_key  i64 registered_at_unix_seconds
```

The signature is made with the key being registered. The server accepts a proof
whose timestamp is within 600 seconds of its clock, which bounds replay of an
old registration. Registering the same key again is idempotent, so replay is
harmless inside that window.

### `mentisdb/thought/v2`

```
"mentisdb/thought/v2\0"
str chain_key
str agent_id
str prev_hash                 (hash of the chain head the thought is appended after; "" for the first thought)
opt<uuid> session_id
str thought_type              (canonical name, e.g. "Decision")
str role                      (canonical name, e.g. "WorkingMemory")
str content
opt<f32> confidence
f32 importance
list<str> tags                (as stored: trimmed, de-duplicated; this includes the "scope:<x>" tag, so scope is covered)
list<str> concepts
list<u64> refs
list<relation> relations
opt<str> entity_type
opt<str> source_episode
```

`relation` = `str kind  uuid target_id  opt<str> chain_key  opt<time> valid_at  opt<time> invalid_at`.

Binding `chain_key` and `prev_hash` means a signed thought is valid for exactly
one position of one chain: moving it to another chain changes `chain_key`;
moving it to another position changes `prev_hash`; resubmitting it to the same
chain fails because the head has moved on. The cost is that a signer must sign
against the current head, so a concurrent writer can make the append fail with
`stale head`; the client fetches the new head and signs again.

The server never edits a signed thought: for a thought that carries a signature
it adds no automatic `References` edges for `refs`, fills in no `valid_at`,
removes no duplicate relations and adds no near-duplicate `Supersedes` edge, so
what is stored is exactly what was signed and can be re-verified later. `id` and `timestamp` are assigned by the chain and are not signed;
they are covered by the record hash instead.

### Signature versions on stored thoughts

The `Thought` record layout is not changed (it is positional bincode and part of
the hash). The version is recorded in `thought_signature` itself:

| `thought_signature` | Meaning |
|---|---|
| 64 bytes | **v1**: signature over the legacy `signable_thought_payload` JSON. Still readable and verifiable; replayable across chains, so never accepted on a `Signed` or `Allowlist` chain. |
| 65 bytes, first byte `0x02` | **v2**: `0x02` followed by the 64-byte signature over the v2 payload. |
| any other length or leading byte | invalid, rejected. |

## Verification on append (library)

`MentisDb::append_thought` enforces, in this order:

1. The reserved `AccessPolicy` thought type is refused on the normal append
   path; governance records have their own entry point.
2. If the thought carries a signature, it must name `signing_key_id`; the agent
   must be registered; the key must exist and not be revoked; the signature
   must verify under the version rules above. A bad signature is rejected on
   every chain, `Open` included.
3. The chain's policy applies:

| Policy | Rule for a new thought |
|---|---|
| `Open` (default) | No signature required. |
| `Signed` | A valid v2 signature by an active key registered to the claimed agent. |
| `Allowlist` | A valid v2 signature by a key whose `(agent_id, key_id, public key)` is in the policy's allowed set **and** that is still active in the registry (revocation is honoured at once). |

Failures are `AccessError` values wrapped in `std::io::Error` with kind
`PermissionDenied`, so embedders can `downcast_ref::<AccessError>()`. REST
returns 403, MCP a permission error.

Existing chains have no policy record and are `Open`; nothing changes for them.

## Policy records (governance in the chain)

A policy change is a thought of the new type **`AccessPolicy`** (role `Audit`),
so it is covered by the hash chain, signed, listed by ordinary traversal and
search, and replayed when the chain is opened.

`content` is exactly this JSON object (field order as shown, no extra fields):

```json
{"format":"mentisdb/access-policy/v1","seq":0,"mode":"allowlist",
 "keys":[{"agent_id":"alice","key_id":"alice-202610","public_key":"<64 hex>"}],
 "keys_root":"<64 hex>","prev_policy":""}
```

* `mode`: `open`, `signed` or `allowlist`.
* `keys`: the **governor / allow set**, sorted by leaf hash. Governors may
  change the policy in every mode; in `Allowlist` mode they are also the
  writers.
* `keys_root`: the commitment to `keys`, an RFC 6962 Merkle tree hash.
  Leaf = `SHA256(0x00 || "mentisdb/access-key/v1\0" || str agent_id || str key_id || bytes[32] public_key)`.
  Node = `SHA256(0x01 || left || right)`; for `n > 1` leaves the split point is the
  largest power of two below `n`; an empty set hashes to `SHA256("")`. A verifier
  recomputes it and rejects a record whose root does not match its keys.
* `seq`: 0 for the first record, then previous `seq + 1`.
* `prev_policy`: the record hash of the previous policy thought, `""` for `seq` 0.
  Together with the hash chain this makes the policy history a verifiable list.

The record thought is signed with the thought v2 payload (so the signature also
binds the content, chain and position). Its `agent_id` and `signing_key_id` name
the signer.

### Validity rules (checked on submission and on replay)

1. `content` parses with exactly the fields above; `seq` and `prev_policy`
   continue the previous valid record; `keys_root` matches `keys`; every key is
   a valid curve point; no duplicate `(agent_id, key_id)`; at most 1024 keys.
2. The thought carries a valid v2 signature.
3. **Authorization.** For `seq > 0` the signer's `(agent_id, key_id, public key)`
   must be in the *current* policy's `keys`. For `seq = 0` (bootstrap) the signer
   must be in the new record's own `keys`.
4. `mode` of `signed` or `allowlist` requires a non-empty `keys` so the policy
   cannot lock the chain permanently with nobody able to change it.

An invalid record found on replay (for example planted by file tampering after
genesis) is **ignored** for enforcement and reported by `chain access verify`.


## Verification when a chain opens

Signatures used to be checked only on append. Opening a chain with governance
records now verifies the governed part of it, so a file edited behind
MentisDB's back cannot introduce unsigned or forged thoughts into a private
segment:

* Every thought in a signed or allowlist segment must carry a valid v2
  signature by a key of its agent that was in the allowed set at that point,
  and every governance record must be a valid continuation. Later revocations
  are ignored: the key was good when it signed.
* A thought that fails is **quarantined**: hidden from every reader (the
  authorized ones too), never returned by any route, reported by
  `MentisDb::verification`, in `chain access verify` (`quarantined`, non-zero
  exit) and in the signature report, like an invalid policy record.
* **Default while unverified: nothing is served.** Verification runs inside
  `open`, so a chain is never readable before it finished; there is no
  "pending" state in which private thoughts are served unchecked. The server
  opens chains lazily off the request path, as it always did.
* **Checkpoint.** After a clean run the chain writes `<chain>.verified.json`,
  signed by the operator key and bound to the chain key, the hash at the
  checkpoint, the policy state there (`keys_root`, `seq`) and the quarantine
  list. The next open trusts it only if the signature verifies with the local
  operator key and the chain still has exactly that hash at that position, and
  then verifies only what is newer. Without an operator key there is no
  checkpoint and every open verifies everything (about 25 µs per signed
  private thought).
* **Divergence locks the chain.** If a valid signed checkpoint exists but the
  file no longer matches it (records removed, prefix rewritten), the chain is
  *locked*: every position reads as private to every reader. Without this a
  truncation that removes the governance records would turn a private chain
  into a fully public one. After inspecting the file the operator deletes the
  `.verified.json` file to re-seal. Restoring a backup over a chain removes
  its checkpoint.
* A forged checkpoint (no operator key, wrong signature, other chain) is
  ignored and the chain is verified from scratch. Someone who can read the
  operator key can forge anything; that is the filesystem boundary again.

## Moving thoughts between chains

Merging and copying re-append thoughts unsigned under another chain's key,
and a merge then deletes the source. For a chain with private segments,
governance records or signed thoughts that would publish private text,
discard signatures (which bind chain and position) and destroy the originals.
`chains/merge` and the dashboard's merge and copy therefore **refuse** such a
source with a clear error and change nothing. `chains/branch` copies nothing
(the branch records its parent), so it is allowed from any thought the caller
can see. Branch-ancestor search checks each ancestor chain's audience and the
bearer token's chain scope on its own.

## Enforcement surfaces

| Surface | Behaviour |
|---|---|
| Library | `MentisDb::append_thought` (above). `ThoughtQuery::audience`, `StandingContextQuery::audience`, `*_for(audience)` variants (`get_thought_for`, `thought_json_for`, `hit_json_for`, `summary_candidates_for`, `resolve_context_for`, `verify_signatures_for`, `append_advice_for`, `version_info_for`, `is_invalidated_for`), `is_visible`, `visible_thoughts`, `redact_for`, `segments()`, `is_read_authorized`. The default audience is `Authorized`: a program that holds the data directory holds every byte. |
| REST and MCP | Every request runs with the audience its proof earns, per chain; with no proof it is public. Code that runs outside the middleware (a unit test calling a service method) is public too, so forgetting to authenticate loses data instead of leaking it. |
| Dashboard | PIN required for chains with private segments and for all access administration. |
| CLI | `mentisdb chain access show \| verify \| admin \| set \| allow \| deny`, `mentisdb agent key add \| revoke`, `agent disable \| activate`, `mentisdb key generate \| show`; read commands take `--read-key-file`; operator commands take `--pin` or `--offline`. |

## Migration

* Existing chains are untouched. A chain without governance records is one
  public segment. A chain governed before this model is private from the
  position of its first non-`open` record; its governance records are private.
* `ChainRegistration` gains two optional fields (`has_private`,
  `public_thought_count`) written on the next flush; absent means public.
* v1 signatures already stored remain valid on public chains and are reported
  as `v1` by `chain access verify`.
* `add_agent_key` (library) still requires a proof of possession and a valid
  curve point; `add_agent_key_as_operator` is the operator's variant.
* Clients that registered keys or set policies over REST or MCP must move to
  the operator commands; the routes answer 403.
* Thought v2 signatures are unchanged and backward compatible.

## Cost of verification on append

`cargo bench --bench access_policy` (aarch64 macOS, release build, buffered writes
so the number is CPU cost, not fsync): an unsigned append to an `Open` chain takes
about 55 µs; a signed append to an `Open`, `Signed` or `Allowlist` chain takes about
75-80 µs, i.e. roughly 22-25 µs more, almost all of it the single strict Ed25519
verification (about 25 µs). The v2 payload rebuild costs about 165 ns and the
allowlist membership check a few hundred ns. Unsigned appends on `Open` chains
pay only a constant-time mode check, so existing deployments see no change. With
durable (fsync) writes the verification is small next to the disk flush.

## Cost of the audience filter

`cargo bench --bench audience_search` (aarch64 macOS, release, one chain of
10 000 public thoughts followed by 10 000 signed private ones, ranked search for
three words, limit 10):

| Case | Time |
|---|---|
| Public chain with no private segment, 10 000 thoughts (every existing deployment) | about 334 µs |
| Private chain, authorized reader (20 000 thoughts visible) | about 691 µs |
| Private chain, public reader (10 000 thoughts visible) | about 423 µs |

The public reader pays about 27% over a chain of the same visible size: the
candidate filter walks the whole chain and BM25 statistics are recomputed over
the visible subset instead of read from the index. Counting visible thoughts
for `head` and listings is a linear scan (about 30 µs at 20 000 thoughts).
Chains with no private segment skip all of it: the audience check is a single
comparison.

## Fuzzing

`fuzz/` holds cargo-fuzz targets for the formats above: `policy_record` (parsing is
a typed accept or reject, accepted records are canonical and rebuildable),
`signature_envelope` (stored-signature shapes, key validation, hex, and "never
verifies with a key validation rejects"), `thought_payload_v2` (deterministic,
domain-prefixed, and distinct chain/agent/head triples never collide), `cli_args`,
`read_auth_header` (the read-proof grammar: typed accept or reject, canonical
re-rendering, verification never panics for any clock), `operator_op` (operator
command bodies and key specs) and `segment_map` (a public reader sees a subset of an
authorized reader's view and never a governance record). Run one with
`cd fuzz && cargo +nightly fuzz run read_auth_header`.

## One writer per data directory

Two processes appending to the same hash chain corrupt it, and a second writer
would also bypass every check in this document. The daemon therefore takes an
operating-system exclusive lock on `<data dir>/mentisdb.lock` for its whole life.
The lock is released by the OS when the process exits or is killed, so a crash
leaves no stale lock to clean up. The file also holds the holder's PID, so a
refused process can name it.

| Process | While another process holds the directory |
|---|---|
| second `mentisdb` daemon | refuses to start, nonzero exit, message names the pid and directory |
| `--offline` operator commands | refuse with the same message; nothing is written |
| `restore` | refuses before touching any file |
| `migrate run` (not `--dry-run`), `retry`, `skip`, `rollback`, `purge-backups` | refuse; the daemon runs its own migrations inside the process that holds the lock |
| `migrate status`, `migrate verify`, `migrate run --dry-run` | read-only, always allowed |

To change a running daemon's keys, use the dashboard (`--pin`). To use an
offline command, stop the daemon first. The lock is advisory: it protects
against mentisdb processes, not against another program that ignores it, and
network filesystems may not honor it, so keep the data directory on a local disk.

## Residual risk

* A public reader learns the existence and rough size of private segments and one
  boundary hash (see above). Hiding those would break verifiability of the
  public history.
* Content written while a chain was public stays public forever; going private
  does not retract it.
* A key that was once allowed and later removed keeps whatever it already read.
* Latency side channels are not closed.
* The operator key and the data directory are as trusted as the host.
* No general request rate limiting exists (only the invalid-proof limiter); run behind a reverse proxy that limits request rates if the port is exposed.
