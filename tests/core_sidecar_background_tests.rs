//! Regression tests for the append-time vector sidecar path: a stale or
//! missing sidecar is rebuilt by the single-flight background worker, never
//! by embedding the whole chain on the request path, and vector coverage gaps
//! are detected and repaired.

use chrono::Utc;
use mentisdb::search::{
    EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, LocalTextEmbeddingProvider,
    VectorSidecar, VectorSidecarEntry, VectorSidecarFreshness,
};
use mentisdb::{MentisDb, ThoughtType};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::ThreadId;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use uuid::Uuid;

/// Error injected by [`ProbeProvider`].
#[derive(Debug)]
struct ProbeError;

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("injected embedding failure")
    }
}

impl std::error::Error for ProbeError {}

/// Shared observations made by [`ProbeProvider`] across clones.
#[derive(Default)]
struct Probe {
    /// Embedded inputs per calling thread.
    calls: std::sync::Mutex<Vec<(ThreadId, usize)>>,
    /// When set, the next `embed_batch` fails once.
    fail_next: AtomicBool,
    /// Total inputs embedded.
    total: AtomicUsize,
}

impl Probe {
    fn inputs_embedded_on(&self, thread: ThreadId) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| *id == thread)
            .map(|(_, count)| count)
            .sum()
    }
}

/// Deterministic provider that records who embeds and can fail on demand.
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
        self.probe
            .calls
            .lock()
            .unwrap()
            .push((std::thread::current().id(), inputs.len()));
        self.probe.total.fetch_add(inputs.len(), Ordering::SeqCst);
        self.inner.embed_batch(inputs).map_err(|_| ProbeError)
    }
}

/// Poll until the handle has adopted every background rebuild.
fn wait_for_adoption(chain: &mut MentisDb) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while chain.has_pending_sidecar_rebuilds() {
        assert!(
            Instant::now() < deadline,
            "background rebuild never finished"
        );
        std::thread::sleep(Duration::from_millis(10));
        chain.poll_vector_backend_upgrades();
    }
}

/// A failed embed leaves the sidecar stale. The next append must not embed the
/// chain on the calling thread (that would run under the chain write lock);
/// the background worker repairs the gap and the sidecar ends complete.
#[test]
fn test_append_with_stale_sidecar_never_embeds_history_on_request_path() {
    let dir = tempdir().unwrap();
    let probe = Arc::new(Probe::default());
    let provider = ProbeProvider {
        inner: LocalTextEmbeddingProvider::new(),
        probe: Arc::clone(&probe),
    };
    let metadata = provider.metadata().clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "stale-append").unwrap();
    chain.manage_vector_sidecar(provider).unwrap();
    for n in 0..20 {
        chain
            .append("a", ThoughtType::Finding, &format!("history entry {n}"))
            .unwrap();
    }

    probe.fail_next.store(true, Ordering::SeqCst);
    chain
        .append("a", ThoughtType::Finding, "embed fails for this one")
        .expect("a derived embed failure must not fail the persisted append");

    let me = std::thread::current().id();
    let before = probe.inputs_embedded_on(me);
    chain
        .append("a", ThoughtType::Finding, "append against a stale sidecar")
        .unwrap();
    // The repair scheduled by the failed embed may finish and be adopted
    // before this append, in which case the append legitimately embeds its own
    // thought (1). While the rebuild is still pending it embeds nothing (0).
    // The old synchronous rebuild embedded all 22.
    let embedded = probe.inputs_embedded_on(me) - before;
    assert!(
        embedded <= 1,
        "the request thread must not embed the chain while the sidecar is stale (embedded {embedded})"
    );

    wait_for_adoption(&mut chain);
    chain.flush().unwrap();
    drop(chain);

    let reopened = MentisDb::open_with_key(dir.path(), "stale-append").unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), 22, "the vector gap was repaired");
    assert!(matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, &metadata)
            .unwrap(),
        VectorSidecarFreshness::Fresh
    ));
}

/// Count and head hash can both match while entries are missing; the
/// freshness check must notice the gap instead of calling it fresh.
#[test]
fn test_sidecar_with_count_and_head_but_missing_entries_is_not_fresh() {
    let provider = LocalTextEmbeddingProvider::new();
    let metadata = provider.metadata().clone();
    let vector = vec![0.5; metadata.dimension];
    let entries = vec![VectorSidecarEntry::new(Uuid::nil(), 0, "h0", vector)];
    let sidecar = VectorSidecar::build(
        "chain",
        metadata.clone(),
        2,
        Some("h1".to_owned()),
        Utc::now(),
        entries,
    )
    .unwrap();
    assert!(matches!(
        sidecar.freshness("chain", 2, Some("h1"), &metadata),
        VectorSidecarFreshness::StaleThoughtCount {
            expected: 2,
            actual: 1
        }
    ));
}
