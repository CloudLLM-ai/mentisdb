---
schema_version: 1
name: rust-concurrency-patterns
description: >
  Use this skill when designing or reviewing concurrent Rust code involving
  shared mutable state, lock contention, async tasks, and throughput
  optimization. Covers RwLock vs DashMap, write batching, per-entity locking,
  and WAL patterns.
tags:
  - rust
  - concurrency
  - async
  - tokio
  - dashmap
  - rwlock
  - performance
  - throughput
triggers:
  - concurrent access
  - lock contention
  - RwLock
  - DashMap
  - write throughput
  - async shared state
  - mutex
  - high concurrency
---

# rust-concurrency-patterns

Use this skill when designing or reviewing concurrent Rust code involving
shared mutable state, lock contention, async tasks, and throughput
optimization. Covers RwLock vs DashMap, write batching, per-entity locking,
and WAL patterns.

Companion to `rust-engineer.md`. Prefer that skill for general style; load
this one when concurrency or write throughput is the problem.

# Rust Concurrency Patterns for High-Throughput Services

## The Core Principle

Lock granularity determines throughput ceiling. Every shared lock is a
serialization point. Identify which locks are on the hot path and eliminate
or shrink them.

MentisDB already follows the per-entity shape for chains: a sharded map of
`chain_key → Arc<RwLock<MentisDb>>` (DashMap + per-chain write lock). New code
should preserve that shape, not reintroduce a global chain map lock.

## Pattern 1: DashMap Instead of `RwLock<HashMap>`

**Problem**: `Arc<RwLock<HashMap<K, V>>>` serializes all concurrent callers on
the outer map — even reads that do not need to modify the map must wait for
writers.

**Solution**: `DashMap<K, V>` shards the map across `2 × CPU` buckets.
Concurrent operations on different keys proceed in parallel with no outer lock.

```toml
[dependencies]
dashmap = "6"
```

```rust
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

// Before — all requests serialize on the outer lock:
// Arc<RwLock<HashMap<String, Arc<RwLock<Entity>>>>>

// After — concurrent requests for different keys are parallel:
struct Service {
    entities: Arc<DashMap<String, Arc<RwLock<Entity>>>>,
}

impl Service {
    async fn get_or_create(&self, key: &str) -> Arc<RwLock<Entity>> {
        // Fast path: key exists — only shard read lock held briefly
        if let Some(existing) = self.entities.get(key) {
            return existing.clone();
        }
        // Slow path: atomic at shard level — no outer async lock needed
        self.entities
            .entry(key.to_string())
            .or_try_insert_with(|| Entity::open(key).map(|e| Arc::new(RwLock::new(e))))
            .unwrap()
            .clone()
    }
}
```

**When to use**: Any map where values are opened/created lazily and many
concurrent callers may look up different keys.

## Pattern 2: Write Buffering (`FLUSH_THRESHOLD`)

**Problem**: Writing to disk on every operation caps throughput at
~1/disk_latency (e.g., 2ms write = max 500 ops/s).

**Solution**: Accumulate writes in memory, flush every N operations or on drop.
Trade per-operation durability for batch throughput.

```rust
const FLUSH_THRESHOLD: usize = 16;

struct BufferedWriter {
    file: BufWriter<File>, // keeps file open — eliminates reopen overhead
    write_buffer: Vec<u8>,
    dirty_count: usize,
    auto_flush: bool, // true = flush every write (safe); false = batch
}

impl BufferedWriter {
    fn append(&mut self, data: &[u8]) -> io::Result<()> {
        if self.auto_flush {
            self.file.write_all(data)?;
            self.file.flush()?;
        } else {
            self.write_buffer.extend_from_slice(data);
            self.dirty_count += 1;
            if self.dirty_count >= FLUSH_THRESHOLD {
                self.flush_buffer()?;
            }
        }
        Ok(())
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.write_buffer.is_empty() {
            return Ok(());
        }
        let buf = std::mem::take(&mut self.write_buffer);
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.dirty_count = 0;
        Ok(())
    }
}

impl Drop for BufferedWriter {
    fn drop(&mut self) {
        let _ = self.flush_buffer(); // never silently lose data
    }
}
```

**Tradeoff**: With `auto_flush=false`, up to `FLUSH_THRESHOLD - 1` operations
may be lost on hard crash. Make this a runtime config option so operators can
choose (MentisDB: `auto_flush` on binary chains).

## Pattern 3: Per-Entity Locking (Not Global)

**Problem**: One global `RwLock<State>` serializes all operations across all
entities.

**Solution**: One `Arc<RwLock<Entity>>` per entity. Reads on entity A do not
block writes on entity B.

```rust
// DON'T: single global lock
struct Service {
    state: Arc<RwLock<AllEntities>>,
}

// DO: per-entity lock, map lookup via DashMap
struct Service {
    entities: Arc<DashMap<EntityId, Arc<RwLock<Entity>>>>,
}
```

With per-entity locking + DashMap, theoretical write throughput scales linearly
with the number of distinct entities being written concurrently.

## Pattern 4: Read-Write Asymmetry

Most services have many more reads than writes. Exploit this:

- Use `tokio::sync::RwLock` (not `std::sync::RwLock`) for async contexts —
  multiple concurrent readers, one exclusive writer
- Keep read locks held for the minimum time — get the data, drop the lock,
  process
- Never hold a write lock across an await point if avoidable

```rust
// Bad: lock held across await
let mut guard = entity.write().await;
let result = some_async_call().await; // write lock held during I/O!
guard.update(result);

// Good: minimize lock scope
let data = {
    let guard = entity.read().await;
    guard.snapshot() // cheap clone
};
let result = process(data).await; // no lock held
entity.write().await.apply(result);
```

## Pattern 5: WAL (Write-Ahead Log) for Maximum Write Throughput

When per-write disk I/O is unavoidable and write throughput must exceed
~1k req/s per entity:

1. Writes go to in-memory ring buffer + append-only WAL file (µs latency)
2. Background Tokio task flushes WAL to main snapshot every 10–100ms or N entries
3. Readers merge WAL + snapshot for consistent view
4. On crash: replay WAL on open

```text
write() ──► ring_buffer + WAL_append  (~5 µs)
                    │
        background flush task
                    │
              main snapshot  (durable, maybe 100ms stale)
```

**Expected throughput**: 5k–50k writes/s per entity vs. ~430/s with per-write
fsync.

**When to use**: High-write-throughput scenarios where brief staleness on crash
is acceptable (e.g., agent thought chains, event logs). MentisDB favors
append-only durability; any WAL design must still preserve hash-chain integrity
on replay.

## Throughput Reference Table

| Pattern | Write throughput / entity | Durability |
|---|---|---|
| Per-write fsync, file reopen | ~250–500 req/s | Full |
| Per-write fsync, BufWriter (no reopen) | ~500–800 req/s | Full |
| Batch flush (`FLUSH_THRESHOLD=16`) | ~2k–5k req/s | Last 15 ops at risk |
| WAL + background flush | ~10k–50k req/s | Last flush window |
| Pure in-memory | ~1M+ req/s | None (process restart loses all) |

## Common Mistakes

- Using `std::sync::Mutex` in async code — blocks the Tokio worker. Use
  `tokio::sync::Mutex` or restructure to avoid holding across `.await`.
- Holding a `DashMap` entry ref across an await — entry refs hold a shard lock.
  Clone the value out before awaiting.
- Dropping `Arc<RwLock<T>>` while a lock guard is live on another thread —
  deadlock risk. Keep guards local and short-lived.
- Forgetting `Drop` on buffer structs — silent data loss on panic or early return.

## Version history

- **2026-07-09** — Initial skill; linked from `rust-engineer.md`.
