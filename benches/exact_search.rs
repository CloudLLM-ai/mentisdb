//! Exact vector search latency and throughput at a large corpus.
//!
//! Harness-free (tail latency and queries per second are the question; see
//! `skills/rust-criterion-benchmarking.md`, gotcha 6). Builds an exact
//! [`mentisdb::search::VectorIndex`] of `MENTISDB_BENCH_CORPUS` (default 100000)
//! random 384-dimensional vectors drawn around 64 cluster centres, runs
//! `MENTISDB_BENCH_QUERIES` (default 300) top-10 queries on one thread and
//! prints p50 / p99 latency, queries per second, live heap and RSS. Only API
//! that exists before and after the flat vector store is used, so the same file
//! runs against either commit.
//!
//! ```sh
//! cargo bench --bench exact_search
//! MENTISDB_BENCH_CORPUS=20000 cargo bench --bench exact_search
//! ```

use mentisdb::search::{EmbeddingMetadata, VectorDocument, VectorIndex, VectorQuery};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const DIMENSION: usize = 384;

/// Counts live heap bytes so memory is compared exactly between commits.
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

fn heap_mib() -> f64 {
    LIVE_BYTES.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0)
}

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

struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    }
}

fn vector(centres: &[Vec<f32>], rng: &mut Lcg, noise: f32) -> Vec<f32> {
    let centre = &centres[(rng.unit() * 1000.0).abs() as usize % centres.len()];
    centre.iter().map(|c| c + rng.unit() * noise).collect()
}

fn main() {
    let corpus = env_usize("MENTISDB_BENCH_CORPUS", 100_000);
    let queries = env_usize("MENTISDB_BENCH_QUERIES", 300);
    let mut rng = Lcg(42);
    let centres: Vec<Vec<f32>> = (0..64)
        .map(|_| (0..DIMENSION).map(|_| rng.unit()).collect())
        .collect();
    let heap_before = heap_mib();
    let documents: Vec<VectorDocument> = (0..corpus)
        .map(|i| VectorDocument::new(format!("doc-{i:07}"), vector(&centres, &mut rng, 0.6)))
        .collect();
    let query_vectors: Vec<Vec<f32>> = (0..queries)
        .map(|_| vector(&centres, &mut rng, 0.6))
        .collect();
    let metadata = EmbeddingMetadata::new("bench", DIMENSION, "v1");
    let index = VectorIndex::from_documents(metadata, documents).expect("index");
    let heap_index = heap_mib() - heap_before;
    let mut latencies = Vec::with_capacity(queries);
    let started = Instant::now();
    let mut checksum = 0.0_f32;
    for vector in &query_vectors {
        let query = VectorQuery::new(vector.clone()).with_limit(10);
        let one = Instant::now();
        let hits = index.search(&query).expect("search");
        latencies.push(one.elapsed().as_secs_f64() * 1e3);
        checksum += hits.first().map_or(0.0, |hit| hit.score);
    }
    let total = started.elapsed().as_secs_f64();
    latencies.sort_by(f64::total_cmp);
    let pick = |fraction: f64| {
        latencies
            [((latencies.len() as f64 * fraction).ceil() as usize).clamp(1, latencies.len()) - 1]
    };
    println!("| exact search | corpus | queries | p50 ms | p99 ms | queries/s | index heap MiB | RSS MiB |");
    println!(
        "| exact | {corpus} | {queries} | {:.3} | {:.3} | {:.1} | {heap_index:.1} | {:.0} |   (checksum {checksum:.4})",
        pick(0.5),
        pick(0.99),
        queries as f64 / total,
        rss_mib()
    );
}
