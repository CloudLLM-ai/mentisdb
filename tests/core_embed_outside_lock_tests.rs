//! Tests for embedding outside the chain lock: a deferred append issues
//! tickets instead of calling the model, tickets are embedded with no lock,
//! and committing the outcomes keeps the sidecar complete and in chain order
//! even when they arrive out of order, fail, or never arrive.

use mentisdb::search::{
    EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, LocalTextEmbeddingProvider,
    VectorSidecarFreshness,
};
use mentisdb::{EmbedOutcome, MentisDb, ThoughtInput, ThoughtType};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[derive(Debug)]
struct ProbeError;

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("injected embedding failure")
    }
}

impl std::error::Error for ProbeError {}

/// Counts model calls and can fail the next one.
#[derive(Default)]
struct Probe {
    calls: AtomicUsize,
    fail_next: AtomicBool,
    /// Milliseconds every call sleeps, to model a slow embedding backend.
    delay_ms: AtomicUsize,
}

struct ProbeProvider {
    inner: LocalTextEmbeddingProvider,
    probe: Arc<Probe>,
}

impl EmbeddingProvider for ProbeProvider {
    type Error = ProbeError;

    fn metadata(&self) -> &EmbeddingMetadata {
        self.inner.metadata()
    }

    fn embed_batch(
        &self,
        inputs: &[mentisdb::search::EmbeddingInput],
    ) -> Result<Vec<EmbeddingVector>, ProbeError> {
        if self.probe.fail_next.swap(false, Ordering::SeqCst) {
            return Err(ProbeError);
        }
        self.probe.calls.fetch_add(inputs.len(), Ordering::SeqCst);
        let delay = self.probe.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay as u64));
        }
        self.inner.embed_batch(inputs).map_err(|_| ProbeError)
    }
}

fn open_chain(dir: &std::path::Path, key: &str) -> (MentisDb, Arc<Probe>, EmbeddingMetadata) {
    let probe = Arc::new(Probe::default());
    let provider = ProbeProvider {
        inner: LocalTextEmbeddingProvider::new(),
        probe: Arc::clone(&probe),
    };
    let metadata = provider.metadata().clone();
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    (chain, probe, metadata)
}

/// Append deferred, returning the outcomes of embedding its tickets.
fn deferred_append(chain: &mut MentisDb, text: &str) -> Vec<EmbedOutcome> {
    chain.defer_next_embedding();
    chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Finding, text))
        .unwrap();
    chain
        .take_embed_tickets()
        .into_iter()
        .map(|ticket| ticket.embed())
        .collect()
}

fn wait_for_adoption(chain: &mut MentisDb) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while chain.has_pending_sidecar_rebuilds() {
        assert!(Instant::now() < deadline, "rebuild never finished");
        std::thread::sleep(Duration::from_millis(10));
        chain.poll_vector_backend_upgrades();
    }
}

/// Assert the persisted sidecar covers `count` thoughts, in index order, fresh.
fn assert_persisted_sidecar(
    dir: &std::path::Path,
    key: &str,
    metadata: &EmbeddingMetadata,
    count: usize,
) {
    let reopened = MentisDb::open_with_key(dir, key).unwrap();
    let sidecar = reopened
        .load_vector_sidecar(metadata)
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), count);
    for (position, entry) in sidecar.entries.iter().enumerate() {
        assert_eq!(entry.thought_index, position as u64, "chain order");
    }
    assert!(matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, metadata)
            .unwrap(),
        VectorSidecarFreshness::Fresh
    ));
}

/// The append itself never calls the model; embedding happens only when the
/// ticket is run, which the caller does with no lock held.
#[test]
fn test_deferred_append_does_not_embed_under_the_lock() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, _) = open_chain(dir.path(), "defer");
    chain.defer_next_embedding();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, "no model call"),
        )
        .unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    let tickets = chain.take_embed_tickets();
    assert_eq!(tickets.len(), 1);
    assert_eq!(tickets[0].thought_index(), 0);
    let outcomes: Vec<_> = tickets.into_iter().map(|t| t.embed()).collect();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    chain.commit_embeddings(outcomes);
}

/// The full sequence leaves a complete, fresh sidecar on disk.
#[test]
fn test_deferred_append_commit_flush_persists_a_fresh_sidecar() {
    let dir = tempdir().unwrap();
    let (mut chain, _, metadata) = open_chain(dir.path(), "full");
    for n in 0..5 {
        let outcomes = deferred_append(&mut chain, &format!("entry {n}"));
        chain.commit_embeddings(outcomes);
        chain.flush_pending_derived().unwrap();
    }
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "full", &metadata, 5);
}

/// Appends whose embeddings finish out of order are still applied in chain
/// order: a vector that arrives early is parked until the gap closes.
#[test]
fn test_out_of_order_commits_are_applied_in_chain_order() {
    let dir = tempdir().unwrap();
    let (mut chain, _, metadata) = open_chain(dir.path(), "reorder");
    let first = deferred_append(&mut chain, "zero");
    let second = deferred_append(&mut chain, "one");
    let third = deferred_append(&mut chain, "two");
    chain.commit_embeddings(third);
    chain.commit_embeddings(second);
    chain.commit_embeddings(first);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "reorder", &metadata, 3);
}

/// Another writer appends between this thought's append and its commit (the
/// chain "moved on"): the vector still lands at its own position.
#[test]
fn test_commit_after_chain_moved_on_keeps_coverage_correct() {
    let dir = tempdir().unwrap();
    let (mut chain, _, metadata) = open_chain(dir.path(), "moved");
    let early = deferred_append(&mut chain, "early");
    for n in 0..3 {
        let outcomes = deferred_append(&mut chain, &format!("later {n}"));
        chain.commit_embeddings(outcomes);
    }
    chain.commit_embeddings(early);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "moved", &metadata, 4);
}

/// A model failure never fails the append; the sidecar is repaired by the
/// background rebuild and ends complete.
#[test]
fn test_embed_failure_is_repaired_by_rebuild() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "fail");
    let ok = deferred_append(&mut chain, "fine");
    chain.commit_embeddings(ok);
    probe.fail_next.store(true, Ordering::SeqCst);
    let failed = deferred_append(&mut chain, "model fails here");
    chain.commit_embeddings(failed);
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "fail", &metadata, 2);
}

/// If a ticket is dropped without being committed (a cancelled caller) and
/// later vectors pile up behind it, the sidecar is rebuilt rather than wedged.
#[test]
fn test_lost_ticket_does_not_wedge_the_sidecar() {
    let dir = tempdir().unwrap();
    let (mut chain, _, metadata) = open_chain(dir.path(), "lost");
    let lost = deferred_append(&mut chain, "never committed");
    drop(lost);
    let total = 70;
    for n in 1..total {
        let outcomes = deferred_append(&mut chain, &format!("behind the lost one {n}"));
        chain.commit_embeddings(outcomes);
    }
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "lost", &metadata, total);
}

/// A process that dies after the append but before the commit leaves the
/// sidecar behind the chain; the next open treats it as stale and the next
/// append repairs it in the background.
#[test]
fn test_crash_between_append_and_commit_is_repaired_on_next_append() {
    let dir = tempdir().unwrap();
    let (mut chain, _, metadata) = open_chain(dir.path(), "crash");
    let outcomes = deferred_append(&mut chain, "ok");
    chain.commit_embeddings(outcomes);
    chain.flush().unwrap();
    let uncommitted = deferred_append(&mut chain, "dies before commit");
    drop(uncommitted);
    drop(chain); // simulated crash: tickets and vectors are gone

    let (mut chain, _, _) = open_chain(dir.path(), "crash");
    chain
        .append("a", ThoughtType::Finding, "after restart")
        .unwrap();
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "crash", &metadata, 3);
}

/// Without `defer_next_embedding` the append embeds synchronously, as before.
#[test]
fn test_default_append_still_embeds_synchronously() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, _) = open_chain(dir.path(), "sync");
    chain.append("a", ThoughtType::Finding, "inline").unwrap();
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert!(chain.take_embed_tickets().is_empty());
}

/// While a slow embedding runs, other users of the chain are not blocked: the
/// chain lock is free for the whole model call. (With the old in-lock embed
/// the reader below waited for the full delay.)
#[test]
fn test_readers_are_not_blocked_while_a_slow_embedding_runs() {
    use std::sync::RwLock;
    let dir = tempdir().unwrap();
    let (chain, probe, metadata) = open_chain(dir.path(), "slow");
    probe.delay_ms.store(400, Ordering::SeqCst);
    let chain = Arc::new(RwLock::new(chain));

    let writer = {
        let chain = Arc::clone(&chain);
        std::thread::spawn(move || {
            let tickets = {
                let mut guard = chain.write().unwrap();
                guard.defer_next_embedding();
                guard
                    .append_thought("a", ThoughtInput::new(ThoughtType::Finding, "slow"))
                    .unwrap();
                guard.take_embed_tickets()
            }; // write lock released here
            let outcomes: Vec<_> = tickets.into_iter().map(|t| t.embed()).collect();
            chain.write().unwrap().commit_embeddings(outcomes);
            chain.read().unwrap().flush_pending_derived().unwrap();
        })
    };

    // Let the writer reach the model call, then time a read.
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    let len = chain.read().unwrap().thoughts().len();
    let waited = started.elapsed();
    assert_eq!(len, 1, "the append is visible while the vector is pending");
    assert!(
        waited < Duration::from_millis(150),
        "a reader waited {waited:?} behind the embedding"
    );
    writer.join().unwrap();
    chain.write().unwrap().flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "slow", &metadata, 1);
}

/// Regression: the implicit-edge overlay must see vectors committed outside
/// the lock. A deferred append once left the overlay without the new thought's
/// edges (and `.auto_edges.bin` unwritten), so graph search lost them.
#[test]
fn test_deferred_commit_updates_implicit_edge_overlay_like_sync_append() {
    let texts = [
        "latency budget for the europe rollout plan review meeting notes",
        "latency budget for the asia rollout plan review meeting notes",
        "latency budget for the americas rollout plan review meeting notes",
        "unrelated gardening notes about tomatoes",
    ];
    let sync_dir = tempdir().unwrap();
    let (mut sync_chain, _, _) = open_chain(sync_dir.path(), "edges");
    sync_chain.with_auto_edge_threshold(0.5);
    sync_chain
        .append("a", ThoughtType::Finding, texts[0])
        .unwrap();
    sync_chain.ensure_implicit_edge_overlay();
    for text in &texts[1..] {
        sync_chain.append("a", ThoughtType::Finding, text).unwrap();
    }

    let deferred_dir = tempdir().unwrap();
    let (mut deferred_chain, _, _) = open_chain(deferred_dir.path(), "edges");
    deferred_chain.with_auto_edge_threshold(0.5);
    deferred_chain
        .append("a", ThoughtType::Finding, texts[0])
        .unwrap();
    deferred_chain.ensure_implicit_edge_overlay();
    for text in &texts[1..] {
        let outcomes = deferred_append(&mut deferred_chain, text);
        deferred_chain.commit_embeddings(outcomes);
    }

    assert!(
        sync_chain.implicit_edge_count() > 0,
        "fixture must have edges"
    );
    assert_eq!(
        deferred_chain.implicit_edge_count(),
        sync_chain.implicit_edge_count()
    );
    assert_eq!(
        deferred_chain.implicit_edge_thought_coverage(),
        sync_chain.implicit_edge_thought_coverage()
    );
}
