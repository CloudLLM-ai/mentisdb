//! Tests for embedding outside the chain lock: a deferred append issues
//! tickets instead of calling the model, tickets are embedded with no lock,
//! and committing the outcomes keeps the sidecar complete and in chain order
//! even when they arrive out of order, fail, or never arrive.
//!
//! A vector that is slow, fails or is lost must cost one extra embedding of
//! that thought, never a re-embed of the corpus. The tests count provider
//! calls (every thought embedded exactly once, plus one per repair) to prove
//! it; nothing depends on wall-clock time.

use mentisdb::search::{
    EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, LocalTextEmbeddingProvider,
    VectorSidecarFreshness,
};
use mentisdb::{
    EmbedOutcome, EmbeddingBacklog, MentisDb, ThoughtInput, ThoughtType, MAX_EMBED_ATTEMPTS,
    MAX_PIPELINED_EMBEDDINGS,
};
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Counts model calls and can fail the next ones.
#[derive(Default)]
struct Probe {
    /// Thoughts embedded successfully.
    calls: AtomicUsize,
    /// How many upcoming model calls fail (failed calls are not counted).
    fail_remaining: AtomicUsize,
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
        let failing = take_failure(&self.probe.fail_remaining);
        if failing {
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

/// Consume one scheduled failure, if any is left.
fn take_failure(remaining: &AtomicUsize) -> bool {
    loop {
        let left = remaining.load(Ordering::SeqCst);
        if left == 0 {
            return false;
        }
        if remaining
            .compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return true;
        }
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

/// A model failure never fails the append. The one thought whose embedding
/// failed is embedded again in the background: two thoughts, two successful
/// model calls, no rebuild of the corpus (which would embed both thoughts
/// again).
#[test]
fn test_embed_failure_is_repaired_by_embedding_the_one_thought_again() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "fail");
    let ok = deferred_append(&mut chain, "fine");
    chain.commit_embeddings(ok);
    probe.fail_remaining.store(1, Ordering::SeqCst);
    let failed = deferred_append(&mut chain, "model fails here");
    chain.commit_embeddings(failed);
    assert_eq!(chain.embedding_backlog(&metadata).unwrap().in_flight, 1);
    wait_for_adoption(&mut chain);
    assert_eq!(
        chain.embedding_backlog(&metadata),
        Some(EmbeddingBacklog::default())
    );
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "fail", &metadata, 2);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
}

/// Regression: a failed model call used to clear every parked vector and
/// rebuild the whole sidecar. With a 20-thought corpus a rebuild would embed
/// 20 more thoughts; retrying the one thought embeds exactly one.
#[test]
fn test_transient_embed_failures_never_rebuild_the_corpus() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "flaky");
    for n in 0..20 {
        let outcomes = deferred_append(&mut chain, &format!("settled {n}"));
        chain.commit_embeddings(outcomes);
    }
    const { assert!(MAX_EMBED_ATTEMPTS > 2, "two failures must stay retryable") };
    probe.fail_remaining.store(2, Ordering::SeqCst);
    let failed = deferred_append(&mut chain, "fails twice");
    chain.commit_embeddings(failed);
    // Behind the failing thought: later appends park until it is repaired.
    for n in 0..3 {
        let outcomes = deferred_append(&mut chain, &format!("behind {n}"));
        chain.commit_embeddings(outcomes);
    }
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "flaky", &metadata, 24);
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        24,
        "every thought was embedded exactly once"
    );
}

/// A thought the provider keeps failing on is retried alone up to
/// [`MAX_EMBED_ATTEMPTS`] times, then the sidecar falls back to a full
/// background rebuild so the vectors behind it are not held back forever.
#[test]
fn test_a_thought_that_keeps_failing_falls_back_to_a_rebuild() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "poison");
    let ok = deferred_append(&mut chain, "fine");
    chain.commit_embeddings(ok);
    probe
        .fail_remaining
        .store(MAX_EMBED_ATTEMPTS as usize, Ordering::SeqCst);
    let failed = deferred_append(&mut chain, "never embeds");
    chain.commit_embeddings(failed);
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "poison", &metadata, 2);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 3, "1 + the rebuild's 2");
}

/// If a ticket is dropped without being committed (a cancelled caller) and
/// later vectors pile up behind it, only that thought is embedded again.
/// Before the fix the 64th later vector cleared the pipeline and rebuilt the
/// corpus.
#[test]
fn test_lost_ticket_does_not_wedge_the_sidecar() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "lost");
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
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        total + 1,
        "each thought once, plus the lost thought embedded again"
    );
}

/// A ticket dropped before it ever ran (the request was cancelled while the
/// embedding was queued) is detected the same way as a dropped outcome.
#[test]
fn test_ticket_dropped_before_running_is_embedded_again() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "unrun");
    chain.defer_next_embedding();
    chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Finding, "dropped"))
        .unwrap();
    drop(chain.take_embed_tickets());
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    // The next append notices the drop and redoes thought 0 as well.
    let outcomes = deferred_append(&mut chain, "after");
    chain.commit_embeddings(outcomes);
    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "unrun", &metadata, 2);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
}

/// A synchronous append (an import, a branch copy) that lands while deferred
/// appends still have vectors in flight finds the sidecar legitimately behind
/// the chain. It used to read that as a stale sidecar and rebuild all of it; it
/// must instead apply its own vector in chain order behind the in-flight one.
#[test]
fn test_synchronous_append_during_in_flight_embeddings_does_not_rebuild() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "mixed");
    for n in 0..20 {
        let outcomes = deferred_append(&mut chain, &format!("settled {n}"));
        chain.commit_embeddings(outcomes);
    }
    let in_flight = deferred_append(&mut chain, "still embedding");
    chain
        .append("a", ThoughtType::Finding, "synchronous import")
        .unwrap();
    chain
        .append("a", ThoughtType::Finding, "another synchronous import")
        .unwrap();
    assert!(!chain.has_pending_sidecar_rebuilds());
    assert_eq!(chain.embedding_backlog(&metadata).unwrap().parked, 2);
    chain.commit_embeddings(in_flight);
    assert_eq!(
        chain.embedding_backlog(&metadata).unwrap().parked,
        0,
        "the in-flight vector released the two behind it"
    );
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "mixed", &metadata, 23);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 23);
}

/// A straggler that finally arrives releases every vector parked behind it, but
/// one commit applies only a bounded number so the write lock is not held for
/// the whole backlog. The rest is applied by later polls, which the service
/// keeps running while `has_pending_sidecar_rebuilds` says so.
#[test]
fn test_released_backlog_is_applied_in_bounded_steps() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "backlog");
    let straggler = deferred_append(&mut chain, "slow embedding");
    let behind = 100;
    for n in 0..behind {
        let outcomes = deferred_append(&mut chain, &format!("behind {n}"));
        chain.commit_embeddings(outcomes);
    }
    assert_eq!(chain.embedding_backlog(&metadata).unwrap().parked, behind);
    assert!(!chain.has_pending_sidecar_rebuilds());

    chain.commit_embeddings(straggler);
    let left = chain.embedding_backlog(&metadata).unwrap().parked;
    assert!(
        left > 0 && left < behind,
        "one commit applied some of the backlog, not all ({left} left)"
    );
    assert!(
        chain.has_pending_sidecar_rebuilds(),
        "work remains for a poll"
    );
    wait_for_adoption(&mut chain);
    assert_eq!(
        chain.embedding_backlog(&metadata),
        Some(EmbeddingBacklog::default())
    );
    chain.flush().unwrap();
    drop(chain);
    assert_persisted_sidecar(dir.path(), "backlog", &metadata, behind + 1);
    assert_eq!(probe.calls.load(Ordering::SeqCst), behind + 1);
}

/// Regression for the embedding-ticket overflow: one straggling embedding that
/// outlasts more later appends than the pipeline holds used to clear every
/// parked vector and re-embed the whole corpus (a 1.3 s hiccup at 50 appends
/// per second was enough). Now the pipeline stays bounded by shedding the
/// excess appends, which are embedded again, alone, once the straggler
/// finishes. Every thought is embedded exactly once and nothing is rebuilt.
#[test]
fn test_straggler_outlasting_the_pipeline_does_not_re_embed_the_corpus() {
    let dir = tempdir().unwrap();
    let (mut chain, probe, metadata) = open_chain(dir.path(), "straggler");
    let settled = 50;
    for n in 0..settled {
        let outcomes = deferred_append(&mut chain, &format!("settled {n}"));
        chain.commit_embeddings(outcomes);
    }
    let straggler = deferred_append(&mut chain, "slow embedding");
    let behind = MAX_PIPELINED_EMBEDDINGS + 20;
    for n in 0..behind {
        let outcomes = deferred_append(&mut chain, &format!("behind the straggler {n}"));
        chain.commit_embeddings(outcomes);
    }
    // The straggler holds one slot, so `MAX - 1` later appends fit and the
    // rest were shed.
    let shed = behind - (MAX_PIPELINED_EMBEDDINGS - 1);
    let backlog = chain.embedding_backlog(&metadata).unwrap();
    assert_eq!(backlog.in_flight, 1);
    assert_eq!(backlog.parked, MAX_PIPELINED_EMBEDDINGS - 1);
    assert_eq!(backlog.missing, shed);
    assert!(
        !chain.has_pending_sidecar_rebuilds(),
        "no rebuild may be scheduled while the straggler is merely slow"
    );

    chain.commit_embeddings(straggler);
    wait_for_adoption(&mut chain);
    assert_eq!(
        chain.embedding_backlog(&metadata),
        Some(EmbeddingBacklog::default())
    );
    chain.flush().unwrap();
    drop(chain);
    let total = settled + 1 + behind;
    assert_persisted_sidecar(dir.path(), "straggler", &metadata, total);
    assert_eq!(
        probe.calls.load(Ordering::SeqCst),
        total,
        "every thought was embedded exactly once; a corpus re-embed would add {total} more"
    );
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
