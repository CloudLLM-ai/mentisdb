use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Persisted summary of one HTTP concurrency benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpConcurrencyReport {
    /// Whether the server used immediate per-append flushing.
    pub auto_flush: bool,
    /// Concurrency levels exercised during the run.
    pub concurrency_levels: Vec<usize>,
    /// Aggregated write-wave rows.
    pub write_rows: Vec<HttpConcurrencyRow>,
    /// Aggregated read-wave rows.
    pub read_rows: Vec<HttpConcurrencyRow>,
}

/// Serializable benchmark row used for persistence and comparisons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HttpConcurrencyRow {
    pub concurrent: usize,
    pub wall_ms: f64,
    pub req_per_sec: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub errors: usize,
}

/// Percentage deltas between a baseline row and the current run.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpConcurrencyRowDelta {
    pub concurrent: usize,
    pub wall_ms_pct: f64,
    pub req_per_sec_pct: f64,
    pub p50_ms_pct: f64,
    pub p95_ms_pct: f64,
    pub p99_ms_pct: f64,
    pub errors_delta: isize,
}

/// Return the persisted baseline path for one HTTP benchmark mode.
pub fn baseline_path(baseline_name: &str, auto_flush: bool) -> PathBuf {
    let sanitized = sanitize_baseline_name(baseline_name);
    let mode = if auto_flush {
        "auto_flush_on"
    } else {
        "auto_flush_off"
    };
    PathBuf::from("target")
        .join("http_concurrency")
        .join(format!("{sanitized}-{mode}.json"))
}

/// Load a persisted report if it exists.
pub fn load_report(path: &Path) -> io::Result<Option<HttpConcurrencyReport>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)?;
    let report = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse HTTP concurrency baseline: {error}"),
        )
    })?;
    Ok(Some(report))
}

/// Save the current report as the new baseline for subsequent runs.
pub fn save_report(path: &Path, report: &HttpConcurrencyReport) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| {
        io::Error::other(format!(
            "Failed to serialize HTTP concurrency baseline: {error}"
        ))
    })?;
    fs::write(path, bytes)
}

/// Compare the current rows against the baseline rows for matching concurrency levels.
pub fn compare_rows(
    baseline: &[HttpConcurrencyRow],
    current: &[HttpConcurrencyRow],
) -> Vec<HttpConcurrencyRowDelta> {
    let baseline_by_concurrency: BTreeMap<usize, &HttpConcurrencyRow> =
        baseline.iter().map(|row| (row.concurrent, row)).collect();

    current
        .iter()
        .filter_map(|row| {
            let previous = baseline_by_concurrency.get(&row.concurrent)?;
            Some(HttpConcurrencyRowDelta {
                concurrent: row.concurrent,
                wall_ms_pct: percent_change(previous.wall_ms, row.wall_ms),
                req_per_sec_pct: percent_change(previous.req_per_sec, row.req_per_sec),
                p50_ms_pct: percent_change(previous.p50_ms, row.p50_ms),
                p95_ms_pct: percent_change(previous.p95_ms, row.p95_ms),
                p99_ms_pct: percent_change(previous.p99_ms, row.p99_ms),
                errors_delta: row.errors as isize - previous.errors as isize,
            })
        })
        .collect()
}

fn percent_change(previous: f64, current: f64) -> f64 {
    if previous == 0.0 {
        return 0.0;
    }
    ((current - previous) / previous) * 100.0
}

fn sanitize_baseline_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect();
    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "latest".to_string()
    } else {
        trimmed.to_string()
    }
}
