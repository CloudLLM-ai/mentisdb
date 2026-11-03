#![cfg(feature = "server")]
//! `GET /v1/chains` must not reveal how much a private chain holds or where it
//! lives, including when the chain cannot be opened to compute the caller's
//! view, and a migration must not turn the registry's "private" flag off.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use mentisdb::migration::fixtures::{semantic_chain, write_fixture, FixtureFormat};
use mentisdb::migration::{run_chain_migration, MigrationOptions};
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::server::{rest_router, MentisDbServiceConfig};
use mentisdb::{
    chain_storage_filename, load_registered_chains, MentisDb, MentisDbRegistration,
    MentisDbRegistry, StorageAdapterKind, ThoughtType,
};
use serde_json::Value;
use tower::util::ServiceExt;

fn dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_list_mask_{name}_{}_{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A chain `vault`: two public thoughts, then the operator makes it an
/// allowlist chain and a third thought is appended (it is private).
fn private_vault(dir: &std::path::Path) {
    let operator = OperatorIdentity::load_or_create(dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(dir, "vault", StorageAdapterKind::Binary).unwrap();
    for text in ["public one", "public two"] {
        chain.append("alice", ThoughtType::Insight, text).unwrap();
    }
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: mentisdb::access::AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    chain.flush().unwrap();
}

async fn list(router: &axum::Router) -> Value {
    let mut request = Request::get("/v1/chains").body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn summary<'a>(listing: &'a Value, key: &str) -> &'a Value {
    listing["chains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|chain| chain["chain_key"] == key)
        .unwrap_or_else(|| panic!("{key} is listed: {listing}"))
}

/// Regression: a private chain that could not be opened (the error was
/// swallowed) was listed with the registry's stored totals and its absolute
/// storage path. It is now listed with only its public count, no agents and
/// no path.
#[tokio::test]
async fn an_unopenable_private_chain_is_listed_masked() {
    let dir = dir("unopenable");
    private_vault(&dir);
    let registered = load_registered_chains(&dir).unwrap().chains["vault"].clone();
    assert!(registered.has_private, "the registry flags the chain");
    assert!(
        registered.thought_count > registered.public_thought_count,
        "stored totals include private thoughts: {registered:?}"
    );

    // Make the chain impossible to open, as a damaged file would.
    let file = dir.join(chain_storage_filename("vault", StorageAdapterKind::Binary));
    std::fs::write(&file, b"this is not a chain file").unwrap();

    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    let listing = list(&router).await;
    let vault = summary(&listing, "vault");
    assert_eq!(vault["storage_location"], "", "{vault}");
    assert_eq!(vault["thought_count"], registered.public_thought_count);
    assert_eq!(vault["agent_count"], 0, "{vault}");
    assert_eq!(vault["derived_backlog"]["implicit_edges"], 0, "{vault}");
    let text = listing.to_string();
    assert!(
        !text.contains(&dir.display().to_string()),
        "no absolute path of the private chain: {text}"
    );
}

/// A public chain is listed with its registry totals and path as before, next
/// to a masked private one.
#[tokio::test]
async fn public_chains_keep_their_totals_and_path() {
    let dir = dir("public");
    private_vault(&dir);
    {
        let mut chain =
            MentisDb::open_with_key_and_storage_kind(&dir, "notes", StorageAdapterKind::Binary)
                .unwrap();
        chain
            .append("alice", ThoughtType::Insight, "plain")
            .unwrap();
        chain.flush().unwrap();
    }
    let router = rest_router(MentisDbServiceConfig::new(
        dir.clone(),
        "default",
        StorageAdapterKind::Binary,
    ));
    let listing = list(&router).await;
    let notes = summary(&listing, "notes");
    assert_eq!(notes["thought_count"], 1);
    assert!(
        notes["storage_location"]
            .as_str()
            .unwrap()
            .contains(&dir.display().to_string()),
        "{notes}"
    );
    // The openable private chain shows the public caller only the public view.
    let vault = summary(&listing, "vault");
    assert_eq!(vault["storage_location"], "");
    assert_eq!(
        vault["thought_count"],
        load_registered_chains(&dir).unwrap().chains["vault"].public_thought_count
    );
}

fn registration(has_private: bool, public: u64) -> MentisDbRegistration {
    let now = chrono::Utc::now();
    MentisDbRegistration {
        chain_key: "demo".into(),
        version: 1,
        storage_adapter: StorageAdapterKind::Binary,
        storage_location: String::new(),
        thought_count: 11,
        agent_count: 0,
        entity_type_count: 0,
        has_private,
        public_thought_count: public,
        created_at: now,
        updated_at: now,
    }
}

fn write_registry(dir: &std::path::Path, entry: Option<MentisDbRegistration>) {
    let mut registry = MentisDbRegistry {
        version: 3,
        ..Default::default()
    };
    if let Some(entry) = entry {
        registry.chains.insert("demo".into(), entry);
    }
    std::fs::write(
        dir.join("mentisdb-registry.json"),
        serde_json::to_vec(&registry).unwrap(),
    )
    .unwrap();
}

fn migrate_demo(dir: &std::path::Path) {
    let file = dir.join(chain_storage_filename("demo", StorageAdapterKind::Binary));
    write_fixture(&file, FixtureFormat::Schema1, &semantic_chain(11)).unwrap();
    run_chain_migration(dir, "demo", &MigrationOptions::default()).unwrap();
}

/// Regression: finalizing a migration wrote `has_private: false` and the full
/// thought count as the public count, so a private chain was listed unmasked
/// until something opened it. The registry's own flag is kept.
#[test]
fn migration_keeps_the_registry_private_flag() {
    let dir = dir("migrate_private");
    write_registry(&dir, Some(registration(true, 4)));
    migrate_demo(&dir);
    let entry = &load_registered_chains(&dir).unwrap().chains["demo"];
    assert!(entry.has_private, "{entry:?}");
    assert_eq!(entry.public_thought_count, 4, "{entry:?}");
    assert_eq!(entry.thought_count, 11);
}

/// A chain the registry flags as public stays public after a migration, with
/// every thought public.
#[test]
fn migration_keeps_a_public_chain_public() {
    let dir = dir("migrate_public");
    write_registry(&dir, Some(registration(false, 0)));
    migrate_demo(&dir);
    let entry = &load_registered_chains(&dir).unwrap().chains["demo"];
    assert!(!entry.has_private, "{entry:?}");
    assert_eq!(entry.public_thought_count, 11);
}

/// A chain the registry has never seen is assumed private until it is opened
/// (the open writes the real value), so a listing cannot expose its totals.
#[test]
fn migration_of_an_unregistered_chain_assumes_private_until_opened() {
    let dir = dir("migrate_unknown");
    write_registry(&dir, None);
    migrate_demo(&dir);
    let entry = &load_registered_chains(&dir).unwrap().chains["demo"];
    assert!(entry.has_private, "{entry:?}");
    assert_eq!(entry.public_thought_count, 0);

    // Opening the chain derives the truth from the chain itself.
    let chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "demo", StorageAdapterKind::Binary).unwrap();
    assert!(!chain.segments().has_private());
    drop(chain);
    let entry = &load_registered_chains(&dir).unwrap().chains["demo"];
    assert!(!entry.has_private, "{entry:?}");
    assert_eq!(entry.public_thought_count, 11);
}
