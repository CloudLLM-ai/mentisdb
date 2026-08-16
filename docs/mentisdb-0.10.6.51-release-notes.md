# MentisDB 0.10.6.51

Permanent skill delete, Skills summary counts, vector sidecar WAL, and search persist wins.

**Date:** August 15, 2026

## Upgrade

```bash
cargo install mentisdb --locked --force
```

Or let the daemon self-updater install tag `0.10.6.51`. Restart the daemon after install.

Existing vector sidecars keep working. The next append starts a sibling `*.json.wal`. Older binaries that only read the JSON snapshot treat a sidecar with a pending WAL as stale and rebuild.

## Skill delete

Revoke still keeps every version for audit. Permanent delete is the explicit exception to the registry’s append-only lifecycle: `SkillRegistry::delete_skill` removes the skill and all versions from `mentisdb-skills.bin`. After delete, the same `skill_id` can be uploaded again as Active.

| Surface | How |
|---|---|
| Crate | `SkillRegistry::delete_skill(skill_id)` |
| REST | `POST /v1/skills/delete` `{ "skill_id": "…" }` |
| MCP | `mentisdb_delete_skill` |
| Dashboard | `DELETE /dashboard/api/skills/{id}` |

Crate, REST, and MCP delete any existing skill. The dashboard UI matches bearer-token delete:

1. **Revoke** on active/deprecated skills (audit row stays).
2. **Delete** on the revoked list row, or **Delete Permanently…** on the detail pane (type the skill id to confirm).

The Skills page summary bar shows total / active / revoked / deprecated (`SkillRegistry::counts()`). Deleted skills are absent and are not counted.

## Vector sidecar WAL

Each append writes one integrity-chained WAL record (`MDBVWAL1`, digest `SHA-256(prev \|\| entry)`) instead of rewriting the full JSON snapshot. Load verifies the snapshot, then replays `*.json.wal`. After 32 records the snapshot is compacted and the WAL is removed.

The thought log remains canonical. A corrupt WAL fails verification and the sidecar is rebuilt.

## Persist and search

- MCP/REST append releases the chain write lock before sidecar WAL and overlay I/O. Crate `append_thought` still flushes derived files before return.
- Implicit-edge first build: pairwise for N ≤ 128; temporary HNSW top-k above that.
- Dashboard reopens a cached chain only when on-disk thought count is ahead of the live handle.
- `query_ranked` uses `search_filtered` on the cached Exact/HNSW backend.
- Exact cosine top-k uses `select_nth_unstable_by`.
- Caches: adjacency by thought count, process-wide Porter stemmer, thesaurus `&[String]`, HNSW `ef_search`, webhook `reqwest::Client`, bearer-token registry mtime.
- Skill persist encodes the live map by reference; `SkillVersion.schema_version` is stored.

## Verification

- `make clippy` (`-D warnings`)
- `cargo test --all-features`
- `cargo package --features local-embeddings`
- Criterion `search_ranked` smoke
- Full LoCoMo / LongMemEval not re-run: fusion weights unchanged. Overlay first-build is approximate only for N > 128.

## Links

- https://github.com/CloudLLM-ai/mentisdb/releases/tag/0.10.6.51
- https://crates.io/crates/mentisdb
- https://docs.mentisdb.com
