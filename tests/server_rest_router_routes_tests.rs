#![cfg(feature = "server")]
//! `rest_router` is the public way to embed the REST API; it must serve every
//! route the daemon serves, because the daemon builds its router from the same
//! function.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::StorageAdapterKind;
use tower::util::ServiceExt;

/// Regression: `rest_router` once kept its own copy of the route table that had
/// drifted and lacked `/v1/chains/merge` and `/v1/vectors/rebuild`, so embedded
/// servers answered 404 where the daemon answered. An empty body is rejected
/// by the handler (4xx other than 404), which proves the route is mounted.
#[tokio::test]
async fn test_rest_router_mounts_merge_and_vector_rebuild_routes() {
    let dir = std::env::temp_dir().join(format!("mentisdb_rest_routes_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for route in ["/v1/chains/merge", "/v1/vectors/rebuild", "/v1/admin/flush"] {
        let router = rest_router(MentisDbServiceConfig::new(
            dir.clone(),
            "default",
            StorageAdapterKind::Binary,
        ));
        let response = router
            .oneshot(
                Request::post(route)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "{route} is not mounted"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
