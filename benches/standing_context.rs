//! Benchmark for `MentisDb::standing_context` on a 10k-thought synthetic chain.
//!
//! One in four thoughts is a standing type, with restated near-duplicates, so
//! the benchmark covers candidate selection, dedupe and rendering.

use criterion::{criterion_group, criterion_main, Criterion};
use mentisdb::{BinaryStorageAdapter, MentisDb, StandingContextQuery, ThoughtInput, ThoughtType};
use std::hint::black_box;
use tempfile::TempDir;

fn populated_chain(count: usize) -> (MentisDb, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix("mentisdb-standing-bench-")
        .tempdir()
        .expect("failed to create tempdir for standing context benchmark");
    let adapter = BinaryStorageAdapter::for_chain_key(dir.path(), "standing-bench");
    let mut chain = MentisDb::open_with_storage(Box::new(adapter))
        .expect("failed to open chain for standing context benchmark");
    let types = [
        ThoughtType::Finding,
        ThoughtType::PreferenceUpdate,
        ThoughtType::Decision,
        ThoughtType::Constraint,
    ];
    for i in 0..count {
        let thought_type = types[i % types.len()];
        let content = format!(
            "synthetic note {i} about topic{} and detail{}",
            i % 97,
            i % 13
        );
        chain
            .append_thought(
                "bench",
                ThoughtInput::new(thought_type, content).with_importance(((i % 10) as f32) / 10.0),
            )
            .expect("failed to append benchmark thought");
    }
    (chain, dir)
}

pub fn standing_context_latency(c: &mut Criterion) {
    let (chain, _dir) = populated_chain(10_000);
    let defaults = StandingContextQuery::new();
    let wide = StandingContextQuery::new()
        .with_per_type_cap(50)
        .with_total_cap(200);
    let mut group = c.benchmark_group("standing_context");
    group.sample_size(20);
    group.bench_function("10k_default_caps", |b| {
        b.iter(|| black_box(chain.standing_context(black_box(&defaults))))
    });
    group.bench_function("10k_wide_caps", |b| {
        b.iter(|| black_box(chain.standing_context(black_box(&wide))))
    });
    group.finish();
}

criterion_group!(benches, standing_context_latency);
criterion_main!(benches);
