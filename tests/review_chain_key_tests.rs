//! Chain-key normalization, branch conflicts and open single-flight failure
//! handling in the server.
#![cfg(feature = "server")]

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{rest_router, MentisDbService, MentisDbServiceConfig};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtType};
use serde_json::{json, Value};
use std::path::PathBuf;
use tower::util::ServiceExt;

struct Fixture {
    dir: PathBuf,
    config: MentisDbServiceConfig,
    scoped: String,
    global: String,
    thought_id: String,
}

fn fixture() -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_review_keys_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let thought_id = {
        let mut chain = MentisDb::open_with_key(&dir, "alpha").unwrap();
        let id = chain
            .append("agent", ThoughtType::Finding, "alpha thought")
            .unwrap()
            .id
            .to_string();
        chain.flush().unwrap();
        id
    };
    MentisDb::open_with_key(&dir, "beta").unwrap();
    let config = MentisDbServiceConfig::new(dir.clone(), "alpha", StorageAdapterKind::Binary)
        .with_bearer_token_access(true);
    let store = config.bearer_token_store.clone();
    let scoped = store
        .create("scoped", BearerTokenScope::chain("alpha").unwrap())
        .unwrap()
        .token;
    let global = store
        .create("admin", BearerTokenScope::Global)
        .unwrap()
        .token;
    Fixture {
        dir,
        config,
        scoped,
        global,
        thought_id,
    }
}

async fn post(app: &axum::Router, uri: &str, token: &str, body: Value) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49200,
        ))));
    app.clone().oneshot(request).await.unwrap().status()
}

fn chain_file(fx: &Fixture, key: &str) -> PathBuf {
    fx.dir.join(mentisdb::chain_storage_filename(
        key,
        StorageAdapterKind::Binary,
    ))
}

/// POST a `mentisdb_merge_chains` MCP call and return (status, body text).
async fn mcp_merge(fx: &Fixture, token: &str, source: &str, target: &str) -> (StatusCode, String) {
    let (router, _b) = mentisdb::server::standard_mcp_router(
        fx.config.clone(),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    );
    let mut request = Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::from(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"mentisdb_merge_chains","arguments":{
                    "source_chain_key": source, "target_chain_key": target}}})
            .to_string(),
        ))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            49201,
        ))));
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A scoped token cannot delete its own chain by merging into a padded
/// spelling of it: the padded key is normalized, the self-merge guard fires,
/// and the source chain and its file survive.
#[tokio::test]
async fn test_padded_merge_target_cannot_delete_scoped_chain() {
    let fx = fixture();
    for (token, target) in [
        (&fx.scoped, "alpha "),
        (&fx.scoped, " alpha"),
        (&fx.scoped, "\talpha"),
        (&fx.global, "alpha "),
    ] {
        let (_status, body) = mcp_merge(&fx, token, "alpha", target).await;
        assert!(
            !body.contains("thoughts_copied"),
            "target {target:?} must not merge: {body}"
        );
        assert!(
            chain_file(&fx, "alpha").exists(),
            "source file must survive"
        );
    }
    let reopened = MentisDb::open_with_key(&fx.dir, "alpha").unwrap();
    assert_eq!(reopened.thoughts().len(), 1);
}

/// Branch refuses padded keys that would land outside the token's scope and
/// never creates a chain file for a padded spelling.
#[tokio::test]
async fn test_padded_branch_key_does_not_create_out_of_scope_chain() {
    let fx = fixture();
    let app = rest_router(fx.config.clone());
    let status = post(
        &app,
        "/v1/chains/branch",
        &fx.scoped,
        json!({
            "source_chain_key": "alpha",
            "branch_thought_id": fx.thought_id,
            "branch_chain_key": " gamma",
        }),
    )
    .await;
    assert!(status.is_client_error(), "got {status}");
    assert!(!chain_file(&fx, " gamma").exists());
    assert!(!chain_file(&fx, "gamma").exists());
}

/// Branching onto an existing or already-branched key is a 409 and does not
/// replace the live handle.
#[tokio::test]
async fn test_branch_onto_existing_chain_returns_409() {
    let fx = fixture();
    let app = rest_router(fx.config.clone());
    let body = |key: &str| {
        json!({
            "source_chain_key": "alpha",
            "branch_thought_id": fx.thought_id,
            "branch_chain_key": key,
        })
    };
    assert_eq!(
        post(&app, "/v1/chains/branch", &fx.global, body("fresh")).await,
        StatusCode::OK
    );
    assert_eq!(
        post(&app, "/v1/chains/branch", &fx.global, body("fresh")).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&app, "/v1/chains/branch", &fx.global, body("beta")).await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        post(&app, "/v1/chains/branch", &fx.global, body("fresh ")).await,
        StatusCode::CONFLICT
    );
}

/// Concurrent first-openers of a chain whose open fails all fail, and once
/// the file is fixed every concurrent opener receives the same single handle.
#[tokio::test]
async fn test_concurrent_openers_after_failed_open_share_one_handle() {
    let fx = fixture();
    let service = MentisDbService::new(fx.config.clone());
    let path = chain_file(&fx, "alpha");
    let good = std::fs::read(&path).unwrap();
    let mut bad = good.clone();
    let first_len = u64::from_le_bytes(bad[0..8].try_into().unwrap()) as usize;
    // Make the (only) record's prefix point far past EOF with a decodable body.
    bad.extend_from_slice(&good);
    bad[8 + first_len..8 + first_len + 8].copy_from_slice(&9_000_000_u64.to_le_bytes());
    std::fs::write(&path, &bad).unwrap();

    let failures = futures_join(&service, 8).await;
    assert!(
        failures.iter().all(Result::is_err),
        "corrupt open must fail"
    );

    std::fs::write(&path, &good).unwrap();
    let handles: Vec<_> = futures_join(&service, 8)
        .await
        .into_iter()
        .map(|r| r.expect("open after repair"))
        .collect();
    assert!(handles
        .windows(2)
        .all(|w| std::sync::Arc::ptr_eq(&w[0], &w[1])));
}

async fn futures_join(
    service: &MentisDbService,
    n: usize,
) -> Vec<
    Result<std::sync::Arc<tokio::sync::RwLock<MentisDb>>, Box<dyn std::error::Error + Send + Sync>>,
> {
    let tasks = (0..n).map(|_| {
        let service = service.clone();
        tokio::spawn(async move {
            service
                .existing_chain_handle("alpha")
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.to_string().into() })
        })
    });
    let mut out = Vec::new();
    for task in tasks.collect::<Vec<_>>() {
        out.push(task.await.unwrap());
    }
    out
}
