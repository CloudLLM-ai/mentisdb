//! HTTP concurrency benchmark for the MentisDB REST server.
//!
//! This harness-free benchmark starts a live `mentisdb` HTTP server **in-process**
//! on a dynamically assigned port (no external daemon required) and measures how
//! many concurrent tokio tasks the server can serve for both write and read paths.
//!
//! Two benchmark suites are run back-to-back at configurable concurrency levels:
//!
//! - **Write wave** — each task appends one thought via `POST /v1/thoughts`.
//! - **Read wave** — each task reads the chain head via `POST /v1/head`.
//!
//! By default this bench runs at **100 / 1 000** concurrent tasks so it remains
//! usable on typical developer machines. Override with
//! `MENTISDB_BENCH_CONCURRENCY`, for example:
//!
//! ```sh
//! MENTISDB_BENCH_CONCURRENCY=100,1000,10000 cargo bench --bench http_concurrency
//! ```
//!
//! Per-suite, the following metrics are reported:
//!
//! | metric        | description                                     |
//! |---------------|-------------------------------------------------|
//! | `wall_ms`     | total elapsed wall-clock time in milliseconds   |
//! | `req/s`       | throughput (N tasks / wall_ms × 1000)           |
//! | `p50_ms`      | median per-task round-trip latency              |
//! | `p95_ms`      | 95th-percentile per-task round-trip latency     |
//! | `p99_ms`      | 99th-percentile per-task round-trip latency     |
//! | `errors`      | number of tasks that received a non-2xx status  |
//!
//! # Running
//!
//! ```sh
//! cargo bench --bench http_concurrency
//! ```
//!
//! The binary prints two Markdown tables to stdout and exits with code 0 on
//! success, or 1 if the server failed to start.

#[path = "support/http_concurrency_support.rs"]
mod http_concurrency_support;

use http_concurrency_support::{
    baseline_path, compare_rows, load_report, save_report, HttpConcurrencyReport,
    HttpConcurrencyRow, HttpConcurrencyRowDelta,
};
use mentisdb::server::{start_servers, MentisDbServerConfig, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use reqwest::Client;
use serde_json::json;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::task::JoinSet;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Chain key used for all benchmark operations.
const CHAIN_KEY: &str = "bench";

/// Number of sequential warm-up appends performed before measurement begins.
const WARMUP_COUNT: usize = 10;

/// Default concurrency levels exercised in each wave.
///
/// Keep defaults moderate so local runs complete in practical time; use
/// `MENTISDB_BENCH_CONCURRENCY` to opt into larger stress levels.
const DEFAULT_CONCURRENCY_LEVELS: &[usize] = &[100, 1_000];
const CONCURRENCY_LEVELS_ENV: &str = "MENTISDB_BENCH_CONCURRENCY";
const AUTO_FLUSH_ENV: &str = "MENTISDB_BENCH_AUTO_FLUSH";
const BASELINE_ENV: &str = "MENTISDB_BENCH_BASELINE";

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Programme entry point.
///
/// Starts the server, runs warm-up, then runs write and read waves at each
/// configured concurrency level, and finally prints the result tables.
#[tokio::main]
async fn main() {
    let concurrency_levels = resolve_concurrency_levels();
    let auto_flush = resolve_auto_flush();
    let baseline_name = resolve_baseline_name();
    eprintln!("concurrency levels: {concurrency_levels:?}");
    eprintln!("auto_flush: {auto_flush}");

    // Keep TempDir alive for the entire benchmark so the chain files on disk
    // are not cleaned up before the server finishes.
    let temp_dir = TempDir::new().expect("failed to create temporary benchmark directory");

    // Build a server config that binds both MCP and REST to ephemeral OS-chosen
    // ports (port 0), eliminating any risk of collisions with running services.
    let config = MentisDbServerConfig {
        service: MentisDbServiceConfig::new(
            temp_dir.path().to_path_buf(),
            CHAIN_KEY,
            StorageAdapterKind::Binary,
        )
        .with_auto_flush(auto_flush),
        mcp_addr: "127.0.0.1:0"
            .parse()
            .expect("static address literal must parse"),
        rest_addr: "127.0.0.1:0"
            .parse()
            .expect("static address literal must parse"),
        https_mcp_addr: None,
        https_rest_addr: None,
        tls_cert_path: temp_dir.path().join("cert.pem"),
        tls_key_path: temp_dir.path().join("key.pem"),
        dashboard_addr: None,
        dashboard_pin: None,
    };

    let handles = start_servers(config)
        .await
        .expect("in-process mentisdb failed to start — cannot run HTTP concurrency benchmark");

    let rest_base = Arc::new(format!("http://{}", handles.rest.local_addr()));
    eprintln!("mentisdb REST listening at {rest_base}");

    let client = Arc::new(
        Client::builder()
            .pool_max_idle_per_host(512)
            .build()
            .expect("failed to build reqwest client"),
    );

    // Warm up: prime the chain so reads find at least some content.
    warmup(&client, &rest_base).await;

    // Write wave ---------------------------------------------------------------
    let mut write_rows: Vec<(usize, BenchRow)> = Vec::new();
    for &n in &concurrency_levels {
        eprintln!("write wave  n={n}…");
        let row = run_write_wave(Arc::clone(&client), Arc::clone(&rest_base), n).await;
        write_rows.push((n, row));
    }

    // Read wave ----------------------------------------------------------------
    let mut read_rows: Vec<(usize, BenchRow)> = Vec::new();
    for &n in &concurrency_levels {
        eprintln!("read wave   n={n}…");
        let row = run_read_wave(Arc::clone(&client), Arc::clone(&rest_base), n).await;
        read_rows.push((n, row));
    }

    // Output ------------------------------------------------------------------
    print_table("Write  —  POST /v1/thoughts", &write_rows);
    println!();
    print_table("Read   —  POST /v1/head", &read_rows);

    let report = HttpConcurrencyReport {
        auto_flush,
        concurrency_levels: concurrency_levels.clone(),
        write_rows: snapshot_rows(&write_rows),
        read_rows: snapshot_rows(&read_rows),
    };
    let baseline_file = baseline_path(&baseline_name, auto_flush);
    match load_report(&baseline_file) {
        Ok(Some(previous)) => {
            println!();
            print_delta_table(
                "Write delta vs previous run",
                &compare_rows(&previous.write_rows, &report.write_rows),
            );
            println!();
            print_delta_table(
                "Read delta vs previous run",
                &compare_rows(&previous.read_rows, &report.read_rows),
            );
        }
        Ok(None) => {
            eprintln!(
                "no previous HTTP concurrency baseline at {}; saving this run as the new baseline",
                baseline_file.display()
            );
        }
        Err(error) => {
            eprintln!(
                "failed to load previous HTTP concurrency baseline {}: {error}",
                baseline_file.display()
            );
        }
    }
    if let Err(error) = save_report(&baseline_file, &report) {
        eprintln!(
            "failed to save HTTP concurrency baseline {}: {error}",
            baseline_file.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Benchmark rows
// ---------------------------------------------------------------------------

/// Resolve concurrency levels from `MENTISDB_BENCH_CONCURRENCY` or fall back to
/// [`DEFAULT_CONCURRENCY_LEVELS`].
///
/// Expected env format is a comma-separated list of positive integers, such as
/// `"100,1000,10000"`. Invalid input falls back to defaults with a warning.
fn resolve_concurrency_levels() -> Vec<usize> {
    let Ok(raw) = env::var(CONCURRENCY_LEVELS_ENV) else {
        return DEFAULT_CONCURRENCY_LEVELS.to_vec();
    };
    let mut parsed = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let Ok(level) = token.parse::<usize>() else {
            eprintln!(
                "invalid {CONCURRENCY_LEVELS_ENV} value '{raw}', using defaults {DEFAULT_CONCURRENCY_LEVELS:?}"
            );
            return DEFAULT_CONCURRENCY_LEVELS.to_vec();
        };
        if level == 0 {
            eprintln!(
                "invalid {CONCURRENCY_LEVELS_ENV} value '{raw}', levels must be > 0; using defaults {DEFAULT_CONCURRENCY_LEVELS:?}"
            );
            return DEFAULT_CONCURRENCY_LEVELS.to_vec();
        }
        parsed.push(level);
    }
    if parsed.is_empty() {
        eprintln!(
            "{CONCURRENCY_LEVELS_ENV} was set but empty, using defaults {DEFAULT_CONCURRENCY_LEVELS:?}"
        );
        return DEFAULT_CONCURRENCY_LEVELS.to_vec();
    }
    parsed.sort_unstable();
    parsed.dedup();
    parsed
}

fn resolve_auto_flush() -> bool {
    env::var(AUTO_FLUSH_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
        .unwrap_or(true)
}

fn resolve_baseline_name() -> String {
    env::var(BASELINE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "latest".to_string())
}

/// Aggregated benchmark result for one concurrency level.
#[derive(Debug)]
struct BenchRow {
    /// Total elapsed wall-clock time for all N tasks to complete.
    wall_time: Duration,
    /// Requests per second: `n / wall_time_secs`.
    throughput_rps: f64,
    /// Median per-task round-trip latency.
    p50: Duration,
    /// 95th-percentile per-task round-trip latency.
    p95: Duration,
    /// 99th-percentile per-task round-trip latency.
    p99: Duration,
    /// Number of tasks that received a non-2xx HTTP response or encountered a
    /// transport error.
    errors: usize,
}

// ---------------------------------------------------------------------------
// Warm-up
// ---------------------------------------------------------------------------

/// Append [`WARMUP_COUNT`] sequential thoughts so the chain is not cold when
/// measurement begins.
///
/// Failures during warm-up are non-fatal — they are printed as warnings.
async fn warmup(client: &Client, base_url: &str) {
    eprintln!("warming up with {WARMUP_COUNT} sequential appends…");
    for i in 0..WARMUP_COUNT {
        let body = build_append_body(i);
        match client
            .post(format!("{base_url}/v1/thoughts"))
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => eprintln!("  warmup[{i}] non-2xx: {}", resp.status()),
            Err(err) => eprintln!("  warmup[{i}] error: {err}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Wave runners
// ---------------------------------------------------------------------------

/// Spawn `n` tasks concurrently, each appending one thought, and return
/// aggregated latency statistics.
async fn run_write_wave(client: Arc<Client>, base_url: Arc<String>, n: usize) -> BenchRow {
    let wall_start = Instant::now();
    let mut set: JoinSet<(Duration, bool)> = JoinSet::new();

    for i in 0..n {
        let c = Arc::clone(&client);
        let url = Arc::clone(&base_url);
        set.spawn(async move {
            let body = build_append_body(i);
            let expected_content = format!("bench thought {i}");
            let t0 = Instant::now();
            let resp = c
                .post(format!("{url}/v1/thoughts"))
                .json(&body)
                .send()
                .await;
            let ok = resp
                .as_ref()
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ok {
                if let Ok(resp) = resp.unwrap().json::<serde_json::Value>().await {
                    if let Some(thought) = resp.get("thought") {
                        if let Some(thought_id) = thought.get("id").and_then(|v| v.as_str()) {
                            let get_resp = c
                                .post(format!("{url}/v1/thought"))
                                .json(&serde_json::json!({
                                    "chain_key": CHAIN_KEY,
                                    "thought_id": thought_id
                                }))
                                .send()
                                .await;
                            if let Ok(get_resp) = get_resp {
                                if let Ok(thought_resp) = get_resp.json::<serde_json::Value>().await {
                                    if let Some(retrieved) = thought_resp.get("thought") {
                                        if let Some(content) = retrieved.get("content").and_then(|v| v.as_str()) {
                                            if content != expected_content {
                                                eprintln!("content mismatch: expected '{expected_content}', got '{content}'");
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            (t0.elapsed(), ok)
        });
    }

    collect_wave_results(set, wall_start, n).await
}

/// Spawn `n` tasks concurrently, each reading the chain head, and return
/// aggregated latency statistics.
async fn run_read_wave(client: Arc<Client>, base_url: Arc<String>, n: usize) -> BenchRow {
    let wall_start = Instant::now();
    let mut set: JoinSet<(Duration, bool)> = JoinSet::new();

    for _ in 0..n {
        let c = Arc::clone(&client);
        let url = Arc::clone(&base_url);
        set.spawn(async move {
            let body = json!({ "chain_key": CHAIN_KEY });
            let t0 = Instant::now();
            let ok = c
                .post(format!("{url}/v1/head"))
                .json(&body)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            (t0.elapsed(), ok)
        });
    }

    collect_wave_results(set, wall_start, n).await
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build the JSON body for an append request for task index `i`.
///
/// Uses a distinct `agent_id` per task to exercise the agent registry path.
fn build_append_body(i: usize) -> serde_json::Value {
    json!({
        "chain_key":    CHAIN_KEY,
        "agent_id":     format!("agent-{i}"),
        "thought_type": "Summary",
        "content":      format!("bench thought {i}"),
    })
}

/// Drain a [`JoinSet`] whose tasks each return `(per_task_duration, success)`,
/// compute percentiles, and return a [`BenchRow`].
///
/// # Arguments
///
/// * `set`         — running task set to drain.
/// * `wall_start`  — `Instant` captured before any tasks were spawned.
/// * `n`           — expected number of tasks (used only for throughput calc).
async fn collect_wave_results(
    mut set: JoinSet<(Duration, bool)>,
    wall_start: Instant,
    n: usize,
) -> BenchRow {
    let mut durations: Vec<Duration> = Vec::with_capacity(n);
    let mut errors: usize = 0;

    while let Some(result) = set.join_next().await {
        match result {
            Ok((dur, true)) => durations.push(dur),
            Ok((dur, false)) => {
                durations.push(dur);
                errors += 1;
            }
            Err(join_err) => {
                // Task panicked — count as error, push a zero duration so the
                // length stays consistent with `n`.
                eprintln!("task panicked: {join_err}");
                durations.push(Duration::ZERO);
                errors += 1;
            }
        }
    }

    let wall_time = wall_start.elapsed();
    let throughput_rps = n as f64 / wall_time.as_secs_f64();

    // Sort to allow index-based percentile extraction.
    durations.sort_unstable();

    let p50 = percentile(&durations, 0.50);
    let p95 = percentile(&durations, 0.95);
    let p99 = percentile(&durations, 0.99);

    BenchRow {
        wall_time,
        throughput_rps,
        p50,
        p95,
        p99,
        errors,
    }
}

/// Return the duration at the given fractional percentile of a **sorted** slice.
///
/// Returns [`Duration::ZERO`] when the slice is empty.
///
/// # Arguments
///
/// * `sorted` — slice sorted in ascending order.
/// * `pct`    — fractional percentile in `[0.0, 1.0]`.
fn percentile(sorted: &[Duration], pct: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    // Nearest-rank formula: index = ceil(pct * len) - 1, clamped to valid range.
    let idx = ((pct * sorted.len() as f64).ceil() as usize).saturating_sub(1);
    sorted[idx.min(sorted.len() - 1)]
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Print a Markdown table with results for all concurrency levels.
///
/// # Arguments
///
/// * `title` — table title printed as a Markdown heading above the table.
/// * `rows`  — slice of `(concurrency, BenchRow)` pairs.
fn print_table(title: &str, rows: &[(usize, BenchRow)]) {
    println!("## {title}");
    println!();
    println!(
        "| {:>10} | {:>10} | {:>10} | {:>10} | {:>10} | {:>10} | {:>8} |",
        "concurrent", "wall_ms", "req/s", "p50_ms", "p95_ms", "p99_ms", "errors"
    );
    println!(
        "|{:->12}|{:->12}|{:->12}|{:->12}|{:->12}|{:->12}|{:->10}|",
        "", "", "", "", "", "", ""
    );
    for (n, row) in rows {
        println!(
            "| {:>10} | {:>10.1} | {:>10.1} | {:>10.3} | {:>10.3} | {:>10.3} | {:>8} |",
            n,
            row.wall_time.as_secs_f64() * 1000.0,
            row.throughput_rps,
            row.p50.as_secs_f64() * 1000.0,
            row.p95.as_secs_f64() * 1000.0,
            row.p99.as_secs_f64() * 1000.0,
            row.errors,
        );
    }
}

fn snapshot_rows(rows: &[(usize, BenchRow)]) -> Vec<HttpConcurrencyRow> {
    rows.iter()
        .map(|(concurrent, row)| HttpConcurrencyRow {
            concurrent: *concurrent,
            wall_ms: row.wall_time.as_secs_f64() * 1000.0,
            req_per_sec: row.throughput_rps,
            p50_ms: row.p50.as_secs_f64() * 1000.0,
            p95_ms: row.p95.as_secs_f64() * 1000.0,
            p99_ms: row.p99.as_secs_f64() * 1000.0,
            errors: row.errors,
        })
        .collect()
}

fn print_delta_table(title: &str, rows: &[HttpConcurrencyRowDelta]) {
    println!("## {title}");
    println!();
    if rows.is_empty() {
        println!("No matching baseline rows were found for this run.");
        return;
    }
    println!(
        "| {:>10} | {:>12} | {:>12} | {:>12} | {:>12} | {:>12} | {:>12} |",
        "concurrent", "wall_ms %", "req/s %", "p50_ms %", "p95_ms %", "p99_ms %", "errors Δ"
    );
    println!(
        "|{:->12}|{:->14}|{:->14}|{:->14}|{:->14}|{:->14}|{:->14}|",
        "", "", "", "", "", "", ""
    );
    for row in rows {
        println!(
            "| {:>10} | {:>+11.1}% | {:>+11.1}% | {:>+11.1}% | {:>+11.1}% | {:>+11.1}% | {:>+12} |",
            row.concurrent,
            row.wall_ms_pct,
            row.req_per_sec_pct,
            row.p50_ms_pct,
            row.p95_ms_pct,
            row.p99_ms_pct,
            row.errors_delta,
        );
    }
}
