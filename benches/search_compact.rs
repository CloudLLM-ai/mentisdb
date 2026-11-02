//! Cost of `compact: true` on ranked and federated search.
//!
//! Seeds two chains through the in-process REST router (long content, typed
//! relations, concepts, agent registry fields), then times each search in the
//! full and the compact form and prints the response size of each, so the
//! bytes saved and any extra serialization time can be read side by side.
//!
//! ```sh
//! cargo bench --bench search_compact
//! ```

use axum::body::Body;
use axum::http::Request;
use criterion::{criterion_group, criterion_main, Criterion};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::hint::black_box;
use tokio::runtime::Runtime;
use tower::util::ServiceExt;

const TOPICS: [&str; 4] = [
    "quokka survey",
    "release checklist",
    "budget decision",
    "sensor calibration",
];

const PARAGRAPH: &str = "Counts were taken at dawn and dusk and cross-checked against \
the previous season's baseline. Two outliers were traced to a miscalibrated sensor and \
excluded. The decision stands unless the variance exceeds the agreed tolerance band, \
and owners were notified through the shared runbook.";

async fn post(router: &axum::Router, uri: &str, body: &Value) -> Vec<u8> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response");
    assert!(response.status().is_success(), "{uri} failed");
    axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
        .await
        .expect("body")
        .to_vec()
}

async fn seed(router: &axum::Router) {
    for chain in ["bench-a", "bench-b"] {
        let mut ids: Vec<String> = Vec::new();
        for n in 0..200usize {
            let topic = TOPICS[n % TOPICS.len()];
            let thought_type = ["Insight", "Decision", "Constraint", "LessonLearned"][n % 4];
            let mut body = json!({
                "chain_key": chain,
                "agent_id": "bench",
                "agent_name": "Benchmark Agent",
                "agent_owner": "bench-team",
                "thought_type": thought_type,
                "content": format!("{topic} {n}. {}", PARAGRAPH.repeat(1 + n % 4)),
                "tags": [topic.split(' ').next().unwrap_or("note"), "bench"],
                "concepts": [topic],
                "importance": 0.5,
                "confidence": 0.8,
            });
            if let Some(target) = ids.get(n.saturating_sub(3)) {
                let kind = if n % 5 == 0 {
                    "Supersedes"
                } else {
                    "References"
                };
                body["relations"] = json!([{"kind": kind, "target_id": target}]);
            }
            let appended: Value =
                serde_json::from_slice(&post(router, "/v1/thoughts", &body).await).expect("json");
            ids.push(
                appended["thought"]["id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }
}

fn bench_compact_search(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let dir = tempfile::Builder::new()
        .prefix("mentisdb-search-compact-bench-")
        .tempdir()
        .expect("tempdir");
    let router = rest_router(MentisDbServiceConfig::new(
        dir.path().to_path_buf(),
        "bench-a",
        StorageAdapterKind::Binary,
    ));
    rt.block_on(seed(&router));

    let cases = [
        (
            "ranked",
            "/v1/ranked-search",
            json!({"chain_key": "bench-a", "text": "budget decision", "limit": 10}),
        ),
        (
            "ranked_graph",
            "/v1/ranked-search",
            json!({"chain_key": "bench-a", "text": "sensor calibration", "limit": 10,
                   "graph": {"max_depth": 2}}),
        ),
        (
            "federated",
            "/v1/federated-search",
            json!({"chain_keys": ["bench-a", "bench-b"], "text": "quokka survey", "limit": 10}),
        ),
    ];

    let mut group = c.benchmark_group("search_compact");
    for (name, uri, body) in cases {
        let mut compact = body.clone();
        compact["compact"] = json!(true);
        let full_bytes = rt.block_on(post(&router, uri, &body)).len();
        let compact_bytes = rt.block_on(post(&router, uri, &compact)).len();
        println!(
            "{name}: full {full_bytes} bytes, compact {compact_bytes} bytes, {:.1}% smaller",
            100.0 * (1.0 - compact_bytes as f64 / full_bytes as f64)
        );
        group.bench_function(format!("{name}/full"), |b| {
            b.iter(|| black_box(rt.block_on(post(&router, uri, &body))))
        });
        group.bench_function(format!("{name}/compact"), |b| {
            b.iter(|| black_box(rt.block_on(post(&router, uri, &compact))))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_compact_search);
criterion_main!(benches);
