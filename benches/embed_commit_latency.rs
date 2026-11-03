//! Chain-lock exposure of the embedding commit path at a large corpus.
//!
//! A harness-free benchmark (tail latency is the question, which Criterion's
//! mean-oriented estimators do not answer; see `skills/rust-criterion-benchmarking.md`,
//! gotcha 6, and `http_concurrency.rs` for the same choice). It drives the
//! sequence a service runs for every append, timing the parts that need a chain
//! lock, against a chain that already holds `N` thoughts and a managed vector
//! sidecar of `N` vectors:
//!
//! 1. write lock: `append_thought` + `take_embed_tickets` (+ `commit_embeddings`
//!    after the untimed embedding);
//! 2. read lock: `flush_pending_derived`, which blocks the next writer.
//!
//! Two scenarios, each printed as a Markdown row:
//!
//! - **`compaction`**: `APPENDS` appends, enough to cross the sidecar WAL
//!   compaction threshold (`N / 16` WAL records) at least twice. Reports p50,
//!   p99, p99.9 and max of the write-lock hold and of the total lock exposure.
//!   Before the fix the compaction's full-corpus hash and clone ran inside the
//!   write-lock hold and its snapshot write inside the flush.
//! - **`straggler`**: one embedding is held back while `BEHIND` later appends
//!   commit, then released. Reports the worst single commit, the worst
//!   background-adoption stall (`poll_vector_backend_upgrades`, which runs under
//!   the write lock) and how many thoughts the provider embedded in total
//!   (`N + BEHIND + 1` when nothing is re-embedded). Before the fix the overflow
//!   re-embedded the whole corpus.
//!
//! The public API used here exists before and after the fix, so the same file
//! can be run against either commit.
//!
//! # Running
//!
//! ```sh
//! MENTISDB_HNSW_THRESHOLD=100000000 cargo bench --bench embed_commit_latency
//! ```
//!
//! `MENTISDB_HNSW_THRESHOLD` keeps the vector backend exact, so the HNSW graph
//! build does not compete for CPU. Environment overrides:
//! `MENTISDB_BENCH_CORPUS` (default 100000), `MENTISDB_BENCH_APPENDS`
//! (default `corpus / 8 + 200`), `MENTISDB_BENCH_BEHIND` (default 600),
//! `MENTISDB_BENCH_SCENARIOS` (comma list of `compaction`, `straggler`).

use mentisdb::search::{
    EmbeddingInput, EmbeddingMetadata, EmbeddingProvider, EmbeddingVector, VectorSidecarFreshness,
};
use mentisdb::{MentisDb, ThoughtInput, ThoughtType};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DIMENSION: usize = 384;

/// Counts live heap bytes so memory can be compared exactly between commits
/// (RSS is noisy: it includes allocator slack and freed-but-retained pages).
struct CountingAllocator;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: forwards every call to the system allocator unchanged and only adds
// atomic bookkeeping of the sizes it was asked for.
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(new_size, Ordering::Relaxed);
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Live heap in MiB.
fn heap_mib() -> f64 {
    LIVE_BYTES.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
}

/// Deterministic, cheap embeddings of a fixed dimension that count the
/// thoughts they embed.
struct CountingProvider {
    metadata: EmbeddingMetadata,
    embedded: Arc<AtomicUsize>,
}

impl CountingProvider {
    fn new(embedded: Arc<AtomicUsize>) -> Self {
        Self {
            metadata: EmbeddingMetadata::new("bench-384", DIMENSION, "v1"),
            embedded,
        }
    }
}

impl EmbeddingProvider for CountingProvider {
    type Error = io::Error;

    fn metadata(&self) -> &EmbeddingMetadata {
        &self.metadata
    }

    fn embed_batch(&self, inputs: &[EmbeddingInput]) -> Result<Vec<EmbeddingVector>, io::Error> {
        self.embedded.fetch_add(inputs.len(), Ordering::SeqCst);
        Ok(inputs
            .iter()
            .map(|input| {
                let mut state = input
                    .text
                    .bytes()
                    .fold(0xcbf29ce484222325_u64, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                    });
                let values = (0..DIMENSION)
                    .map(|_| {
                        state = state
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5
                    })
                    .collect();
                EmbeddingVector::new(values)
            })
            .collect())
    }
}

/// Resident set size of this process in MiB, from `ps` (0 if unavailable).
fn rss_mib() -> f64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
        .map_or(0.0, |kib| kib / 1024.0)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn text(n: u64) -> String {
    format!(
        "benchmark thought {n} about deployment rollback latency and cache {}",
        n % 97
    )
}

/// A chain of `corpus` thoughts with a fresh managed sidecar of the same size.
struct Fixture {
    chain: MentisDb,
    embedded: Arc<AtomicUsize>,
    metadata: EmbeddingMetadata,
    next: u64,
    _dir: TempDir,
}

fn build_fixture(corpus: usize) -> Fixture {
    let dir = tempfile::Builder::new()
        .prefix("mentisdb-embed-commit-bench-")
        .tempdir()
        .expect("tempdir");
    let embedded = Arc::new(AtomicUsize::new(0));
    let provider = CountingProvider::new(Arc::clone(&embedded));
    let metadata = provider.metadata().clone();
    let mut chain = MentisDb::open_with_key(dir.path(), "bench").expect("open chain");
    let started = Instant::now();
    for n in 0..corpus as u64 {
        chain
            .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, text(n)))
            .expect("append");
    }
    chain
        .manage_vector_sidecar(provider)
        .expect("manage sidecar");
    chain.flush().expect("flush");
    eprintln!(
        "fixture: {corpus} thoughts + sidecar in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    embedded.store(0, Ordering::SeqCst);
    Fixture {
        chain,
        embedded,
        metadata,
        next: corpus as u64,
        _dir: dir,
    }
}

/// Durations of one scenario, summarized as percentiles in milliseconds.
struct Samples(Vec<Duration>);

impl Samples {
    fn percentile(&self, fraction: f64) -> f64 {
        let mut sorted = self.0.clone();
        sorted.sort();
        let rank = ((sorted.len() as f64 * fraction).ceil() as usize).clamp(1, sorted.len());
        sorted[rank - 1].as_secs_f64() * 1e3
    }
}

/// One append with the lock-holding steps timed; the embedding runs between
/// them untimed, as it does in a service.
struct Cycle {
    write_hold: Duration,
    exposure: Duration,
}

fn deferred_cycle(fixture: &mut Fixture) -> Cycle {
    let n = fixture.next;
    fixture.next += 1;
    let started = Instant::now();
    fixture.chain.defer_next_derived_flush();
    fixture.chain.defer_next_embedding();
    fixture
        .chain
        .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, text(n)))
        .expect("append");
    let tickets = fixture.chain.take_embed_tickets();
    let first = started.elapsed();
    let outcomes: Vec<_> = tickets.into_iter().map(|ticket| ticket.embed()).collect();
    let commit_started = Instant::now();
    fixture.chain.commit_embeddings(outcomes);
    let write_hold = first + commit_started.elapsed();
    let flush_started = Instant::now();
    fixture.chain.flush_pending_derived().expect("flush");
    Cycle {
        write_hold,
        exposure: write_hold + flush_started.elapsed(),
    }
}

fn scenario_compaction(corpus: usize, appends: usize) {
    let mut fixture = build_fixture(corpus);
    let rss_after_fixture = rss_mib();
    let heap_after_fixture = heap_mib();
    let (mut holds, mut exposures) = (Vec::new(), Vec::new());
    for _ in 0..appends {
        let cycle = deferred_cycle(&mut fixture);
        holds.push(cycle.write_hold);
        exposures.push(cycle.exposure);
    }
    let rss_after_appends = rss_mib();
    let heap_after_appends = heap_mib();
    // Appends outrun the background implicit-edge scan; drain what it left and
    // report the scan's throughput at this corpus size.
    let backlog = fixture.chain.implicit_edge_backlog();
    let drain_started = Instant::now();
    while fixture.chain.implicit_edge_backlog() > 0 || fixture.chain.has_pending_sidecar_rebuilds()
    {
        std::thread::sleep(Duration::from_millis(5));
        fixture.chain.poll_vector_backend_upgrades();
    }
    let drain = drain_started.elapsed().as_secs_f64();
    println!(
        "| compaction | {corpus} | {appends} | memory MiB (RSS) after fixture / after appends | {:.0} / {:.0} |",
        rss_after_fixture, rss_after_appends
    );
    println!(
        "| compaction | {corpus} | {appends} | live heap MiB after fixture / after appends | {heap_after_fixture:.1} / {heap_after_appends:.1} |"
    );
    println!(
        "| compaction | {corpus} | {appends} | implicit-edge backlog left / drain s / scan vectors per s | {backlog} / {drain:.2} / {:.0} |",
        backlog as f64 / drain.max(1e-9)
    );
    let (holds, exposures) = (Samples(holds), Samples(exposures));
    println!(
        "| compaction | {corpus} | {appends} | write hold ms p50/p99/p99.9/max | {:.3} / {:.3} / {:.3} / {:.3} |",
        holds.percentile(0.50),
        holds.percentile(0.99),
        holds.percentile(0.999),
        holds.percentile(1.0),
    );
    println!(
        "| compaction | {corpus} | {appends} | lock exposure ms p50/p99/p99.9/max | {:.3} / {:.3} / {:.3} / {:.3} |",
        exposures.percentile(0.50),
        exposures.percentile(0.99),
        exposures.percentile(0.999),
        exposures.percentile(1.0),
    );
    finish_and_verify(fixture, corpus + appends);
}

fn scenario_straggler(corpus: usize, behind: usize) {
    let mut fixture = build_fixture(corpus);
    // The straggler: appended and embedded, but committed only at the end.
    let n = fixture.next;
    fixture.next += 1;
    fixture.chain.defer_next_embedding();
    fixture
        .chain
        .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, text(n)))
        .expect("append");
    let straggler: Vec<_> = fixture
        .chain
        .take_embed_tickets()
        .into_iter()
        .map(|ticket| ticket.embed())
        .collect();
    let mut worst_commit = Duration::ZERO;
    for _ in 0..behind {
        let n = fixture.next;
        fixture.next += 1;
        fixture.chain.defer_next_embedding();
        fixture
            .chain
            .append_thought("bench", ThoughtInput::new(ThoughtType::Finding, text(n)))
            .expect("append");
        let outcomes: Vec<_> = fixture
            .chain
            .take_embed_tickets()
            .into_iter()
            .map(|ticket| ticket.embed())
            .collect();
        let started = Instant::now();
        fixture.chain.commit_embeddings(outcomes);
        worst_commit = worst_commit.max(started.elapsed());
    }
    let started = Instant::now();
    fixture.chain.commit_embeddings(straggler);
    worst_commit = worst_commit.max(started.elapsed());
    let mut worst_poll = Duration::ZERO;
    let deadline = Instant::now() + Duration::from_secs(600);
    while fixture.chain.has_pending_sidecar_rebuilds() {
        assert!(Instant::now() < deadline, "background work never finished");
        std::thread::sleep(Duration::from_millis(5));
        let started = Instant::now();
        fixture.chain.poll_vector_backend_upgrades();
        worst_poll = worst_poll.max(started.elapsed());
    }
    let embedded = fixture.embedded.load(Ordering::SeqCst);
    let thoughts = corpus + behind + 1;
    println!(
        "| straggler | {corpus} | {behind} | worst commit ms / worst adoption stall ms / thoughts embedded (of {} new) | {:.3} / {:.3} / {embedded} |",
        behind + 1,
        worst_commit.as_secs_f64() * 1e3,
        worst_poll.as_secs_f64() * 1e3,
    );
    finish_and_verify(fixture, thoughts);
}

/// Flush, wait for background compaction, and check the sidecar on disk is
/// complete and fresh: speed must not cost correctness.
fn finish_and_verify(fixture: Fixture, thoughts: usize) {
    fixture.chain.flush().expect("flush");
    let metadata = fixture.metadata.clone();
    let dir = fixture._dir.path().to_path_buf();
    drop(fixture.chain);
    // A compaction thread may still be folding the WAL; give it time to finish
    // before reopening so the check sees the settled files.
    std::thread::sleep(Duration::from_millis(500));
    let reopened = MentisDb::open_with_key(&dir, "bench").expect("reopen");
    let sidecar = reopened
        .load_vector_sidecar(&metadata)
        .expect("load sidecar")
        .expect("sidecar present");
    let fresh = matches!(
        reopened
            .vector_sidecar_freshness(&sidecar, &metadata)
            .expect("freshness"),
        VectorSidecarFreshness::Fresh
    );
    println!(
        "|   verify | sidecar entries {} of {thoughts}, fresh: {fresh} | | | |",
        sidecar.entries.len()
    );
}

fn main() {
    let corpus = env_usize("MENTISDB_BENCH_CORPUS", 100_000);
    let appends = env_usize("MENTISDB_BENCH_APPENDS", corpus / 8 + 200);
    let behind = env_usize("MENTISDB_BENCH_BEHIND", 600);
    let scenarios = std::env::var("MENTISDB_BENCH_SCENARIOS")
        .unwrap_or_else(|_| "compaction,straggler".to_owned());
    println!("| scenario | corpus | appends | metric | value |");
    println!("|---|---|---|---|---|");
    for scenario in scenarios.split(',') {
        match scenario.trim() {
            "compaction" => scenario_compaction(corpus, appends),
            "straggler" => scenario_straggler(corpus, behind),
            other => eprintln!("unknown scenario {other:?}"),
        }
    }
}
