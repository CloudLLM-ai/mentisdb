#![cfg(feature = "server")]
//! Cross-chain lineage against the public audience: a replacement written in a
//! *private* segment of another chain must not redirect, annotate or re-score a
//! public reader's view of the old public policy, on every path that resolves
//! lineage across chains (branch search with ancestors, federated search over
//! REST, library federated search) and in context bundles. An authorized
//! reader still gets the redirect.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::audience::Audience;
use mentisdb::lineage::cross_chain_successor;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::read_auth::{fresh_nonce, sign_request};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::{
    MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtQuery, ThoughtRelation,
    ThoughtRelationKind, ThoughtType,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tower::util::ServiceExt;
use uuid::Uuid;

const OLD_POLICY: &str = "Release policy: we deploy to production on Fridays after lunch.";
const NEW_POLICY: &str = "Ship changes only early in the week and never ahead of a weekend.";
const QUERY: &str = "release policy deploy to production on Fridays";
const WRITER_SEED: [u8; 32] = [61; 32];
/// Fields that would tell a public reader a private replacement exists.
const HINTS: [&str; 5] = [
    "successor_of",
    "successors",
    "current_version",
    "contested",
    "successor_of_chain",
];

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mentisdb_xlineage_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Make `chain` private, allow the writer key, and (when `replacement`) write
/// a signed private thought that supersedes `old` on chain "parent".
fn privatize(chain: &mut MentisDb, dir: &Path, old: Uuid, replacement: bool) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let key = SigningKey::from_bytes(&WRITER_SEED);
    apply(
        chain,
        &operator,
        &OperatorOp::SetMode {
            mode: AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    apply(
        chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "writer".into(),
            key_id: "w1".into(),
            public_key: to_hex(&key.verifying_key().to_bytes()),
        },
    )
    .unwrap();
    if replacement {
        let head = chain.thoughts().last().unwrap().hash.clone();
        let mut input = ThoughtInput::new(ThoughtType::Decision, NEW_POLICY).with_relations(vec![
            ThoughtRelation {
                kind: ThoughtRelationKind::Supersedes,
                target_id: old,
                chain_key: Some("parent".into()),
                valid_at: None,
                invalid_at: None,
            },
        ]);
        let payload = thought_payload_v2(chain.chain_key(), "writer", &head, &input);
        input.signing_key_id = Some("w1".into());
        input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
        chain.append_thought("writer", input).unwrap();
    }
}

struct World {
    dir: PathBuf,
    old: Uuid,
}

/// "parent" (public, old policy), "branch" (a branch of parent) and "sibling":
/// the last two are private and, when `replacement`, hold the superseding thought.
fn world(name: &str, replacement: bool) -> World {
    let dir = dir(name);
    let (old, _) = {
        let mut parent =
            MentisDb::open_with_key_and_storage_kind(&dir, "parent", StorageAdapterKind::Binary)
                .unwrap();
        let old = parent
            .append_thought("a", ThoughtInput::new(ThoughtType::Decision, OLD_POLICY))
            .unwrap()
            .id;
        parent.flush().unwrap();
        (old, ())
    };
    let mut branch = MentisDb::branch_from(&dir, "parent", old, "branch").unwrap();
    privatize(&mut branch, &dir, old, replacement);
    branch.flush().unwrap();
    let mut sibling =
        MentisDb::open_with_key_and_storage_kind(&dir, "sibling", StorageAdapterKind::Binary)
            .unwrap();
    sibling
        .append("a", ThoughtType::Insight, "sibling opening note")
        .unwrap();
    privatize(&mut sibling, &dir, old, replacement);
    sibling.flush().unwrap();
    World { dir, old }
}

fn router(world: &World) -> axum::Router {
    rest_router(MentisDbServiceConfig::new(
        world.dir.clone(),
        "parent",
        StorageAdapterKind::Binary,
    ))
}

async fn post(router: &axum::Router, uri: &str, body: Value, authorized: bool) -> Value {
    let text = body.to_string();
    let mut builder = Request::post(uri).header("content-type", "application/json");
    if authorized {
        builder = builder.header(
            "x-mentis-auth",
            sign_request(
                &WRITER_SEED,
                "writer",
                "w1",
                "POST",
                uri,
                text.as_bytes(),
                chrono::Utc::now().timestamp(),
                &fresh_nonce(),
            ),
        );
    }
    let mut request = builder.body(Body::from(text)).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            45000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

/// No hint of a private replacement anywhere in `value`, and no private text.
fn assert_unhinted(what: &str, value: &Value) {
    let text = value.to_string();
    for hint in HINTS {
        assert!(
            !text.contains(&format!("\"{hint}\"")),
            "{what}: `{hint}` leaked: {text}"
        );
    }
    assert!(
        !text.contains("early in the week"),
        "{what}: private text leaked: {text}"
    );
    assert!(
        !text.contains("\"superseded\""),
        "{what}: status leaked: {text}"
    );
}

/// `(content, chain_key, total score)` of every hit, for before/after comparison.
fn digest(value: &Value) -> Vec<(String, String, String)> {
    value["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["thought"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                h["chain_key"].as_str().unwrap_or_default().to_string(),
                format!("{:.6}", h["score"]["total"].as_f64().unwrap_or(0.0)),
            )
        })
        .collect()
}

#[tokio::test]
async fn public_branch_search_keeps_the_old_policy_and_changes_nothing() {
    let (with, without) = (world("branch_with", true), world("branch_without", false));
    let body = json!({"chain_key":"branch","text":QUERY,"limit":5});
    let (a, b) = (
        post(&router(&with), "/v1/ranked-search", body.clone(), false).await,
        post(&router(&without), "/v1/ranked-search", body.clone(), false).await,
    );
    assert_eq!(a["results"][0]["thought"]["content"], OLD_POLICY, "{a}");
    assert_unhinted("branch ranked", &a);
    // (the branch's own genesis text names a different parent id in each world,
    // so only the policy hit is compared score for score)
    let policy_only = |v: &Value| {
        digest(v)
            .into_iter()
            .filter(|(c, _, _)| !c.starts_with("Branch from"))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        policy_only(&a),
        policy_only(&b),
        "scores must not depend on the private replacement"
    );
    assert_eq!(a["total"], b["total"]);
    // The authorized reader is redirected to the replacement.
    let r = post(&router(&with), "/v1/ranked-search", body, true).await;
    assert_eq!(r["results"][0]["thought"]["content"], NEW_POLICY, "{r}");
}

#[tokio::test]
async fn public_federated_search_keeps_the_old_policy_with_and_without_include_invalidated() {
    let (with, without) = (world("fed_with", true), world("fed_without", false));
    for include in [false, true] {
        let body = json!({"chain_keys":["parent","sibling"],"text":QUERY,"limit":5,"include_invalidated":include});
        let a = post(&router(&with), "/v1/federated-search", body.clone(), false).await;
        let b = post(
            &router(&without),
            "/v1/federated-search",
            body.clone(),
            false,
        )
        .await;
        assert_eq!(
            a["results"][0]["thought"]["content"], OLD_POLICY,
            "include={include}: {a}"
        );
        assert_eq!(a["results"][0]["chain_key"], "parent");
        assert_unhinted(&format!("federated include={include}"), &a);
        assert_eq!(digest(&a), digest(&b), "include={include}");
        assert_eq!(a["total"], b["total"], "include={include}");
    }
    let body = json!({"chain_keys":["parent","sibling"],"text":QUERY,"limit":5});
    let r = post(&router(&with), "/v1/federated-search", body.clone(), true).await;
    assert_eq!(r["results"][0]["thought"]["content"], NEW_POLICY, "{r}");
    assert_eq!(r["results"][0]["chain_key"], "sibling");
    // The audit view for an authorized reader says what replaced the old policy.
    let audit = json!({"chain_keys":["parent","sibling"],"text":QUERY,"limit":5,"include_invalidated":true});
    let r = post(&router(&with), "/v1/federated-search", audit, true).await;
    let old = r["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["thought"]["content"] == OLD_POLICY)
        .expect("old kept");
    assert_eq!(old["thought"]["version_status"], "superseded");
}

#[tokio::test]
async fn context_bundles_do_not_redirect_or_annotate_for_a_public_reader() {
    let (with, without) = (world("bundle_with", true), world("bundle_without", false));
    for chain in ["parent", "branch"] {
        let body = json!({"chain_key":chain,"text":QUERY,"limit":5});
        let a = post(&router(&with), "/v1/context-bundles", body.clone(), false).await;
        let b = post(&router(&without), "/v1/context-bundles", body, false).await;
        assert_unhinted(&format!("bundles {chain}"), &a);
        assert_eq!(a["total_bundles"], b["total_bundles"], "{chain}");
        if chain == "parent" {
            assert_eq!(
                a["bundles"][0]["seed"]["thought"]["content"], OLD_POLICY,
                "{a}"
            );
            assert_eq!(
                a["bundles"][0]["seed"]["lexical_score"],
                b["bundles"][0]["seed"]["lexical_score"]
            );
        } else {
            // Bundles are per chain: the branch itself holds no match, and the
            // private replacement must not show up as one.
            assert_eq!(a["bundles"], b["bundles"], "{a}");
        }
    }
}

#[test]
fn library_federated_search_and_cross_chain_successor_respect_the_audience() {
    let w = world("library", true);
    let open = |key: &str| {
        MentisDb::open_with_key_and_storage_kind(&w.dir, key, StorageAdapterKind::Binary).unwrap()
    };
    let (parent, sibling) = (open("parent"), open("sibling"));
    let query = |audience| {
        RankedSearchQuery::new()
            .with_filter(ThoughtQuery::new().with_audience(audience))
            .with_text(QUERY)
    };
    let mut other = std::collections::BTreeMap::new();
    other.insert("sibling".to_string(), query(Audience::Public));
    let public = parent.query_federated(
        "parent",
        &[("sibling", &sibling)],
        &query(Audience::Public),
        &other,
        10,
        0,
    );
    assert_eq!(public.hits[0].thought.content, OLD_POLICY);
    assert_eq!(public.hits[0].chain_key, "parent");
    assert!(
        public.hits[0].version.is_plain(),
        "{:?}",
        public.hits[0].version
    );
    assert!(public.hits[0].version.successor_of.is_empty());
    // With `include_invalidated` nothing is annotated either.
    let audit = query(Audience::Public).with_include_invalidated(true);
    other.insert("sibling".to_string(), audit.clone());
    let audited = parent.query_federated("parent", &[("sibling", &sibling)], &audit, &other, 10, 0);
    assert!(audited.hits.iter().all(|h| h.version.is_plain()));
    // The edge itself only counts for an audience that may see its source.
    let public_view = [
        ("parent", &parent, Audience::Public),
        ("sibling", &sibling, Audience::Public),
    ];
    assert!(cross_chain_successor(&public_view, "parent", w.old, "a").is_none());
    let authorized_sibling = [
        ("parent", &parent, Audience::Public),
        ("sibling", &sibling, Audience::Authorized),
    ];
    assert_eq!(
        cross_chain_successor(&authorized_sibling, "parent", w.old, "a")
            .unwrap()
            .thought
            .content,
        NEW_POLICY
    );
    // And an authorized federated search redirects.
    other.insert("sibling".to_string(), query(Audience::Authorized));
    let redirected = parent.query_federated(
        "parent",
        &[("sibling", &sibling)],
        &query(Audience::Authorized),
        &other,
        10,
        0,
    );
    let seen: Vec<(&str, u64, &str)> = redirected
        .hits
        .iter()
        .map(|h| {
            (
                h.chain_key,
                h.thought.index,
                &h.thought.content[..30.min(h.thought.content.len())],
            )
        })
        .collect();
    // (vector similarity also surfaces the governance records for an authorized reader)
    assert!(
        redirected
            .hits
            .iter()
            .any(|h| h.thought.content == NEW_POLICY),
        "{seen:?}"
    );
    assert!(
        redirected
            .hits
            .iter()
            .all(|h| h.thought.content != OLD_POLICY),
        "{seen:?}"
    );
}
