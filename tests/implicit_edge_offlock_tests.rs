//! Implicit-edge neighbours are computed off the chain lock and must give the
//! overlay an inline scan per append would give.
//!
//! The oracle is the inline path (`with_inline_edge_scan_limit(usize::MAX)`,
//! the code that always ran under the write lock). A second chain takes the
//! same thoughts through deferred embedding, random out-of-order commits and
//! the background scan, with appends landing while a scan is held on a latch,
//! and the two overlays are compared entry by entry, scores bit for bit.
//! Nothing here depends on wall-clock time.

use mentisdb::search::{
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, ImplicitEdgeOverlay,
};
use mentisdb::{EmbedOutcome, MentisDb, ThoughtInput, ThoughtType};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tempfile::tempdir;

const DIMENSION: usize = 16;

/// Embeds `c=<cluster> v=<variant>` as the cluster centroid plus variant noise,
/// so thoughts of one cluster sit around the edge threshold and equal
/// (cluster, variant) pairs give identical vectors (equal scores: ties).
struct ClusterProvider {
    metadata: EmbeddingMetadata,
}

impl ClusterProvider {
    fn new() -> Self {
        Self {
            metadata: EmbeddingMetadata::new("cluster-test", DIMENSION, "v1"),
        }
    }
}

fn unit_noise(seed: u64, norm: f32) -> Vec<f32> {
    let mut state = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let raw: Vec<f32> = (0..DIMENSION)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5
        })
        .collect();
    let length = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    raw.iter().map(|v| v / length * norm).collect()
}

fn token(text: &str, key: &str) -> u64 {
    text.split_whitespace()
        .find_map(|word| word.strip_prefix(key))
        .and_then(|value| value.parse().ok())
        .expect("test thought carries its cluster and variant")
}

impl EmbeddingProvider for ClusterProvider {
    type Error = std::io::Error;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        Ok(inputs
            .iter()
            .map(|input| {
                let centroid = unit_noise(1000 + token(&input.text, "c="), 1.0);
                let noise = unit_noise(token(&input.text, "v="), 0.45);
                EmbeddingVector::new(centroid.iter().zip(&noise).map(|(c, n)| c + n).collect())
            })
            .collect())
    }
}

/// Tiny deterministic generator for the schedule.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

fn open_chain(dir: &std::path::Path, key: &str) -> MentisDb {
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    chain.manage_vector_sidecar(ClusterProvider::new()).unwrap();
    chain
}

fn text_for(rng: &mut Lcg) -> String {
    format!("c={} v={}", rng.next(4), rng.next(6))
}

/// An overlay as `(index) -> [(index, score bits)]`, comparable across chains
/// whose thought ids differ.
fn normalized(chain: &MentisDb) -> BTreeMap<u64, Vec<(u64, u32)>> {
    let by_id: BTreeMap<_, _> = chain
        .thoughts()
        .iter()
        .map(|thought| (thought.id, thought.index))
        .collect();
    let overlay: &ImplicitEdgeOverlay = chain.implicit_edge_overlay().expect("overlay present");
    overlay
        .edges
        .iter()
        .map(|(id, neighbours)| {
            (
                by_id[id],
                neighbours
                    .iter()
                    .map(|n| (by_id[&n.thought_id], n.cosine_score.to_bits()))
                    .collect(),
            )
        })
        .collect()
}

fn drain(chain: &mut MentisDb) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while chain.has_pending_sidecar_rebuilds() || chain.implicit_edge_backlog() > 0 {
        assert!(Instant::now() < deadline, "background work never finished");
        std::thread::sleep(Duration::from_millis(2));
        chain.poll_vector_backend_upgrades();
    }
}

fn deferred_append(chain: &mut MentisDb, content: &str) -> Vec<EmbedOutcome> {
    chain.defer_next_embedding();
    chain
        .append_thought("a", ThoughtInput::new(ThoughtType::Finding, content))
        .unwrap();
    chain
        .take_embed_tickets()
        .into_iter()
        .map(|ticket| ticket.embed())
        .collect()
}

/// A latch the scan thread waits on until the test opens it.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn hook(self: &Arc<Self>) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Arc::clone(self);
        Arc::new(move || {
            let mut open = gate.open.lock().unwrap();
            while !*open {
                open = gate.changed.wait(open).unwrap();
            }
        })
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

/// The off-lock overlay equals the inline one on a randomized corpus with
/// out-of-order commits, polls at random points, and appends that land while a
/// scan is held. 420 thoughts cross three chunk boundaries (128 per chunk), so
/// scans are split between the snapshot and the live delta.
#[test]
fn test_background_overlay_equals_the_inline_overlay_on_a_random_corpus() {
    let total = 420;
    let mut rng = Lcg(7);
    let contents: Vec<String> = (0..total).map(|_| text_for(&mut rng)).collect();

    let oracle_dir = tempdir().unwrap();
    let mut oracle = open_chain(oracle_dir.path(), "edges");
    oracle.with_inline_edge_scan_limit(usize::MAX);
    oracle
        .append("a", ThoughtType::Finding, &contents[0])
        .unwrap();
    oracle.ensure_implicit_edge_overlay();
    for content in &contents[1..] {
        oracle.append("a", ThoughtType::Finding, content).unwrap();
    }

    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "edges");
    chain.with_inline_edge_scan_limit(0);
    chain
        .append("a", ThoughtType::Finding, &contents[0])
        .unwrap();
    chain.ensure_implicit_edge_overlay();

    let mut held: Vec<Vec<EmbedOutcome>> = Vec::new();
    let mut gate: Option<Arc<Gate>> = None;
    for (position, content) in contents.iter().enumerate().skip(1) {
        // Sometimes hold the scan thread on a latch for a while.
        if gate.is_none() && position % 53 == 0 {
            let latch = Arc::new(Gate::default());
            chain.set_edge_scan_hook(Some(latch.hook()));
            gate = Some(latch);
        }
        let outcomes = deferred_append(&mut chain, content);
        if rng.next(3) == 0 {
            held.push(outcomes);
        } else {
            chain.commit_embeddings(outcomes);
        }
        if rng.next(5) == 0 && !held.is_empty() {
            let at = rng.next(held.len() as u64) as usize;
            chain.commit_embeddings(held.swap_remove(at));
        }
        if rng.next(4) == 0 {
            chain.poll_vector_backend_upgrades();
        }
        if gate.is_some() && position % 53 == 17 {
            gate.take().unwrap().open();
            chain.set_edge_scan_hook(None);
        }
    }
    if let Some(latch) = gate.take() {
        latch.open();
        chain.set_edge_scan_hook(None);
    }
    for outcomes in held {
        chain.commit_embeddings(outcomes);
    }
    drain(&mut chain);

    let expected = normalized(&oracle);
    assert!(
        expected.values().map(Vec::len).sum::<usize>() > 400,
        "the fixture must produce a rich overlay"
    );
    assert_eq!(normalized(&chain), expected);
    assert_eq!(chain.implicit_edge_backlog(), 0);
}

/// Appends that land while a scan is held are not lost and not applied twice:
/// the held scan finishes against its snapshot, the later vectors queue behind
/// it, and the result is the oracle's.
#[test]
fn test_vectors_applied_while_a_scan_runs_queue_behind_it() {
    let total = 200;
    let mut rng = Lcg(99);
    let contents: Vec<String> = (0..total).map(|_| text_for(&mut rng)).collect();

    let oracle_dir = tempdir().unwrap();
    let mut oracle = open_chain(oracle_dir.path(), "queue");
    oracle.with_inline_edge_scan_limit(usize::MAX);
    oracle
        .append("a", ThoughtType::Finding, &contents[0])
        .unwrap();
    oracle.ensure_implicit_edge_overlay();
    for content in &contents[1..] {
        oracle.append("a", ThoughtType::Finding, content).unwrap();
    }

    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "queue");
    chain.with_inline_edge_scan_limit(0);
    chain
        .append("a", ThoughtType::Finding, &contents[0])
        .unwrap();
    chain.ensure_implicit_edge_overlay();
    let latch = Arc::new(Gate::default());
    chain.set_edge_scan_hook(Some(latch.hook()));
    for content in &contents[1..] {
        let outcomes = deferred_append(&mut chain, content);
        chain.commit_embeddings(outcomes);
    }
    assert!(
        chain.implicit_edge_backlog() > 100,
        "nothing was applied while the scan was held ({})",
        chain.implicit_edge_backlog()
    );
    latch.open();
    chain.set_edge_scan_hook(None);
    drain(&mut chain);
    assert_eq!(normalized(&chain), normalized(&oracle));
}

/// A scan started for an overlay that is replaced meanwhile is dropped, not
/// applied on top of the new one.
#[test]
fn test_scan_for_a_replaced_overlay_is_discarded() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "replaced");
    chain.with_inline_edge_scan_limit(0);
    let mut rng = Lcg(3);
    chain
        .append("a", ThoughtType::Finding, &text_for(&mut rng))
        .unwrap();
    chain.ensure_implicit_edge_overlay();
    let latch = Arc::new(Gate::default());
    chain.set_edge_scan_hook(Some(latch.hook()));
    for _ in 0..20 {
        let outcomes = deferred_append(&mut chain, &text_for(&mut rng));
        chain.commit_embeddings(outcomes);
    }
    assert!(chain.implicit_edge_backlog() > 0);
    // Replace the overlay (as a repair would) while the scan is held.
    let path = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().contains("auto_edges"))
        .expect("overlay file");
    std::fs::remove_file(path).unwrap();
    chain.ensure_implicit_edge_overlay();
    assert_eq!(chain.implicit_edge_backlog(), 0, "the rebuild covers them");
    let rebuilt = normalized(&chain);
    latch.open();
    chain.set_edge_scan_hook(None);
    drain(&mut chain);
    assert_eq!(
        normalized(&chain),
        rebuilt,
        "the stale scan changed nothing"
    );
}

/// A scan thread that dies leaves the overlay to be rebuilt, never half
/// applied.
#[test]
fn test_failed_scan_drops_the_overlay_for_a_rebuild() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "failed");
    chain.with_inline_edge_scan_limit(0);
    let mut rng = Lcg(5);
    chain
        .append("a", ThoughtType::Finding, &text_for(&mut rng))
        .unwrap();
    chain.ensure_implicit_edge_overlay();
    chain.set_edge_scan_hook(Some(Arc::new(|| panic!("injected scan failure"))));
    let outcomes = deferred_append(&mut chain, &text_for(&mut rng));
    chain.commit_embeddings(outcomes);
    chain.set_edge_scan_hook(None);
    let deadline = Instant::now() + Duration::from_secs(30);
    while chain.implicit_edge_overlay().is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
        chain.poll_vector_backend_upgrades();
    }
    assert_eq!(chain.implicit_edge_backlog(), 0);
    chain.ensure_implicit_edge_overlay();
    assert!(
        chain.implicit_edge_overlay().is_some(),
        "repair rebuilds it"
    );
}

/// Small chains keep the old behaviour: the overlay is current when the commit
/// returns.
#[test]
fn test_small_chains_still_compute_edges_inline() {
    let dir = tempdir().unwrap();
    let mut chain = open_chain(dir.path(), "inline");
    let mut rng = Lcg(11);
    chain.append("a", ThoughtType::Finding, "c=0 v=0").unwrap();
    chain.ensure_implicit_edge_overlay();
    for _ in 0..30 {
        let outcomes = deferred_append(&mut chain, &text_for(&mut rng));
        chain.commit_embeddings(outcomes);
        assert_eq!(chain.implicit_edge_backlog(), 0);
    }
}

/// Regression: adopting a rebuilt sidecar re-serialized and SHA-256 hashed the
/// whole sidecar (`compact_to_path`) under the caller's lock to persist the few
/// thoughts appended during the rebuild. They now go to the WAL: the adopting
/// thread computes no full-corpus digest, and the files reload complete.
#[test]
fn test_adopting_a_rebuilt_sidecar_does_not_hash_the_corpus() {
    use mentisdb::search::full_sidecar_digests_on_this_thread;
    let dir = tempdir().unwrap();
    let provider = ClusterProvider::new();
    let metadata = provider.metadata().clone();
    let mut chain = open_chain(dir.path(), "adopt");
    let mut rng = Lcg(21);
    for _ in 0..40 {
        chain
            .append("a", ThoughtType::Finding, &text_for(&mut rng))
            .unwrap();
    }
    // The worker builds the provider: hold it there while thoughts are added,
    // so adoption has to embed them.
    let latch = Arc::new(Gate::default());
    let worker_latch = Arc::clone(&latch);
    assert!(chain
        .schedule_vector_sidecar_rebuild(metadata.clone(), move || {
            let mut open = worker_latch.open.lock().unwrap();
            while !*open {
                open = worker_latch.changed.wait(open).unwrap();
            }
            Ok(ClusterProvider::new())
        })
        .unwrap());
    for _ in 0..5 {
        chain
            .append("a", ThoughtType::Finding, &text_for(&mut rng))
            .unwrap();
    }
    latch.open();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !chain.has_finished_sidecar_rebuilds() {
        assert!(Instant::now() < deadline, "rebuild never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    let before = full_sidecar_digests_on_this_thread();
    chain.poll_vector_backend_upgrades();
    assert_eq!(
        full_sidecar_digests_on_this_thread(),
        before,
        "adoption hashed the whole sidecar"
    );
    assert!(!chain.has_pending_sidecar_rebuilds());
    chain.flush().unwrap();
    drop(chain);
    mentisdb::search::wait_for_sidecar_compaction(
        &MentisDb::open_with_key(dir.path(), "adopt")
            .unwrap()
            .vector_sidecar_path(&metadata)
            .unwrap(),
    );
    let reopened = MentisDb::open_with_key(dir.path(), "adopt").unwrap();
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), 45);
    assert!(matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, &metadata)
            .unwrap(),
        mentisdb::search::VectorSidecarFreshness::Fresh
    ));
}
