#![cfg(feature = "server")]
//! The dashboard's "Generate key pair" flow on the agent page: the key pair is
//! made in the browser with WebCrypto, only the public key is registered, and
//! the private key is shown once.
//!
//! The pure JavaScript of the flow (PKCS#8 seed extraction, the registration
//! payload, the request body) sits between `BEGIN keygen-helpers` and `END
//! keygen-helpers` in `src/dashboard_static/index.html`. These tests extract that
//! block and run it under `node` (20 or newer, which has WebCrypto Ed25519),
//! then check its output with the Rust implementation, and post the body it
//! builds to the real dashboard router. They skip, loudly, when `node` is
//! missing or too old. The modal itself needs a browser; the static checks pin
//! its safety properties and the manual steps are in the changelog entry.

use std::path::{Path, PathBuf};
use std::process::Command;
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
pub use mentisdb::search;
use mentisdb::signing::{from_hex, key_registration_payload, to_hex};
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

const INDEX_HTML: &str = include_str!("../src/dashboard_static/index.html");
const PIN: &str = "2468";

fn helpers() -> &'static str {
    let start = INDEX_HTML
        .find("// BEGIN keygen-helpers")
        .expect("BEGIN marker");
    let end = INDEX_HTML
        .find("// END keygen-helpers")
        .expect("END marker");
    &INDEX_HTML[start..end]
}

/// The major version of `node`, or `None` when it is not installed.
fn node_major() -> Option<u32> {
    let out = Command::new("node").arg("--version").output().ok()?;
    String::from_utf8(out.stdout)
        .ok()?
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn node_usable() -> bool {
    let usable = node_major().is_some_and(|major| major >= 20);
    if !usable {
        eprintln!("SKIPPED: these tests need `node` 20 or newer on PATH");
    }
    usable
}

fn scratch(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_keygen_js_{label}_{}_{n}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the extracted helpers followed by `driver` (which must print one JSON line).
fn run_node(label: &str, driver: &str) -> Value {
    let dir = scratch(label);
    let script = dir.join("run.js");
    std::fs::write(
        &script,
        format!(
            "const {{ webcrypto }} = require('node:crypto');\n{}\n{driver}\n",
            helpers()
        ),
    )
    .unwrap();
    let out = Command::new("node").arg(&script).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)))
}

fn generate_in_node(chain: &str, agent: &str, key_id: &str) -> Value {
    run_node(
        "generate",
        &format!(
            "generateAgentKey(webcrypto.subtle, {chain:?}, {agent:?}, {key_id:?}, Math.floor(Date.now() / 1000)).then(g => console.log(JSON.stringify(g)));"
        ),
    )
}

fn public_of_seed(seed_hex: &str) -> String {
    let seed: [u8; 32] = from_hex(seed_hex).unwrap().try_into().unwrap();
    to_hex(&SigningKey::from_bytes(&seed).verifying_key().to_bytes())
}

/// The seed taken from the PKCS#8 export is the key's real seed (it derives the
/// exported public key), in the 64-hex format `--private-key-file` reads, and
/// the key file the page offers is accepted by the CLI loader.
#[test]
fn test_browser_seed_extraction_matches_the_public_key() {
    if !node_usable() {
        return;
    }
    for _ in 0..3 {
        let generated = generate_in_node("reg", "bot", "k1");
        let seed = generated["seedHex"].as_str().unwrap();
        assert_eq!(seed.len(), 64);
        assert!(seed
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(
            public_of_seed(seed),
            generated["publicHex"].as_str().unwrap()
        );
        let dir = scratch("seedfile");
        let file = dir.join("bot-k1.key");
        std::fs::write(&file, format!("{seed}\n")).unwrap();
        let secret = mentisdb::cli::remote::read_secret_key_file(file.to_str().unwrap()).unwrap();
        assert_eq!(to_hex(&secret.0), seed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A PKCS#8 blob that is not the standard 48-byte Ed25519 layout is rejected
/// instead of yielding a wrong seed.
#[test]
fn test_pkcs8_with_an_unexpected_layout_is_rejected() {
    if !node_usable() {
        return;
    }
    let result = run_node(
        "pkcs8",
        r#"
        const ok = new Uint8Array(48); ok.set(Buffer.from(ED25519_PKCS8_PREFIX, 'hex')); ok.fill(7, 16);
        const attempt = bytes => { try { ed25519SeedFromPkcs8(bytes); return 'accepted'; } catch (e) { return 'rejected'; } };
        const wrongPrefix = ok.slice(); wrongPrefix[0] ^= 1;
        console.log(JSON.stringify({
          good: Buffer.from(ed25519SeedFromPkcs8(ok)).toString('hex'),
          short: attempt(ok.slice(0, 47)),
          long: attempt(new Uint8Array(49)),
          wrongPrefix: attempt(wrongPrefix),
          empty: attempt(new Uint8Array(0)),
        }));
        "#,
    );
    assert_eq!(result["good"], "07".repeat(32));
    for case in ["short", "long", "wrongPrefix", "empty"] {
        assert_eq!(result[case], "rejected", "{case}");
    }
}

/// The JavaScript registration payload is byte-for-byte the one the server
/// verifies, including non-ASCII ids, and the proof the page signs verifies.
#[test]
fn test_registration_payload_and_proof_match_the_server() {
    if !node_usable() {
        return;
    }
    let public = [7u8; 32];
    let result = run_node(
        "payload",
        "const pub = new Uint8Array(32).fill(7); console.log(JSON.stringify({ ascii: Buffer.from(keyRegistrationPayload('reg', 'bot', 'k1', pub, 1700000000)).toString('hex'), unicode: Buffer.from(keyRegistrationPayload('cadena-\u{00f1}', '\u{1f916}', 'k\u{00e9}y', pub, -5)).toString('hex') }));",
    );
    assert_eq!(
        result["ascii"],
        to_hex(&key_registration_payload(
            "reg",
            "bot",
            "k1",
            &public,
            1_700_000_000
        ))
        .as_str()
    );
    assert_eq!(
        result["unicode"],
        to_hex(&key_registration_payload(
            "cadena-\u{00f1}",
            "\u{1f916}",
            "k\u{00e9}y",
            &public,
            -5
        ))
        .as_str()
    );

    let generated = generate_in_node("reg", "bot", "k1");
    let body = &generated["body"];
    let proof = mentisdb::access::KeyRegistrationProof {
        registered_at_unix_secs: body["registered_at_unix_secs"].as_i64().unwrap(),
        signature: from_hex(body["registration_signature_hex"].as_str().unwrap()).unwrap(),
    };
    let public = from_hex(body["public_key_hex"].as_str().unwrap()).unwrap();
    assert!(proof
        .verify("reg", "bot", "k1", &public, proof.registered_at_unix_secs)
        .is_ok());
    assert!(
        proof
            .verify(
                "reg",
                "bot",
                "other-id",
                &public,
                proof.registered_at_unix_secs
            )
            .is_err(),
        "the proof names the key id"
    );
}

/// A browser whose WebCrypto cannot generate Ed25519 keys is reported as
/// unsupported (so the page points at the CLI), not as a generic failure, and
/// no key material is returned.
#[test]
fn test_unsupported_webcrypto_is_reported_distinctly() {
    if !node_usable() {
        return;
    }
    let result = run_node(
        "unsupported",
        r#"
        const refusing = { generateKey: async () => { throw new DOMException('no', 'NotSupportedError'); } };
        generateAgentKey(refusing, 'reg', 'bot', 'k1', 1).then(
          () => console.log(JSON.stringify({ outcome: 'resolved' })),
          e => console.log(JSON.stringify({ outcome: 'rejected', unsupported: e.keygenUnsupported === true })));
        "#,
    );
    assert_eq!(result, json!({"outcome": "rejected", "unsupported": true}));
}

fn dashboard(dir: &Path) -> axum::Router {
    dashboard_with_pin(dir, Some(PIN))
}

fn dashboard_with_pin(dir: &Path, pin: Option<&str>) -> axum::Router {
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

async fn post_keys(router: &axum::Router, agent: &str, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(format!("/dashboard/api/agents/reg/{agent}/keys"))
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1")
        .header("authorization", format!("Bearer {PIN}"))
        .body(Body::from(body.to_string()))
        .unwrap();
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

/// The request the page builds (public key as hex, timestamp, proof) is
/// accepted by the registration endpoint the Add Key button uses, registers
/// exactly that public key, and an altered body is refused.
#[tokio::test]
async fn test_registration_endpoint_accepts_the_body_the_page_sends() {
    let dir = scratch("endpoint");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain.append("bot", ThoughtType::Insight, "hello").unwrap();
    chain.flush().unwrap();
    drop(chain);
    let router = dashboard(&dir);

    // Without node, the same body shape is built in Rust, so the contract with
    // the endpoint is still pinned.
    let (body, public) = if node_usable() {
        let generated = generate_in_node("reg", "bot", "page-1");
        let public = generated["publicHex"].as_str().unwrap().to_string();
        (generated["body"].clone(), public)
    } else {
        use ed25519_dalek::Signer;
        let key = SigningKey::from_bytes(&[9u8; 32]);
        let public = key.verifying_key().to_bytes();
        let now = chrono::Utc::now().timestamp();
        let signature = key.sign(&key_registration_payload(
            "reg", "bot", "page-1", &public, now,
        ));
        (
            json!({
                "key_id": "page-1",
                "algorithm": "ed25519",
                "public_key_hex": to_hex(&public),
                "registered_at_unix_secs": now,
                "registration_signature_hex": to_hex(&signature.to_bytes()),
            }),
            to_hex(&public),
        )
    };
    let (status, reply) = post_keys(&router, "bot", &body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let keys = reply["public_keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["key_id"], "page-1");
    assert_eq!(
        to_hex(
            &keys[0]["public_key_bytes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect::<Vec<_>>()
        ),
        public
    );

    // An altered body is refused: the proof names the key id.
    let mut other = body.clone();
    other["key_id"] = json!("page-2");
    let (status, _) = post_keys(&router, "bot", &other).await;
    assert!(
        status.is_client_error(),
        "a proof for another key id: {status}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Static pins on the page: the private key is revealed only after the public
/// key was registered, is never sent, stored or put through `innerHTML`, can
/// only be dismissed after the acknowledgement, and the download URL is revoked.
#[test]
fn test_page_reveals_the_private_key_only_after_registration() {
    let handler_start = INDEX_HTML
        .find("getElementById('gk-generate').addEventListener")
        .expect("generate handler");
    let handler = &INDEX_HTML[handler_start
        ..INDEX_HTML
            .find("wireKeyDelete(chainKey, agentId);\n\n    // ── Export")
            .unwrap()];
    let registered = handler.find("await api(").expect("registers through api()");
    let revealed = handler
        .find("showPrivateKeyOnce(")
        .expect("reveals the key");
    assert!(registered < revealed, "register first, then reveal");
    assert!(
        handler.find("seedHex").unwrap() > registered,
        "the seed is not touched before registration succeeded"
    );
    assert!(
        handler.contains("keygenUnsupported") && handler.contains("mentisdb agent key generate")
    );
    assert!(
        !handler.contains("body: JSON.stringify(generated)"),
        "only generated.body is sent"
    );

    let modal_start = INDEX_HTML.find("function showPrivateKeyOnce").unwrap();
    let modal = &INDEX_HTML[modal_start..INDEX_HTML.find("function refreshKeys").unwrap()];
    for forbidden in [
        "api(",
        "fetch(",
        "innerHTML",
        "localStorage",
        "sessionStorage",
        "console.",
    ] {
        assert!(
            !modal.contains(forbidden),
            "the modal must not use {forbidden}"
        );
    }
    for required in [
        "This is the only time this private key will be shown. MentisDB keeps only the public key and cannot recover it. Save it somewhere safe now. Never paste it into a chat or give it to an AI agent.",
        "Download key file",
        "run `chmod 600 <file>` on macOS or Linux.",
        "I have saved this key",
        "setTimeout(() => URL.revokeObjectURL(url), 10000)",
        "e.key === 'Escape'",
        "if (saved.checked) close()",
        "secret = null",
        "overlay.remove()",
        "aria-modal",
    ] {
        assert!(modal.contains(required), "missing {required:?}");
    }
    // The page makes all requests through api(); the new flow added none.
    assert_eq!(INDEX_HTML.matches("fetch(").count(), 1);
}

/// Add Key accepts a pasted public key as bare hex, `0x` hex or 32 decimal
/// bytes, and refuses anything that is not exactly 32 bytes of valid input.
#[test]
fn test_add_key_input_forms_normalize_to_64_hex() {
    if !node_usable() {
        return;
    }
    let result = run_node(
        "inputs",
        r#"
        const bytes = Array.from({ length: 32 }, (_, i) => i * 8 + 1);
        const hex = bytes.map(b => b.toString(16).padStart(2, '0')).join('');
        const attempt = raw => { try { return publicKeyInputToHex(raw); } catch (e) { return 'error'; } };
        console.log(JSON.stringify({
          hex,
          bare: attempt(hex.toUpperCase()),
          prefixed: attempt('0x' + hex),
          spaced: attempt('0x' + hex.replace(/(..)/g, '$1 ')),
          decimal: attempt(bytes.join(', ')),
          short: attempt(hex.slice(2)),
          oddHex: attempt('0x' + hex.slice(1)),
          notHex: attempt('0x' + 'zz'.repeat(32)),
          big: attempt(bytes.slice(1).concat([256]).join(',')),
          empty: attempt(''),
        }));
        "#,
    );
    let hex = result["hex"].clone();
    for form in ["bare", "prefixed", "spaced", "decimal"] {
        assert_eq!(result[form], hex, "{form}");
    }
    for bad in ["short", "oddHex", "notHex", "big", "empty"] {
        assert_eq!(result[bad], "error", "{bad}");
    }
}

async fn post_access(
    router: &axum::Router,
    chain: &str,
    body: &Value,
    pin: Option<&str>,
) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/dashboard/api/chains/{chain}/access"))
        .header("content-type", "application/json")
        .header("x-mentisdb-dashboard", "1");
    if let Some(pin) = pin {
        request = request.header("authorization", format!("Bearer {pin}"));
    }
    router
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
        .status()
}

/// The request Add Key builds (the operator `register_key` operation, no
/// proof) is accepted behind the PIN and registers the pasted key; it is not
/// available without a session, nor on a dashboard that has no PIN.
#[tokio::test]
async fn test_add_key_body_is_accepted_only_for_the_pin_session() {
    let dir = scratch("addkey");
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    chain.append("bot", ThoughtType::Insight, "hello").unwrap();
    chain.flush().unwrap();
    drop(chain);
    let public = to_hex(
        &SigningKey::from_bytes(&[5u8; 32])
            .verifying_key()
            .to_bytes(),
    );
    let pasted = format!("0x{public}");
    // The page's own normalization produces the body field.
    let built = if node_usable() {
        run_node(
            "addkey_body",
            &format!(
                "console.log(JSON.stringify({{ public_key: publicKeyInputToHex({pasted:?}) }}));"
            ),
        )["public_key"]
            .as_str()
            .unwrap()
            .to_string()
    } else {
        public.clone()
    };
    assert_eq!(built, public);
    let body =
        json!({"op": "register_key", "agent_id": "bot", "key_id": "pasted-1", "public_key": built});

    let router = dashboard(&dir);
    assert!(
        !post_access(&router, "reg", &body, None).await.is_success(),
        "no session, no registration"
    );
    assert_eq!(
        post_access(&router, "reg", &body, Some(PIN)).await,
        StatusCode::OK
    );
    let registered =
        MentisDb::open_with_key_and_storage_kind(&dir, "reg", StorageAdapterKind::Binary).unwrap();
    let key = &registered.get_agent("bot").unwrap().public_keys[0];
    assert_eq!(key.key_id, "pasted-1");
    assert_eq!(to_hex(&key.public_key_bytes), public);

    let no_pin = dashboard_with_pin(&dir, None);
    let mut again = body.clone();
    again["key_id"] = json!("pasted-2");
    assert_eq!(
        post_access(&no_pin, "reg", &again, None).await,
        StatusCode::FORBIDDEN,
        "a dashboard without a PIN cannot tell the operator apart"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A refusal for a missing dashboard PIN is turned into an explanation that names
/// the fix; any other error text passes through unchanged.
#[test]
fn test_key_admin_errors_explain_a_missing_pin() {
    if !node_usable() {
        return;
    }
    let result = run_node(
        "adminerr",
        r#"console.log(JSON.stringify({
          pin: keyAdminErrorText('HTTP 403: {"error":"administering chain access needs a dashboard PIN: set MENTISDB_DASHBOARD_PIN"}'),
          other: keyAdminErrorText('HTTP 400: key id is already bound'),
        }));"#,
    );
    let pin = result["pin"].as_str().unwrap();
    assert!(
        pin.contains("has no PIN")
            && pin.contains("MENTISDB_DASHBOARD_PIN")
            && pin.contains("--offline"),
        "{pin}"
    );
    assert_eq!(result["other"], "HTTP 400: key id is already bound");
}
