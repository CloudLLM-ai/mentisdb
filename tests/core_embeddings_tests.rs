//! Regression tests for issue #23: opening a chain must not embed its history
//! synchronously, rebuilds are single-flight and serialized, and
//! `MENTISDB_EMBEDDINGS` opt-outs are honoured.

use mentisdb::search::{
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector,
    LocalTextEmbeddingProvider,
};
use mentisdb::{EmbeddingsMode, MentisDb, ThoughtType};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};
use tempfile::tempdir;

/// Serializes tests: the sidecar rebuild worker is process-wide.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|error| error.into_inner())
}

/// Closed-by-default gate letting a test hold embedding work in flight.
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Gate {
    fn wait_open(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }
}

/// Shared observations made by [`CountingProvider`].
#[derive(Default)]
struct Probe {
    factory_calls: AtomicUsize,
    embedded_inputs: AtomicUsize,
    max_batch: AtomicUsize,
    active: AtomicUsize,
    max_active: AtomicUsize,
    threads: Mutex<Vec<ThreadId>>,
}

struct CountingProvider {
    metadata: EmbeddingMetadata,
    probe: Arc<Probe>,
    gate: Arc<Gate>,
}

impl EmbeddingProvider for CountingProvider {
    type Error = std::io::Error;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, Self::Error> {
        let active = self.probe.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.max_active.fetch_max(active, Ordering::SeqCst);
        self.probe
            .threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        self.gate.wait_open();
        self.probe
            .embedded_inputs
            .fetch_add(inputs.len(), Ordering::SeqCst);
        self.probe
            .max_batch
            .fetch_max(inputs.len(), Ordering::SeqCst);
        let dimension = self.metadata.dimension;
        let vectors = inputs
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let mut values = vec![0.0_f32; dimension];
                values[i % dimension] = 1.0;
                EmbeddingVector::new(values)
            })
            .collect();
        self.probe.active.fetch_sub(1, Ordering::SeqCst);
        Ok(vectors)
    }
}

fn fastembed_metadata() -> EmbeddingMetadata {
    EmbeddingMetadata::new(
        mentisdb::search::FASTEMBED_MINILM_MODEL_ID,
        mentisdb::search::FASTEMBED_MINILM_DIMENSION,
        mentisdb::search::FASTEMBED_MINILM_VERSION,
    )
}

fn factory(
    probe: &Arc<Probe>,
    gate: &Arc<Gate>,
) -> impl FnOnce() -> std::io::Result<CountingProvider> + Send + 'static {
    let probe = Arc::clone(probe);
    let gate = Arc::clone(gate);
    move || {
        probe.factory_calls.fetch_add(1, Ordering::SeqCst);
        Ok(CountingProvider {
            metadata: fastembed_metadata(),
            probe,
            gate,
        })
    }
}

fn chain_with(dir: &std::path::Path, key: &str, thoughts: usize) -> MentisDb {
    let mut chain = MentisDb::open_with_key(dir, key).unwrap();
    for i in 0..thoughts {
        chain
            .append(
                "agent",
                ThoughtType::Finding,
                &format!("thought number {i}"),
            )
            .unwrap();
    }
    chain.flush().unwrap();
    chain
}

fn wait_until_adopted(chain: &mut MentisDb) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while chain.has_pending_sidecar_rebuilds() {
        assert!(Instant::now() < deadline, "rebuild was never adopted");
        std::thread::sleep(Duration::from_millis(10));
        chain.poll_vector_backend_upgrades();
    }
}

/// Opening a chain with no sidecar must return without embedding on the
/// caller's thread or rewriting config to fastembed; the rebuild then runs
/// in bounded batches on the worker and is adopted.
#[test]
fn test_open_without_sidecar_defers_embedding_to_background_worker() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut chain = chain_with(dir.path(), "deferred", 100);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    let started = Instant::now();
    chain
        .apply_persisted_managed_vector_sidecars_with_fastembed_factory(
            EmbeddingsMode::Auto,
            factory(&probe, &gate),
        )
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "open blocked on embedding"
    );
    assert_eq!(probe.embedded_inputs.load(Ordering::SeqCst), 0);
    assert!(chain.managed_vector_sidecars().is_empty());
    assert!(chain.has_pending_sidecar_rebuilds());
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let body = std::fs::read(entry.path()).unwrap_or_default();
            let flipped = String::from_utf8_lossy(&body).contains("fastembed");
            assert!(
                !flipped || name.contains("fastembed"),
                "open rewrote {name} to enable fastembed"
            );
        }
    }

    gate.open();
    wait_until_adopted(&mut chain);
    assert!(
        !probe
            .threads
            .lock()
            .unwrap()
            .contains(&std::thread::current().id()),
        "embedding ran on the caller thread"
    );
    assert_eq!(probe.embedded_inputs.load(Ordering::SeqCst), 100);
    assert!(probe.max_batch.load(Ordering::SeqCst) <= 32);
    assert_eq!(chain.managed_vector_sidecars(), vec![fastembed_metadata()]);
    let sidecar = chain
        .load_vector_sidecar(&fastembed_metadata())
        .unwrap()
        .expect("sidecar persisted");
    assert_eq!(sidecar.entries.len(), 100);
}

/// A second request for the same sidecar while one is queued or running is
/// refused, so the model factory runs once.
#[test]
fn test_rebuild_is_single_flight_per_sidecar() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut chain = chain_with(dir.path(), "single", 3);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    assert!(chain
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap());
    assert!(!chain
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap());

    gate.open();
    wait_until_adopted(&mut chain);
    assert_eq!(probe.factory_calls.load(Ordering::SeqCst), 1);

    // Once finished the slot is free again.
    let gate = Arc::new(Gate::default());
    gate.open();
    assert!(chain
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap());
    wait_until_adopted(&mut chain);
}

/// Rebuilds for different chains share one worker and never overlap.
#[test]
fn test_rebuilds_across_chains_run_one_at_a_time() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut first = chain_with(dir.path(), "first", 40);
    let mut second = chain_with(dir.path(), "second", 40);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    assert!(first
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap());
    assert!(second
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap());
    gate.open();
    wait_until_adopted(&mut first);
    wait_until_adopted(&mut second);
    assert_eq!(probe.max_active.load(Ordering::SeqCst), 1);
    assert_eq!(probe.embedded_inputs.load(Ordering::SeqCst), 80);
}

/// Thoughts appended while the rebuild ran are embedded when it is adopted.
#[test]
fn test_adoption_catches_up_thoughts_appended_during_rebuild() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut chain = chain_with(dir.path(), "catchup", 5);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    chain
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap();
    chain
        .append("agent", ThoughtType::Finding, "appended mid rebuild")
        .unwrap();
    gate.open();
    wait_until_adopted(&mut chain);

    let sidecar = chain
        .load_vector_sidecar(&fastembed_metadata())
        .unwrap()
        .unwrap();
    assert_eq!(sidecar.entries.len(), 6);
    assert!(matches!(
        chain
            .vector_sidecar_freshness(&sidecar, &fastembed_metadata())
            .unwrap(),
        mentisdb::search::VectorSidecarFreshness::Fresh
    ));
}

/// `off` leaves no managed sidecars; `local-text` manages only the built-in
/// provider and never asks for the fastembed factory.
#[test]
fn test_embeddings_mode_opt_outs_are_respected() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut chain = chain_with(dir.path(), "optout", 3);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    chain
        .apply_persisted_managed_vector_sidecars_with_fastembed_factory(
            EmbeddingsMode::LocalText,
            factory(&probe, &gate),
        )
        .unwrap();
    let local = LocalTextEmbeddingProvider::new();
    assert_eq!(
        chain.managed_vector_sidecars(),
        vec![EmbeddingProvider::metadata(&local).clone()]
    );

    chain
        .apply_persisted_managed_vector_sidecars_with_fastembed_factory(
            EmbeddingsMode::Off,
            factory(&probe, &gate),
        )
        .unwrap();
    assert!(chain.managed_vector_sidecars().is_empty());
    assert!(!chain.has_pending_sidecar_rebuilds());
    assert_eq!(probe.factory_calls.load(Ordering::SeqCst), 0);
}

/// Mode names round-trip through `Display`/`FromStr` and bad input is rejected.
#[test]
fn test_embeddings_mode_parses_and_displays() {
    for mode in [
        EmbeddingsMode::Auto,
        EmbeddingsMode::Off,
        EmbeddingsMode::LocalText,
        EmbeddingsMode::FastEmbed,
    ] {
        assert_eq!(mode.to_string().parse::<EmbeddingsMode>().unwrap(), mode);
    }
    assert_eq!(
        " OFF ".parse::<EmbeddingsMode>().unwrap(),
        EmbeddingsMode::Off
    );
    assert!("sometimes".parse::<EmbeddingsMode>().is_err());
    assert_eq!(EmbeddingsMode::default(), EmbeddingsMode::Auto);
}

/// A rebuild that finishes more than `SIDECAR_ADOPT_CATCHUP_LIMIT` thoughts
/// behind the chain is not abandoned: adoption continues over several polls
/// until the sidecar is complete and fresh.
#[test]
fn test_adoption_beyond_catchup_limit_is_rescheduled_not_dropped() {
    let _serial = serial();
    let dir = tempdir().unwrap();
    let mut chain = chain_with(dir.path(), "bigdelta", 5);
    let probe = Arc::new(Probe::default());
    let gate = Arc::new(Gate::default());

    chain
        .schedule_vector_sidecar_rebuild(fastembed_metadata(), factory(&probe, &gate))
        .unwrap();
    for i in 0..300 {
        chain
            .append("agent", ThoughtType::Finding, &format!("mid rebuild {i}"))
            .unwrap();
    }
    gate.open();
    wait_until_adopted(&mut chain);

    let sidecar = chain
        .load_vector_sidecar(&fastembed_metadata())
        .unwrap()
        .unwrap();
    assert_eq!(sidecar.entries.len(), 305);
    assert!(matches!(
        chain
            .vector_sidecar_freshness(&sidecar, &fastembed_metadata())
            .unwrap(),
        mentisdb::search::VectorSidecarFreshness::Fresh
    ));
}
