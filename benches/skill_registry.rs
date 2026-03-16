//! Benchmarks for SkillRegistry upload throughput, search latency, and
//! delta reconstruction performance.
//!
//! Three benchmark groups cover the core skill-registry hot paths:
//!
//! - **`upload_throughput`**: first-version (Full) upload latency and delta
//!   upload latency for single skills, plus batch scenarios of 1, 10, and 50
//!   uploads.
//! - **`search_latency`**: text, tag, and list-all operations over a
//!   registry pre-populated with 100 skills across 10 tags.
//! - **`reconstruct_latency`**: read-skill reconstruction at chain depths of
//!   1, 5, 10, and 20 versions, measuring the cumulative patch-application cost.

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use mentisdb::{SkillFormat, SkillQuery, SkillRegistry, SkillUpload};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Markdown template for benchmark skills.
///
/// `{name}` and `{tag}` are replaced per-skill so each entry in the registry
/// has a distinct skill id while all share a structurally identical document.
const SKILL_TEMPLATE: &str = r#"---
schema_version: 1
name: {name}
description: Benchmark skill for registry performance testing
tags: [{tag}, bench-skill]
triggers: [benchmark, bench-{name}]
warnings: []
---

## Overview

This is a benchmark skill named `{name}`.  It contains enough text to be
representative of a real skill document without being excessively large.

## When to Use

Invoke this skill during benchmark runs to exercise the upload and search code
paths of [`SkillRegistry`].

## Steps

1. Populate the registry with this skill.
2. Run the criterion benchmark group.
3. Inspect `target/criterion` for HTML reports.
"#;

/// Alternate body appended to a skill on successive versions to produce
/// non-empty unified diffs.  The `{version}` placeholder is replaced so each
/// version hash differs.
const VERSION_DELTA_SUFFIX: &str =
    "\n## Version Note\n\nThis is version `{version}` of the benchmark skill.\n";

/// Create a minimal, unique Markdown skill document for skill `index`.
///
/// `tag_index` selects one of 10 tag buckets so that search-by-tag benchmarks
/// can target a subset of the 100-skill registry.
fn skill_markdown(index: usize, tag_index: usize) -> String {
    SKILL_TEMPLATE
        .replace("{name}", &format!("bench-skill-{index:04}"))
        .replace("{tag}", &format!("bench-tag-{tag_index:02}"))
}

/// Create a fresh, isolated [`SkillRegistry`] in a temporary directory.
///
/// Returns both the registry and the [`TempDir`] guard; the guard must remain
/// alive for the duration of the benchmark to prevent premature deletion.
fn temp_registry(label: &str) -> (SkillRegistry, TempDir) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("mentisdb-skill-bench-{label}-"))
        .tempdir()
        .expect("failed to create tempdir for skill benchmark");
    let registry =
        SkillRegistry::open(dir.path()).expect("failed to open SkillRegistry for benchmark");
    (registry, dir)
}

/// Populate `registry` with `count` skills, distributing them evenly across
/// 10 tag buckets.  Returns the skill id of the last uploaded skill.
fn populate_registry(registry: &mut SkillRegistry, count: usize) -> String {
    let mut last_id = String::new();
    for i in 0..count {
        let tag_index = i % 10;
        let content = skill_markdown(i, tag_index);
        let summary = registry
            .upload_skill(
                SkillUpload::new("bench-agent", SkillFormat::Markdown, &content)
                    .with_agent_identity(Some("Bench Agent"), None),
            )
            .expect("populate_registry: upload failed");
        last_id = summary.skill_id;
    }
    last_id
}

// ---------------------------------------------------------------------------
// Group 1 – upload_throughput
// ---------------------------------------------------------------------------

/// Benchmark uploading the first version (Full content path) of a single skill.
///
/// Each iteration starts with a fresh empty registry so the binary persistence
/// write for a single-entry file is measured without prior data.
pub fn bench_upload_first_version(c: &mut Criterion) {
    let mut group = c.benchmark_group("upload_throughput");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    group.bench_function("upload_first_version", |b| {
        b.iter_batched(
            || temp_registry("upload-first"),
            |(mut registry, _dir)| {
                let content = black_box(skill_markdown(0, 0));
                let summary = registry
                    .upload_skill(SkillUpload::new(
                        "bench-agent",
                        SkillFormat::Markdown,
                        &content,
                    ))
                    .expect("upload_first_version: upload failed");
                black_box(summary.skill_id);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark uploading a second version (Delta content path) of a skill.
///
/// Each iteration pre-seeds v0 in setup (outside the timing window), then
/// measures only the second upload which computes a unified diff, stores it
/// as a delta, rebuilds indexes, and persists the registry.
pub fn bench_upload_delta_version(c: &mut Criterion) {
    let mut group = c.benchmark_group("upload_throughput");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    group.bench_function("upload_delta_version", |b| {
        b.iter_batched(
            || {
                let (mut registry, dir) = temp_registry("upload-delta");
                let v0 = skill_markdown(0, 0);
                registry
                    .upload_skill(
                        SkillUpload::new("bench-agent", SkillFormat::Markdown, &v0)
                            .with_skill_id("bench-delta-skill"),
                    )
                    .expect("upload_delta_version setup: v0 upload failed");
                (registry, dir)
            },
            |(mut registry, _dir)| {
                // v1 appends a version note — produces a non-trivial diff.
                let v0 = skill_markdown(0, 0);
                let v1 = format!("{v0}{}", VERSION_DELTA_SUFFIX.replace("{version}", "1"));
                let summary = registry
                    .upload_skill(
                        SkillUpload::new("bench-agent", SkillFormat::Markdown, black_box(&v1))
                            .with_skill_id("bench-delta-skill"),
                    )
                    .expect("upload_delta_version: v1 upload failed");
                black_box(summary.skill_id);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark batch upload of 1, 10, and 50 skills into a fresh registry.
///
/// Reports throughput in skills per second, measuring the combined cost of
/// document parsing, delta computation (v1+ uses diff), index rebuilding, and
/// binary persistence for each upload.
pub fn bench_upload_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("upload_throughput");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    for n in [1u64, 10, 50] {
        group.throughput(Throughput::Elements(n));
        group.bench_with_input(BenchmarkId::new("upload_batch", n), &n, |b, &n| {
            b.iter_batched(
                || temp_registry("upload-batch"),
                |(mut registry, _dir)| {
                    for i in 0..n as usize {
                        let content = skill_markdown(i, i % 10);
                        registry
                            .upload_skill(SkillUpload::new(
                                "bench-agent",
                                SkillFormat::Markdown,
                                black_box(&content),
                            ))
                            .expect("upload_batch: upload failed");
                    }
                    black_box(registry.list_skills().len());
                },
                BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 2 – search_latency
// ---------------------------------------------------------------------------

/// Benchmark search operations over a registry pre-populated with 100 skills
/// across 10 tag buckets.
///
/// Three sub-benchmarks are included:
/// - `search_by_text`: free-text filter over all skill names/descriptions.
/// - `search_by_tag`: tag-index filter returning ~10 matching skills.
/// - `list_all`: `list_skills()` returning all 100 summaries.
///
/// The 100-skill registry is built once in setup; only the search call is
/// inside the timing window.
pub fn search_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_latency");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    // Build the shared registry once; keep `_dir` alive for the group.
    let (mut seed_registry, _dir) = temp_registry("search-latency");
    populate_registry(&mut seed_registry, 100);
    let registry = seed_registry;

    // Free-text search.
    group.bench_function("search_by_text", |b| {
        let q = SkillQuery {
            text: Some("benchmark".to_string()),
            ..Default::default()
        };
        b.iter(|| {
            let results = registry.search_skills(black_box(&q));
            black_box(results.len());
        });
    });

    // Tag filter — matches ~10 skills.
    group.bench_function("search_by_tag", |b| {
        let q = SkillQuery {
            tags_any: vec!["bench-tag-03".to_string()],
            ..Default::default()
        };
        b.iter(|| {
            let results = registry.search_skills(black_box(&q));
            black_box(results.len());
        });
    });

    // List all 100 summaries.
    group.bench_function("list_all", |b| {
        b.iter(|| {
            let summaries = registry.list_skills();
            black_box(summaries.len());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Group 3 – reconstruct_latency
// ---------------------------------------------------------------------------

/// Benchmark skill reconstruction at chain depths of 1, 5, 10, and 20 versions.
///
/// Each depth variant pre-seeds a registry with N sequential versions of the
/// same skill (v0 is Full, v1–vN are Deltas).  The benchmark then calls
/// `read_skill` which must apply the full patch chain to reconstruct the
/// latest content.  This measures cumulative patch-application cost as a
/// function of version depth.
pub fn reconstruct_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("reconstruct_latency");
    group.measurement_time(std::time::Duration::from_secs(10));
    group.warm_up_time(std::time::Duration::from_secs(3));

    for depth in [1usize, 5, 10, 20] {
        group.bench_with_input(
            BenchmarkId::new("reconstruct_depth", depth),
            &depth,
            |b, &depth| {
                // Build a registry with `depth` versions in setup.
                b.iter_batched(
                    || {
                        let (mut registry, dir) = temp_registry("reconstruct");
                        let skill_id = "bench-reconstruct-skill";
                        let base = skill_markdown(0, 0);
                        // v0 — Full.
                        registry
                            .upload_skill(
                                SkillUpload::new("bench-agent", SkillFormat::Markdown, &base)
                                    .with_skill_id(skill_id),
                            )
                            .expect("reconstruct setup: v0 upload failed");
                        // v1..vN-1 — Deltas.
                        for v in 1..depth {
                            let content = format!(
                                "{base}{}",
                                VERSION_DELTA_SUFFIX.replace("{version}", &v.to_string())
                            );
                            registry
                                .upload_skill(
                                    SkillUpload::new(
                                        "bench-agent",
                                        SkillFormat::Markdown,
                                        &content,
                                    )
                                    .with_skill_id(skill_id),
                                )
                                .expect("reconstruct setup: version upload failed");
                        }
                        (registry, dir, skill_id.to_string())
                    },
                    |(registry, _dir, skill_id)| {
                        let text = registry
                            .read_skill(black_box(&skill_id), None, SkillFormat::Markdown)
                            .expect("reconstruct_latency: read_skill failed");
                        black_box(text.len());
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion wiring
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_upload_first_version,
    bench_upload_delta_version,
    bench_upload_batch,
    search_latency,
    reconstruct_latency
);
criterion_main!(benches);
