#[path = "../benches/support/http_concurrency_support.rs"]
mod http_concurrency_support;

use http_concurrency_support::{
    baseline_path, compare_rows, load_report, save_report, HttpConcurrencyReport,
    HttpConcurrencyRow,
};

#[test]
fn baseline_path_is_mode_scoped_and_sanitized() {
    let path = baseline_path("team nightly/run", false);
    let rendered = path.display().to_string();
    assert!(rendered.contains("team-nightly-run-auto_flush_off.json"));
}

#[test]
fn compare_rows_matches_by_concurrency_and_reports_percent_deltas() {
    let baseline = vec![
        HttpConcurrencyRow {
            concurrent: 100,
            wall_ms: 10.0,
            req_per_sec: 1000.0,
            p50_ms: 1.0,
            p95_ms: 2.0,
            p99_ms: 3.0,
            errors: 0,
        },
        HttpConcurrencyRow {
            concurrent: 1000,
            wall_ms: 200.0,
            req_per_sec: 5000.0,
            p50_ms: 80.0,
            p95_ms: 120.0,
            p99_ms: 150.0,
            errors: 1,
        },
    ];
    let current = vec![
        HttpConcurrencyRow {
            concurrent: 1000,
            wall_ms: 100.0,
            req_per_sec: 6000.0,
            p50_ms: 60.0,
            p95_ms: 110.0,
            p99_ms: 120.0,
            errors: 0,
        },
        HttpConcurrencyRow {
            concurrent: 100,
            wall_ms: 12.5,
            req_per_sec: 900.0,
            p50_ms: 1.5,
            p95_ms: 2.5,
            p99_ms: 3.5,
            errors: 2,
        },
    ];

    let deltas = compare_rows(&baseline, &current);
    assert_eq!(deltas.len(), 2);

    let low = deltas.iter().find(|row| row.concurrent == 100).unwrap();
    assert!((low.wall_ms_pct - 25.0).abs() < 0.001);
    assert!((low.req_per_sec_pct + 10.0).abs() < 0.001);
    assert_eq!(low.errors_delta, 2);

    let high = deltas.iter().find(|row| row.concurrent == 1000).unwrap();
    assert!((high.wall_ms_pct + 50.0).abs() < 0.001);
    assert!((high.req_per_sec_pct - 20.0).abs() < 0.001);
    assert_eq!(high.errors_delta, -1);
}

#[test]
fn http_concurrency_report_round_trips_through_json() {
    let report = HttpConcurrencyReport {
        auto_flush: true,
        concurrency_levels: vec![100, 1000],
        write_rows: vec![HttpConcurrencyRow {
            concurrent: 100,
            wall_ms: 10.0,
            req_per_sec: 1000.0,
            p50_ms: 1.0,
            p95_ms: 2.0,
            p99_ms: 3.0,
            errors: 0,
        }],
        read_rows: vec![HttpConcurrencyRow {
            concurrent: 100,
            wall_ms: 5.0,
            req_per_sec: 2000.0,
            p50_ms: 0.5,
            p95_ms: 1.0,
            p99_ms: 1.5,
            errors: 0,
        }],
    };

    let encoded = serde_json::to_vec(&report).unwrap();
    let decoded: HttpConcurrencyReport = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, report);
}

#[test]
fn save_and_load_report_round_trip_via_filesystem() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("http-bench.json");
    let report = HttpConcurrencyReport {
        auto_flush: false,
        concurrency_levels: vec![1000],
        write_rows: vec![HttpConcurrencyRow {
            concurrent: 1000,
            wall_ms: 42.0,
            req_per_sec: 9000.0,
            p50_ms: 10.0,
            p95_ms: 20.0,
            p99_ms: 25.0,
            errors: 0,
        }],
        read_rows: Vec::new(),
    };

    save_report(&path, &report).unwrap();
    let loaded = load_report(&path).unwrap().unwrap();
    assert_eq!(loaded, report);
}
