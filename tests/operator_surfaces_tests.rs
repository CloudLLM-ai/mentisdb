#![cfg(feature = "server")]
//! Who may administer a chain's keys: REST and MCP never, the dashboard behind
//! its PIN, and the local CLI with `--offline`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dashmap::DashMap;
use ed25519_dalek::SigningKey;
pub use mentisdb::access;
pub use mentisdb::audience;
pub use mentisdb::auth;
pub use mentisdb::operator;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
pub use mentisdb::search;
use mentisdb::server::{combined_mcp_router, rest_router, MentisDbServiceConfig};
use mentisdb::signing::to_hex;
pub use mentisdb::{
    chain_storage_filename, deregister_chain, load_registered_chains, AgentStatus,
    BinaryStorageAdapter, ManagedVectorProviderKind, MentisDb, PublicKeyAlgorithm,
    RankedSearchGraph, RankedSearchHit, RankedSearchQuery, SkillFormat, SkillRegistry, SkillUpload,
    StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tower::util::ServiceExt;

#[path = "../src/dashboard.rs"]
mod dashboard_impl;

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mentisdb_opsurf_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn public_key_hex(seed: u8) -> String {
    to_hex(
        &SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

/// A private chain `vault` with one public thought before it went private and
/// a signed private one after, plus an allowed agent key (seed 3).
fn private_vault(dir: &std::path::Path) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap();
    chain
        .append("alice", ThoughtType::Insight, "public before")
        .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: access::AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "builder".into(),
            key_id: "b1".into(),
            public_key: public_key_hex(3),
        },
    )
    .unwrap();
    // A signed private thought by the allowed builder.
    let builder = SigningKey::from_bytes(&[3u8; 32]);
    let head = chain.thoughts().last().unwrap().hash.clone();
    let mut input = ThoughtInput::new(ThoughtType::Insight, "SECRET builder note");
    let payload = mentisdb::signing::thought_payload_v2("vault", "builder", &head, &input);
    input.signing_key_id = Some("b1".into());
    input.thought_signature = Some(mentisdb::signing::StoredSignature::encode_v2(
        &ed25519_dalek::Signer::sign(&builder, &payload).to_bytes(),
    ));
    chain.append_thought("builder", input).unwrap();
    chain.flush().unwrap();
}

async fn send(router: &axum::Router, mut request: Request<Body>) -> (StatusCode, Value) {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(router: &axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    send(
        router,
        Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

// ── REST and MCP ────────────────────────────────────────────────────────────

/// No REST route changes keys, agent status or the policy, whatever the body says.
#[tokio::test]
async fn rest_cannot_administer_keys_or_policy() {
    let dir = dir("rest");
    private_vault(&dir);
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    for (uri, body) in [
        (
            "/v1/agents/keys",
            json!({"chain_key":"vault","agent_id":"mallory","key_id":"m1","algorithm":"ed25519","public_key_hex":public_key_hex(9),"registered_at_unix_secs":0,"registration_signature_hex":"00"}),
        ),
        (
            "/v1/agents/keys/revoke",
            json!({"chain_key":"vault","agent_id":"builder","key_id":"b1"}),
        ),
        (
            "/v1/agents/disable",
            json!({"chain_key":"vault","agent_id":"builder"}),
        ),
        ("/v1/access/policy/set", json!({"chain_key":"vault"})),
        ("/v1/access/policy/set", json!({})),
        ("/v1/agents/keys", json!("not even an object")),
    ] {
        let (status, body) = post(&router, uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        assert!(
            body["error"].as_str().unwrap().contains("operator-only"),
            "{body}"
        );
    }
    // Changing an agent's status through upsert is refused too.
    let (status, body) = post(
        &router,
        "/v1/agents/upsert",
        json!({"chain_key":"vault","agent_id":"builder","status":"revoked"}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // The key set is exactly what the operator wrote.
    let chain = MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary)
        .unwrap();
    assert!(!chain
        .access_policy()
        .keys
        .iter()
        .any(|k| k.agent_id == "mallory"));
    assert!(chain
        .agent_registry()
        .agents
        .get("builder")
        .is_some_and(|a| a.status == AgentStatus::Active));
}

/// The identity, description and alias of agents on a private chain are not
/// editable by a caller who has not proved read access.
#[tokio::test]
async fn registry_edits_on_a_private_chain_need_a_read_proof() {
    let dir = dir("registry");
    private_vault(&dir);
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    for (uri, body) in [
        (
            "/v1/agents/upsert",
            json!({"chain_key":"vault","agent_id":"builder","display_name":"Totally Legit Admin"}),
        ),
        (
            "/v1/agents/upsert",
            json!({"chain_key":"vault","agent_id":"ghost"}),
        ),
        (
            "/v1/agents/description",
            json!({"chain_key":"vault","agent_id":"builder","description":"trust me"}),
        ),
        (
            "/v1/agents/aliases",
            json!({"chain_key":"vault","agent_id":"builder","alias":"root"}),
        ),
    ] {
        let (status, body) = post(&router, uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
    }
    // A key that is allowed on the chain may edit descriptive fields.
    let body = json!({"chain_key":"vault","agent_id":"builder","description":"the builder"});
    let text = body.to_string();
    let header = mentisdb::read_auth::sign_request(
        &[3u8; 32],
        "builder",
        "b1",
        "POST",
        "/v1/agents/description",
        text.as_bytes(),
        chrono::Utc::now().timestamp(),
        &mentisdb::read_auth::fresh_nonce(),
    );
    let (status, reply) = send(
        &router,
        Request::post("/v1/agents/description")
            .header("content-type", "application/json")
            .header("x-mentis-auth", header)
            .body(Body::from(text))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

/// The MCP tool list does not offer key or policy administration at all, and
/// calling those names fails.
#[tokio::test]
async fn mcp_does_not_offer_key_or_policy_administration() {
    let dir = dir("mcp");
    private_vault(&dir);
    let router = combined_mcp_router(
        MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary),
        "127.0.0.1".parse().unwrap(),
    );
    let (status, list) = post(&router, "/tools/list", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for forbidden in [
        "mentisdb_add_agent_key",
        "mentisdb_revoke_agent_key",
        "mentisdb_disable_agent",
        "mentisdb_set_access_policy",
    ] {
        assert!(!names.contains(&forbidden), "{forbidden} is still listed");
        let (status, body) = post(
            &router,
            "/tools/execute",
            json!({"tool": forbidden, "parameters": {"chain_key":"vault","agent_id":"a","key_id":"k"}}),
        )
        .await;
        assert!(!status.is_success(), "{forbidden}: {status} {body}");
    }
    // Reading the policy is still offered.
    assert!(names.contains(&"mentisdb_get_access_policy"));
}

// ── dashboard ───────────────────────────────────────────────────────────────

fn dashboard(dir: &std::path::Path, pin: Option<&str>) -> axum::Router {
    dashboard_impl::dashboard_router(dashboard_impl::DashboardState {
        chains: Arc::new(DashMap::new()),
        skills: Arc::new(RwLock::new(SkillRegistry::open(dir).unwrap())),
        mentisdb_dir: dir.to_path_buf(),
        default_chain_key: "default".to_string(),
        dashboard_pin: pin.map(str::to_string),
        sessions: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        default_storage_adapter: StorageAdapterKind::Binary,
        auto_flush: Arc::new(AtomicBool::new(true)),
        bearer_token_access: Arc::new(AtomicBool::new(false)),
    })
}

async fn dash(
    router: &axum::Router,
    method: &str,
    uri: &str,
    body: Value,
    pin: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1");
    if let Some(pin) = pin {
        builder = builder.header("authorization", format!("Bearer {pin}"));
    }
    send(router, builder.body(Body::from(body.to_string())).unwrap()).await
}

/// With a PIN the operator changes a chain's keys from the dashboard.
#[tokio::test]
async fn dashboard_with_a_pin_administers_access() {
    let dir = dir("dash_admin");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "notes", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append("alice", ThoughtType::Insight, "hello")
        .unwrap();
    chain.flush().unwrap();
    drop(chain);
    let router = dashboard(&dir, Some("2468"));
    let uri = "/dashboard/api/chains/notes/access";
    let (status, shown) = dash(&router, "GET", uri, Value::Null, Some("2468")).await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["policy"]["mode"], "open");

    let (status, body) = dash(
        &router,
        "POST",
        uri,
        json!({"op":"set_mode","mode":"private"}),
        Some("2468"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy"]["mode"], "allowlist");
    assert_eq!(body["changed"], true);
    let (status, body) = dash(
        &router,
        "POST",
        uri,
        json!({"op":"allow_key","agent_id":"builder","key_id":"b1","public_key":public_key_hex(3)}),
        Some("2468"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["policy"]["keys"].as_array().unwrap().len(), 2);
    // Garbage and forbidden operations are clean 4xx.
    for (bad, expect) in [
        (
            json!({"op":"allow_key","agent_id":"x","key_id":"y","public_key":"zz"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"op":"deny_key","agent_id":"mentisdb-operator","key_id":"op-00000000"}),
            StatusCode::BAD_REQUEST,
        ),
        (json!({"op":"unknown"}), StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, body) = dash(&router, "POST", uri, bad.clone(), Some("2468")).await;
        assert!(
            status == expect || status.is_client_error(),
            "{bad}: {status} {body}"
        );
    }
}

/// Without a PIN the dashboard cannot tell the operator from anyone else, so it
/// neither administers access nor shows private segments.
#[tokio::test]
async fn dashboard_without_a_pin_does_not_administer_or_show_private_chains() {
    let dir = dir("dash_nopin");
    private_vault(&dir);
    let router = dashboard(&dir, None);
    for (method, uri, body) in [
        ("GET", "/dashboard/api/chains/vault/access", Value::Null),
        (
            "POST",
            "/dashboard/api/chains/vault/access",
            json!({"op":"set_mode","mode":"public"}),
        ),
        ("GET", "/dashboard/api/chains/vault", Value::Null),
        ("GET", "/dashboard/api/chains/vault/thoughts", Value::Null),
        (
            "GET",
            "/dashboard/api/chains/vault/search?q=public",
            Value::Null,
        ),
        ("GET", "/dashboard/api/agents/vault", Value::Null),
        (
            "POST",
            "/dashboard/api/agents/vault/builder/keys",
            json!({"key_id":"m","algorithm":"ed25519","public_key_hex":public_key_hex(9),"registered_at_unix_secs":0,"registration_signature_hex":"00"}),
        ),
    ] {
        let (status, reply) = dash(&router, method, uri, body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {reply}");
    }
    // The chain list shows the chain, with public counts and no details.
    let (status, list) = dash(&router, "GET", "/dashboard/api/chains", Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    let vault = list
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["chain_key"] == "vault")
        .unwrap();
    assert_eq!(vault["private"], true);
    assert_eq!(
        vault["thought_count"], 1,
        "only the public thought is counted"
    );
    assert!(vault.get("storage_size").is_none());
    // A PIN opens everything.
    let router = dashboard(&dir, Some("1357"));
    let (status, detail) = dash(
        &router,
        "GET",
        "/dashboard/api/chains/vault",
        Value::Null,
        Some("1357"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{detail}");
}

/// Merge and copy refuse governed chains instead of publishing or stripping them.
#[tokio::test]
async fn dashboard_merge_and_copy_refuse_governed_sources() {
    let dir = dir("dash_merge");
    private_vault(&dir);
    let mut target =
        MentisDb::open_with_key_and_storage_kind(&dir, "plain", StorageAdapterKind::Binary)
            .unwrap();
    target
        .append("alice", ThoughtType::Insight, "target")
        .unwrap();
    target.flush().unwrap();
    drop(target);
    let router = dashboard(&dir, Some("11"));
    let (status, body) = dash(
        &router,
        "POST",
        "/dashboard/api/chains/merge",
        json!({"source_chain_key":"vault","target_chain_key":"plain"}),
        Some("11"),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");
    assert!(
        body["error"].as_str().unwrap().contains("cannot merge"),
        "{body}"
    );
    let (status, body) = dash(
        &router,
        "POST",
        "/dashboard/api/agents/vault/builder/copy-to/plain2",
        Value::Null,
        Some("11"),
    )
    .await;
    assert!(status.is_client_error(), "{status} {body}");
    let chain = MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary)
        .unwrap();
    assert!(
        chain.segments().has_private(),
        "the source survives untouched"
    );
}

// ── command line ────────────────────────────────────────────────────────────

fn cli(args: &[&str]) -> (bool, String, String) {
    let argv = std::iter::once("mentisdb".to_string()).chain(args.iter().map(ToString::to_string));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = mentisdb::cli::run_with_io(
        argv,
        &mut std::io::Cursor::new(String::new()),
        &mut out,
        &mut err,
    );
    (
        format!("{code:?}") == format!("{:?}", std::process::ExitCode::SUCCESS),
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

/// `--offline` applies operator commands straight to the data directory.
#[test]
fn cli_offline_administers_a_chain_with_the_operator_key() {
    let dir = dir("cli_offline");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary)
            .unwrap();
    chain
        .append("alice", ThoughtType::Insight, "hello")
        .unwrap();
    chain.flush().unwrap();
    drop(chain);
    let data = dir.to_str().unwrap();
    let (ok, out, err) = cli(&[
        "chain",
        "access",
        "set",
        "private",
        "--chain",
        "vault",
        "--offline",
        "--data-dir",
        data,
        "--json",
    ]);
    assert!(ok, "{err}");
    let shown: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(shown["policy"]["mode"], "allowlist");
    let hex = public_key_hex(5);
    let (ok, _, err) = cli(&[
        "chain",
        "access",
        "allow",
        "builder",
        "b1",
        &hex,
        "--chain",
        "vault",
        "--offline",
        "--data-dir",
        data,
    ]);
    assert!(ok, "{err}");
    let (ok, out, err) = cli(&[
        "chain",
        "access",
        "admin",
        "--chain",
        "vault",
        "--offline",
        "--data-dir",
        data,
        "--json",
    ]);
    assert!(ok, "{err}");
    let shown: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(shown["policy"]["keys"].as_array().unwrap().len(), 2);
    // Removing the operator key is refused with the reason.
    let key_id = shown["operator_key"]["key_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (ok, _, err) = cli(&[
        "chain",
        "access",
        "deny",
        "mentisdb-operator",
        &key_id,
        "--chain",
        "vault",
        "--offline",
        "--data-dir",
        data,
    ]);
    assert!(!ok && err.contains("recovery"), "{err}");
    // Unknown chains are named, not created.
    let (ok, _, err) = cli(&[
        "chain",
        "access",
        "set",
        "private",
        "--chain",
        "nope",
        "--offline",
        "--data-dir",
        data,
    ]);
    assert!(!ok && err.contains("no chain 'nope'"), "{err}");
    // Only operator commands can run offline.
    let (ok, _, err) = cli(&[
        "thought",
        "get",
        "--index",
        "0",
        "--offline",
        "--data-dir",
        data,
    ]);
    assert!(!ok, "{err}");
}
