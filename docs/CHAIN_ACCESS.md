# Per-chain write access control

Status: design, implemented in slices (see the changelog `UNRELEASED` block).

MentisDB chains are append-only and hash-linked, but until now anyone who could
reach the REST/MCP port could write to any chain, `signing_key_id` /
`thought_signature` were stored without being checked, and agent public keys
could be registered without proving possession of the private key. This
document defines how a chain can require Ed25519-signed writes, how that
requirement is recorded in the chain itself, and how it is enforced.

## Threat model

In scope:

1. **Unauthorized writer.** A client that can call the API but is not supposed
   to write to a chain (stolen bearer token, shared network, another tenant).
2. **Forged identity.** A writer claiming to be agent `A` without `A`'s key.
3. **Replay.** Re-submitting a thought `A` really signed onto another chain, or
   onto the same chain at another position.
4. **Key squatting.** Registering a public key one does not own, or a weak or
   malformed key, so later signature checks are meaningless.
5. **Silent policy change.** Loosening the policy (or swapping the allowed key
   set) without a trace; the policy history must be auditable and tamper
   evident.

Out of scope (documented limits):

* An attacker with write access to the chain *files* can append records. The
  hash chain detects edits to existing records but cannot stop a well-formed
  append. Policy records are still self-authenticating (they must verify under
  the rules below), so file access alone cannot forge a policy change **after
  the genesis policy record exists**. It can still plant the genesis record on a
  chain that never had one (see *Bootstrap*).
* Confidentiality. Policies control who may write, not who may read.
* Private-key custody. Keys are generated and held by the agent.
* The agent registry is a sidecar file, not part of the hash chain. `Signed`
  chains therefore trust the registry's key list; `Allowlist` chains do not,
  because the allowed public keys are committed inside the chain.

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

### Bootstrap

A chain starts `Open` with no governance record. The first record (`seq` 0) is
the bootstrap:

* It is accepted only through the operator-authenticated entry points: the
  library call `MentisDb::set_access_policy`, REST/MCP with a **global** bearer
  token, or the dashboard behind its PIN. A chain-scoped token cannot bootstrap.
* The signer must include their own key in `keys`; that key becomes the first
  governor. This is trust-on-first-record: whoever the operator lets publish
  `seq` 0 is the root of trust. Operators should record the genesis record's
  `hash` out of band (`chain access show` prints it).
* After `seq` 0 exists, the operator-authenticated entry points are no longer
  enough: every further change needs a governor's signature (rule 3).
* Lost governance keys cannot be recovered in place. The documented recovery is
  `chain branch` from the last thought before the first policy record, or
  restoring a backup.

Setting the mode back to `open` is a normal signed record; the governor set
stays on record and keeps controlling later changes.

## Enforcement surfaces

| Surface | Behaviour |
|---|---|
| Library | `MentisDb::append_thought` (above); `set_access_policy`, `access_policy`, `verify_signatures`. |
| MCP and REST | All appends go through the library path. New tools/routes: `mentisdb_get_access_policy` (`POST /v1/access/policy`), `mentisdb_set_access_policy` (`POST /v1/access/policy/set`), `mentisdb_verify_signatures` (`POST /v1/access/verify`). `set` needs a global token. Appending with a bad or missing signature returns 403. |
| Dashboard | Chain detail shows the mode, `keys_root`, `seq`, the allowed keys and the genesis hash (read only). Changes go through the API/CLI because they need a private-key signature. |
| CLI | `mentisdb chain access show \| set \| allow \| deny \| verify`, plus `mentisdb key generate`. |

## Migration

* No schema change: existing chains and thoughts are untouched and stay `Open`.
* v1 signatures already stored remain valid on `Open` chains and are reported as
  `v1` by `chain access verify`.
* `add_agent_key` now requires a proof of possession and a valid curve point;
  keys already in a registry are not re-checked and keep working, but
  `chain access verify` flags any that are not valid points.
* Adding the `AccessPolicy` thought type changes nothing for old readers until
  a policy record is written; a chain that has one cannot be read by a
  MentisDB version that predates the type.

## Cost of verification on append

`cargo bench --bench access_policy` (aarch64 macOS, release build, buffered writes
so the number is CPU cost, not fsync): an unsigned append to an `Open` chain takes
about 55 µs; a signed append to an `Open`, `Signed` or `Allowlist` chain takes about
75-80 µs, i.e. roughly 22-25 µs more, almost all of it the single strict Ed25519
verification (about 25 µs). The v2 payload rebuild costs about 165 ns and the
allowlist membership check a few hundred ns. Unsigned appends on `Open` chains
pay only a constant-time mode check, so existing deployments see no change. With
durable (fsync) writes the verification is small next to the disk flush.

## Fuzzing

`fuzz/` holds cargo-fuzz targets for the formats above: `policy_record` (parsing is
a typed accept or reject, accepted records are canonical and rebuildable),
`signature_envelope` (stored-signature shapes, key validation, hex, and "never
verifies with a key validation rejects"), `thought_payload_v2` (deterministic,
domain-prefixed, and distinct chain/agent/head triples never collide) and
`cli_args`. Run one with `cd fuzz && cargo +nightly fuzz run policy_record`.
