//! Web dashboard for the `mentisdbd` binary.
//!
//! This module exposes a self-contained HTML dashboard at `/dashboard` on a
//! configurable port (default 9475).  All static HTML is embedded via
//! `include_str!` so the binary has no runtime file-system dependency on
//! frontend assets.
//!
//! # Authentication
//!
//! When [`DashboardState::dashboard_pin`] is set, every request under
//! `/dashboard` (except the login page itself) is gated by a PIN check:
//!
//! - `Authorization: Bearer <pin>` HTTP header, **or**
//! - `mentisdb_pin=<pin>` browser cookie (set automatically after a
//!   successful `/dashboard/login` form POST).
//!
//! If neither is present the request is redirected to `/dashboard/login`.

use crate::{
    deregister_chain, load_registered_chains, AgentStatus, ManagedVectorProviderKind, MentisDb,
    PublicKeyAlgorithm, RankedSearchGraph, RankedSearchQuery, SkillFormat, SkillRegistry,
    SkillUpload, StorageAdapterKind, Thought, ThoughtInput, ThoughtQuery, ThoughtRelationKind,
    ThoughtRole, ThoughtType,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
    Form, Json, Router,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use uuid::Uuid;

// ── Embedded static HTML ──────────────────────────────────────────────────────

/// Main dashboard page HTML.
const DASHBOARD_HTML: &str = include_str!("dashboard_static/index.html");

/// Login page HTML (used only when a PIN is configured).
const LOGIN_HTML: &str = include_str!("dashboard_static/login.html");

// ── State ─────────────────────────────────────────────────────────────────────

/// Shared state threaded through every dashboard handler.
///
/// All fields wrap their data in `Arc` so cloning the state is cheap; the
/// clone is used by the PIN authentication middleware.
#[derive(Clone)]
pub(crate) struct DashboardState {
    /// Live chain map shared with the REST service.
    pub chains: Arc<DashMap<String, Arc<RwLock<MentisDb>>>>,
    /// Live skill registry shared with the REST service.
    pub skills: Arc<RwLock<SkillRegistry>>,
    /// On-disk directory where chain files are stored.
    pub mentisdb_dir: PathBuf,
    /// Default chain key resolved when none is specified.
    #[allow(dead_code)]
    pub default_chain_key: String,
    /// Optional PIN required to access the dashboard.
    pub dashboard_pin: Option<String>,
    /// Storage adapter kind used when opening chains from disk.
    pub default_storage_adapter: StorageAdapterKind,
    /// Whether newly opened chains should flush immediately on each append.
    #[allow(dead_code)]
    pub auto_flush: bool,
}

// ── Router builder ────────────────────────────────────────────────────────────

/// Build and return the complete dashboard [`Router`].
///
/// Routes under `/dashboard` and `/dashboard/api/**` are protected by the
/// PIN middleware when `state.dashboard_pin` is set.  The login endpoints
/// are always public so the user can authenticate.
pub(crate) fn dashboard_router(state: DashboardState) -> Router {
    // ── API sub-router ────────────────────────────────────────────────────
    let api = Router::new()
        // Chain listing
        .route("/chains", get(api_chains))
        .route("/chains", post(api_bootstrap_chain))
        .route(
            "/chains/{chain_key}",
            get(api_chain_detail).delete(api_delete_chain),
        )
        .route(
            "/chains/{chain_key}/vectors/{provider_key}/enable",
            post(api_enable_vector_sidecar),
        )
        .route(
            "/chains/{chain_key}/vectors/{provider_key}/disable",
            post(api_disable_vector_sidecar),
        )
        .route(
            "/chains/{chain_key}/vectors/{provider_key}/sync",
            post(api_sync_vector_sidecar),
        )
        .route(
            "/chains/{chain_key}/vectors/{provider_key}/rebuild",
            post(api_rebuild_vector_sidecar),
        )
        // Thoughts for a chain
        .route("/chains/{chain_key}/thoughts", get(api_chain_thoughts))
        .route("/chains/{chain_key}/search", get(api_chain_search))
        .route(
            "/chains/{chain_key}/search/bundles",
            get(api_chain_search_bundles),
        )
        .route(
            "/chains/{chain_key}/search/agents",
            get(api_chain_search_agents),
        )
        // Single thought lookup
        .route("/thoughts/{chain_key}/{thought_id}", get(api_get_thought))
        // Thoughts for an agent within a chain
        .route(
            "/chains/{chain_key}/agents/{agent_id}/thoughts",
            get(api_agent_thoughts),
        )
        // Agent listing — all chains
        .route("/agents", get(api_agents_all).post(api_create_agent))
        // Agent listing — single chain
        .route("/agents/{chain_key}", get(api_agents_by_chain))
        // Single-agent read + patch
        .route(
            "/agents/{chain_key}/{agent_id}",
            get(api_get_agent).patch(api_patch_agent),
        )
        // Agent lifecycle mutations
        .route(
            "/agents/{chain_key}/{agent_id}/revoke",
            post(api_revoke_agent),
        )
        .route(
            "/agents/{chain_key}/{agent_id}/activate",
            post(api_activate_agent),
        )
        // Agent key management
        .route(
            "/agents/{chain_key}/{agent_id}/keys",
            post(api_add_agent_key),
        )
        .route(
            "/agents/{chain_key}/{agent_id}/keys/{key_id}",
            delete(api_delete_agent_key),
        )
        // Agent memory export
        .route(
            "/agents/{chain_key}/{agent_id}/memory-markdown",
            get(api_agent_memory_markdown),
        )
        // Bulk import from MEMORY.md format
        .route(
            "/chains/{chain_key}/import-markdown",
            post(api_import_markdown),
        )
        // Copy agent memories to another chain
        .route(
            "/agents/{chain_key}/{agent_id}/copy-to/{target_chain_key}",
            post(api_copy_agent_to_chain),
        )
        // Merge all thoughts from a source chain into a target chain, then delete the source
        .route("/chains/merge", post(api_merge_chains))
        // Skill listing, reading, and uploading
        .route("/skills", get(api_skills).post(api_upload_skill))
        .route("/skills/{skill_id}", get(api_get_skill))
        .route("/skills/{skill_id}/versions", get(api_skill_versions))
        .route("/skills/{skill_id}/diff", get(api_skill_diff))
        .route("/skills/{skill_id}/revoke", post(api_revoke_skill))
        .route("/skills/{skill_id}/deprecate", post(api_deprecate_skill))
        // Version
        .route("/version", get(api_version));

    // ── Protected surface (PIN-gated when pin is set) ─────────────────────
    let protected = Router::new()
        .route("/dashboard", get(serve_dashboard))
        .route("/dashboard/", get(serve_dashboard))
        .nest("/dashboard/api", api)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            pin_auth_middleware,
        ));

    // ── Full dashboard router ─────────────────────────────────────────────
    Router::new()
        .merge(protected)
        .route("/dashboard/login", get(serve_login))
        .route("/dashboard/login", post(handle_login))
        .with_state(state)
}

// ── PIN authentication middleware ─────────────────────────────────────────────

/// Axum middleware that enforces the dashboard PIN.
///
/// Passes the request through unchanged when no PIN is configured.
/// When a PIN is set it accepts:
///
/// - `Authorization: Bearer <pin>` header
/// - `mentisdb_pin=<pin>` cookie
///
/// Any other request is redirected to `/dashboard/login`.
async fn pin_auth_middleware(
    State(state): State<DashboardState>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(required_pin) = &state.dashboard_pin else {
        // No PIN configured — open access.
        return next.run(request).await;
    };

    let headers = request.headers();

    // ── Check Authorization: Bearer <pin> header ──────────────────────────
    if let Some(auth_val) = headers.get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_val.to_str() {
            if let Some(provided) = auth_str.strip_prefix("Bearer ") {
                if provided == required_pin.as_str() {
                    return next.run(request).await;
                }
            }
        }
    }

    // ── Check mentisdb_pin cookie ─────────────────────────────────────────
    if let Some(cookie_val) = headers.get(header::COOKIE) {
        if let Ok(cookie_str) = cookie_val.to_str() {
            for part in cookie_str.split(';') {
                if let Some(pin) = part.trim().strip_prefix("mentisdb_pin=") {
                    if pin == required_pin.as_str() {
                        return next.run(request).await;
                    }
                }
            }
        }
    }

    // ── Neither matched — redirect to login ───────────────────────────────
    Redirect::to("/dashboard/login").into_response()
}

// ── Static HTML handlers ──────────────────────────────────────────────────────

/// Serve the main dashboard HTML.
async fn serve_dashboard() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DASHBOARD_HTML,
    )
}

/// Serve the login page HTML.
async fn serve_login() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        LOGIN_HTML,
    )
}

// ── Login POST handler ────────────────────────────────────────────────────────

/// Tracks failed login attempts per client IP: (attempt_count, first_attempt_time).
static RATE_LIMIT_MAP: std::sync::LazyLock<std::sync::Mutex<HashMap<String, (u32, Instant)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

const RATE_LIMIT_MAX_ATTEMPTS: u32 = 5;
const RATE_LIMIT_WINDOW_SECS: u64 = 300; // 5 minutes

/// Form body for the `/dashboard/login` POST.
#[derive(Deserialize)]
struct LoginForm {
    pin: String,
}

/// Handle a login form submission.
///
/// On success sets the `mentisdb_pin` cookie and redirects to `/dashboard`.
/// On failure redirects back to `/dashboard/login?error=1`.
///
/// Rate limiting is applied per source IP. Since the dashboard binds to localhost by
/// default, all local users share the same IP — the limit primarily protects against
/// automated brute-force attacks from the same machine.
async fn handle_login(
    State(state): State<DashboardState>,
    Form(form): Form<LoginForm>,
) -> Response {
    let ip_key = "localhost"; // Dashboard binds to 127.0.0.1; all clients share this key.

    // Check rate limit before PIN comparison.
    {
        let now = Instant::now();
        let map = RATE_LIMIT_MAP.lock().unwrap();
        if let Some(&(count, first_attempt)) = map.get(ip_key) {
            let elapsed = now.duration_since(first_attempt);
            if count >= RATE_LIMIT_MAX_ATTEMPTS && elapsed.as_secs() < RATE_LIMIT_WINDOW_SECS {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many failed attempts. Try again later.",
                )
                    .into_response();
            }
        }
        // Expire old entries outside the lock scope to avoid holding the lock too long.
        drop(map);
        let mut map = RATE_LIMIT_MAP.lock().unwrap();
        if let Some(&(_count, first_attempt)) = map.get(ip_key) {
            let elapsed = now.duration_since(first_attempt);
            if elapsed.as_secs() >= RATE_LIMIT_WINDOW_SECS {
                map.remove(ip_key);
            }
        }
    }

    let pin_matches = state
        .dashboard_pin
        .as_deref()
        .map(|required| {
            subtle::ConstantTimeEq::ct_eq(form.pin.as_bytes(), required.as_bytes()).into()
        })
        .unwrap_or(true); // No PIN configured → any submission succeeds.

    if pin_matches {
        RATE_LIMIT_MAP.lock().unwrap().remove(ip_key);
        (
            StatusCode::SEE_OTHER,
            [
                (
                    header::SET_COOKIE,
                    format!(
                        "mentisdb_pin={}; Path=/; HttpOnly; SameSite=Strict",
                        form.pin
                    ),
                ),
                (header::LOCATION, "/dashboard".to_string()),
            ],
            "",
        )
            .into_response()
    } else {
        let now = Instant::now();
        let mut map = RATE_LIMIT_MAP.lock().unwrap();
        let entry = map.entry(ip_key.to_string()).or_insert_with(|| (0, now));
        entry.0 += 1;
        entry.1 = now;
        Redirect::to("/dashboard/login?error=1").into_response()
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Build a `500 Internal Server Error` JSON response.
fn internal_error(err: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": err.to_string() })),
    )
}

/// Build a `404 Not Found` JSON response.
fn not_found(msg: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": msg.to_string() })),
    )
}

/// Return `true` when a cached chain has been deleted on disk and should no
/// longer be served from the dashboard cache.
async fn evict_deleted_cached_chain(
    state: &DashboardState,
    chain_key: &str,
    _arc: &Arc<RwLock<MentisDb>>,
) -> Result<bool, (StatusCode, Json<Value>)> {
    let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;
    if !registry.chains.contains_key(chain_key) {
        state.chains.remove(chain_key);
        return Ok(true);
    }
    Ok(false)
}

/// Look up a chain in the live cache; fall back to opening it from disk.
///
/// The opened chain is inserted into `state.chains` so subsequent requests
/// can reuse it without touching the file system.
async fn get_or_open_chain(
    state: &DashboardState,
    chain_key: &str,
) -> Result<Arc<RwLock<MentisDb>>, (StatusCode, Json<Value>)> {
    let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;
    let registered_storage = registry.chains.get(chain_key).map(|entry| {
        entry
            .storage_adapter
            .for_chain_key(&state.mentisdb_dir, chain_key)
    });

    // Try the live cache first (clone the Arc to avoid holding the DashMap shard lock across an await).
    if let Some(arc) = state.chains.get(chain_key).map(|r| r.value().clone()) {
        if state.auto_flush {
            if let Some(storage) = registered_storage {
                if let Ok(mut refreshed) = MentisDb::open_with_storage(storage) {
                    if refreshed.set_auto_flush(state.auto_flush).is_ok()
                        && refreshed.apply_persisted_managed_vector_sidecars().is_ok()
                    {
                        let refreshed = Arc::new(RwLock::new(refreshed));
                        state
                            .chains
                            .insert(chain_key.to_string(), refreshed.clone());
                        return Ok(refreshed);
                    }
                }
            }
        }
        if evict_deleted_cached_chain(state, chain_key, &arc).await? {
            return Err(not_found(format!("chain '{chain_key}' not found")));
        }
        return Ok(arc);
    }

    let Some(storage) = registered_storage else {
        return Err(not_found(format!("chain '{chain_key}' not found")));
    };

    let mut chain = MentisDb::open_with_storage(storage)
        .map_err(|e| not_found(format!("chain '{chain_key}': {e}")))?;
    chain
        .set_auto_flush(state.auto_flush)
        .map_err(internal_error)?;
    chain
        .apply_persisted_managed_vector_sidecars()
        .map_err(internal_error)?;

    let arc = Arc::new(RwLock::new(chain));
    state.chains.insert(chain_key.to_string(), arc.clone());
    Ok(arc)
}

/// Map a string token to a [`ThoughtType`] variant.
///
/// Returns `None` for any unrecognised name.
fn parse_thought_type(s: &str) -> Option<ThoughtType> {
    match s.trim() {
        "PreferenceUpdate" => Some(ThoughtType::PreferenceUpdate),
        "UserTrait" => Some(ThoughtType::UserTrait),
        "RelationshipUpdate" => Some(ThoughtType::RelationshipUpdate),
        "Finding" => Some(ThoughtType::Finding),
        "Insight" => Some(ThoughtType::Insight),
        "FactLearned" => Some(ThoughtType::FactLearned),
        "PatternDetected" => Some(ThoughtType::PatternDetected),
        "Hypothesis" => Some(ThoughtType::Hypothesis),
        "Mistake" => Some(ThoughtType::Mistake),
        "Correction" => Some(ThoughtType::Correction),
        "LessonLearned" => Some(ThoughtType::LessonLearned),
        "AssumptionInvalidated" => Some(ThoughtType::AssumptionInvalidated),
        "Constraint" => Some(ThoughtType::Constraint),
        "Plan" => Some(ThoughtType::Plan),
        "Subgoal" => Some(ThoughtType::Subgoal),
        "Decision" => Some(ThoughtType::Decision),
        "StrategyShift" => Some(ThoughtType::StrategyShift),
        "Wonder" => Some(ThoughtType::Wonder),
        "Question" => Some(ThoughtType::Question),
        "Idea" => Some(ThoughtType::Idea),
        "Experiment" => Some(ThoughtType::Experiment),
        "ActionTaken" => Some(ThoughtType::ActionTaken),
        "TaskComplete" => Some(ThoughtType::TaskComplete),
        "Checkpoint" => Some(ThoughtType::Checkpoint),
        "StateSnapshot" => Some(ThoughtType::StateSnapshot),
        "Handoff" => Some(ThoughtType::Handoff),
        "Summary" => Some(ThoughtType::Summary),
        "Reframe" => Some(ThoughtType::Reframe),
        "Surprise" => Some(ThoughtType::Surprise),
        _ => None,
    }
}

fn parse_managed_vector_provider_kind(raw: &str) -> Option<ManagedVectorProviderKind> {
    match raw.trim() {
        "local-text-v1" => Some(ManagedVectorProviderKind::LocalTextV1),
        _ => None,
    }
}

// ── API response shape helpers ────────────────────────────────────────────────

/// Serialise a page of thoughts alongside pagination metadata.
///
/// When `reverse` is `true` the slice is returned newest-first (descending by
/// append index). Pagination is applied in streaming order so the full filtered
/// result set does not need to be reversed or materialized up front.
fn paginated_thoughts<F>(
    thoughts: &[Thought],
    page: usize,
    per_page: usize,
    reverse: bool,
    mut predicate: F,
) -> Value
where
    F: FnMut(&Thought) -> bool,
{
    let page = page.max(1);
    let per_page = per_page.max(1);
    let start = (page.saturating_sub(1)).saturating_mul(per_page);
    let mut total = 0usize;
    let mut slice = Vec::with_capacity(per_page);

    if reverse {
        for thought in thoughts.iter().rev() {
            if !predicate(thought) {
                continue;
            }
            if total >= start && slice.len() < per_page {
                slice.push(thought);
            }
            total += 1;
        }
    } else {
        for thought in thoughts {
            if !predicate(thought) {
                continue;
            }
            if total >= start && slice.len() < per_page {
                slice.push(thought);
            }
            total += 1;
        }
    }

    let pages = total.div_ceil(per_page);

    json!({
        "thoughts": slice,
        "total": total,
        "page": page,
        "per_page": per_page,
        "pages": pages,
    })
}

fn paginated_thought_refs(
    thoughts: &[&Thought],
    page: usize,
    per_page: usize,
    reverse: bool,
) -> Value {
    let page = page.max(1);
    let per_page = per_page.max(1);
    let total = thoughts.len();
    let pages = total.div_ceil(per_page);
    let start = (page.saturating_sub(1)).saturating_mul(per_page);

    let slice: Vec<&Thought> = if reverse {
        thoughts
            .iter()
            .rev()
            .skip(start)
            .take(per_page)
            .copied()
            .collect()
    } else {
        thoughts
            .iter()
            .skip(start)
            .take(per_page)
            .copied()
            .collect()
    };

    json!({
        "thoughts": slice,
        "total": total,
        "page": page,
        "per_page": per_page,
        "pages": pages,
    })
}

fn dashboard_ranked_graph() -> RankedSearchGraph {
    RankedSearchGraph::new()
        .with_mode(crate::search::GraphExpansionMode::IncomingOnly)
        .with_max_depth(2)
        .with_max_visited(128)
}

fn dashboard_search_text(params: &DashboardSearchQuery) -> Option<String> {
    params
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn dashboard_search_filter(params: &DashboardSearchQuery) -> ThoughtQuery {
    let mut query = ThoughtQuery::new();
    if let Some(agent_id) = params
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|agent_id| !agent_id.is_empty())
    {
        query = query.with_agent_ids([agent_id.to_string()]);
    }
    if let Some(types) = parse_type_filter(params.types.as_deref()) {
        query = query.with_types(types);
    }
    query
}

fn dashboard_pages(total: usize, per_page: usize) -> usize {
    total.div_ceil(per_page.max(1))
}

fn relation_kind_label(kind: ThoughtRelationKind) -> &'static str {
    match kind {
        ThoughtRelationKind::References => "references",
        ThoughtRelationKind::Summarizes => "summarizes",
        ThoughtRelationKind::Corrects => "corrects",
        ThoughtRelationKind::Invalidates => "invalidates",
        ThoughtRelationKind::CausedBy => "caused_by",
        ThoughtRelationKind::Supports => "supports",
        ThoughtRelationKind::Contradicts => "contradicts",
        ThoughtRelationKind::DerivedFrom => "derived_from",
        ThoughtRelationKind::ContinuesFrom => "continues_from",
        ThoughtRelationKind::RelatedTo => "related_to",
        ThoughtRelationKind::Supersedes => "supersedes",
    }
}

fn thought_json_for_locator(
    chain: &MentisDb,
    locator: &crate::search::ThoughtLocator,
) -> Option<Value> {
    if locator.chain_key.is_some() {
        return None;
    }
    if let Some(index) = locator.thought_index {
        if let Some(thought) = chain.get_thought_by_index(index) {
            if thought.id == locator.thought_id {
                return Some(chain.thought_json(thought));
            }
        }
    }
    chain
        .get_thought_by_id(locator.thought_id)
        .map(|thought| chain.thought_json(thought))
}

fn graph_path_to_json(path: &crate::search::GraphExpansionPath) -> Value {
    json!({
        "seed": path.seed,
        "hops": path.hops.iter().map(|hop| {
            json!({
                "direction": hop.direction,
                "edge": hop.edge,
            })
        }).collect::<Vec<_>>(),
    })
}

fn ranked_hit_response(
    chain: &MentisDb,
    hit: crate::RankedSearchHit<'_>,
) -> DashboardRankedHitResponse {
    DashboardRankedHitResponse {
        thought: chain.thought_json(hit.thought),
        score: DashboardRankedScoreResponse {
            lexical: hit.score.lexical,
            vector: hit.score.vector,
            graph: hit.score.graph,
            relation: hit.score.relation,
            seed_support: hit.score.seed_support,
            importance: hit.score.importance,
            confidence: hit.score.confidence,
            recency: hit.score.recency,
            total: hit.score.total,
        },
        matched_terms: hit.matched_terms,
        match_sources: hit
            .match_sources
            .into_iter()
            .map(|source| source.as_str().to_string())
            .collect(),
        graph_distance: hit.graph_distance,
        graph_seed_paths: hit.graph_seed_paths,
        graph_relation_kinds: hit
            .graph_relation_kinds
            .into_iter()
            .map(relation_kind_label)
            .map(str::to_string)
            .collect(),
        graph_path: hit.graph_path.as_ref().map(graph_path_to_json),
    }
}

fn thought_counts_by_agent(thoughts: &[Thought]) -> HashMap<&str, u64> {
    let mut counts = HashMap::new();
    for thought in thoughts {
        *counts.entry(thought.agent_id.as_str()).or_insert(0) += 1;
    }
    counts
}

// ── Query parameter structs ───────────────────────────────────────────────────

/// Query parameters for thought-listing endpoints.
#[derive(Deserialize, Default)]
struct ThoughtsQuery {
    /// 1-based page number (defaults to 1).
    page: Option<usize>,
    /// Items per page (defaults to 50).
    per_page: Option<usize>,
    /// Comma-separated list of [`ThoughtType`] names to filter by.
    types: Option<String>,
    /// Sort order: `"asc"` (oldest first) or `"desc"` (newest first, default).
    order: Option<String>,
}

/// Query parameters for chain-scoped dashboard search.
#[derive(Deserialize, Default)]
struct DashboardSearchQuery {
    /// 1-based page number (defaults to 1).
    page: Option<usize>,
    /// Items per page (defaults to 50).
    per_page: Option<usize>,
    /// Comma-separated list of [`ThoughtType`] names to filter by.
    types: Option<String>,
    /// Sort order: `"asc"` (oldest first) or `"desc"` (newest first, default).
    order: Option<String>,
    /// Full-text filter over content, tags, concepts, and registry fields.
    text: Option<String>,
    /// Optional producing agent id.
    agent_id: Option<String>,
}

#[derive(Serialize)]
struct DashboardRankedScoreResponse {
    lexical: f32,
    vector: f32,
    graph: f32,
    relation: f32,
    seed_support: f32,
    importance: f32,
    confidence: f32,
    recency: f32,
    total: f32,
}

#[derive(Serialize)]
struct DashboardRankedHitResponse {
    thought: Value,
    score: DashboardRankedScoreResponse,
    matched_terms: Vec<String>,
    match_sources: Vec<String>,
    graph_distance: Option<usize>,
    graph_seed_paths: usize,
    graph_relation_kinds: Vec<String>,
    graph_path: Option<Value>,
}

#[derive(Serialize)]
struct DashboardSearchResponse {
    mode: String,
    backend: Option<String>,
    thoughts: Vec<Value>,
    results: Vec<DashboardRankedHitResponse>,
    bundles: Vec<DashboardContextBundleResponse>,
    total: usize,
    page: usize,
    per_page: usize,
    pages: usize,
}

#[derive(Serialize)]
struct DashboardContextBundleSeedResponse {
    locator: crate::search::ThoughtLocator,
    lexical_score: f32,
    matched_terms: Vec<String>,
    thought: Option<Value>,
}

#[derive(Serialize)]
struct DashboardContextBundleHitResponse {
    locator: crate::search::ThoughtLocator,
    thought: Option<Value>,
    depth: usize,
    seed_path_count: usize,
    relation_kinds: Vec<String>,
    path: Value,
}

#[derive(Serialize)]
struct DashboardContextBundleResponse {
    seed: DashboardContextBundleSeedResponse,
    support: Vec<DashboardContextBundleHitResponse>,
}

#[derive(Serialize)]
struct DashboardContextBundlesResponse {
    total_bundles: usize,
    consumed_hits: usize,
    page: usize,
    per_page: usize,
    pages: usize,
    bundles: Vec<DashboardContextBundleResponse>,
}

/// Query parameters for the skill-diff endpoint.
#[derive(Deserialize)]
struct DiffQuery {
    /// Version UUID to use as the "before" side of the diff.
    from: Option<String>,
    /// Version UUID to use as the "after" side of the diff.
    to: Option<String>,
}

// ── API: chain listing ────────────────────────────────────────────────────────

/// `GET /dashboard/api/chains`
///
/// Returns a JSON array of chain summaries with live thought and agent counts.
/// Includes both chains registered on disk and any chains currently live in the
/// in-memory DashMap cache (e.g. created mid-session via MCP/REST).
async fn api_chains(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Read the on-disk registry — this is a single fast JSON file read and
    // gives us thought_count / agent_count for every registered chain without
    // opening any chain file.
    let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;

    // Start with all registered chains, then overlay live (in-memory) counts
    // for any chains that are already open in the DashMap cache.
    let mut by_key: BTreeMap<String, Value> = registry
        .chains
        .into_iter()
        .map(|(key, reg)| {
            let v = json!({
                "chain_key":     key.clone(),
                "thought_count": reg.thought_count,
                "agent_count":   reg.agent_count,
                "head_hash":     Value::Null,
            });
            (key, v)
        })
        .collect();

    // For chains already open in the live cache, upgrade to live counts and
    // add the head hash (not stored in the registry).
    for entry in state.chains.iter() {
        let key = entry.key().clone();
        if let Ok(chain) = entry.value().try_read() {
            let v = json!({
                "chain_key":     key.clone(),
                "thought_count": chain.thoughts().len(),
                "agent_count":   chain.agent_registry().agents.len(),
                "head_hash":     chain.head_hash().map(ToString::to_string),
            });
            by_key.insert(key, v);
        }
    }

    // Also include any live chains that aren't in the registry yet (e.g.
    // freshly bootstrapped chains not yet flushed to the registry file).
    for entry in state.chains.iter() {
        let key = entry.key().clone();
        if !by_key.contains_key(&key) {
            if let Ok(chain) = entry.value().try_read() {
                by_key.insert(key.clone(), json!({
                    "chain_key":     key,
                    "thought_count": chain.thoughts().len(),
                    "agent_count":   chain.agent_registry().agents.len(),
                    "head_hash":     chain.head_hash().map(ToString::to_string),
                }));
            }
        }
    }

    let chains: Vec<Value> = by_key.into_values().collect();
    Ok(Json(json!(chains)))
}

/// `GET /dashboard/api/chains/:chain_key`
///
/// Returns one chain summary plus vector sidecar management state.
async fn api_chain_detail(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;
    let vector_sidecars = chain
        .managed_vector_sidecar_statuses()
        .map_err(internal_error)?;
    Ok(Json(json!({
        "chain_key": chain_key,
        "thought_count": chain.thoughts().len(),
        "agent_count": chain.agent_registry().agents.len(),
        "head_hash": chain.head_hash().map(ToString::to_string),
        "vector_sidecars": vector_sidecars,
    })))
}

async fn api_enable_vector_sidecar(
    State(state): State<DashboardState>,
    Path((chain_key, provider_key)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let provider_kind = parse_managed_vector_provider_kind(&provider_key).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown vector provider '{provider_key}'") })),
        )
    })?;
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;
    let status = chain
        .set_managed_vector_sidecar_enabled(provider_kind, true)
        .map_err(internal_error)?;
    Ok(Json(json!({ "status": status })))
}

async fn api_disable_vector_sidecar(
    State(state): State<DashboardState>,
    Path((chain_key, provider_key)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let provider_kind = parse_managed_vector_provider_kind(&provider_key).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown vector provider '{provider_key}'") })),
        )
    })?;
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;
    let status = chain
        .set_managed_vector_sidecar_enabled(provider_kind, false)
        .map_err(internal_error)?;
    Ok(Json(json!({ "status": status })))
}

async fn api_sync_vector_sidecar(
    State(state): State<DashboardState>,
    Path((chain_key, provider_key)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let provider_kind = parse_managed_vector_provider_kind(&provider_key).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown vector provider '{provider_key}'") })),
        )
    })?;
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;
    let status = chain
        .sync_managed_vector_sidecar_now(provider_kind)
        .map_err(internal_error)?;
    Ok(Json(json!({ "status": status })))
}

#[derive(Deserialize)]
struct RebuildVectorSidecarBody {
    confirm_delete: bool,
}

async fn api_rebuild_vector_sidecar(
    State(state): State<DashboardState>,
    Path((chain_key, provider_key)): Path<(String, String)>,
    Json(body): Json<RebuildVectorSidecarBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !body.confirm_delete {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "confirm_delete=true is required to rebuild the vector sidecar from scratch"
            })),
        ));
    }
    let provider_kind = parse_managed_vector_provider_kind(&provider_key).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("unknown vector provider '{provider_key}'") })),
        )
    })?;
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;
    let status = chain
        .rebuild_managed_vector_sidecar_from_scratch(provider_kind)
        .map_err(internal_error)?;
    Ok(Json(json!({ "status": status })))
}

// ── API: bootstrap chain ──────────────────────────────────────────────────────

/// JSON body for `POST /dashboard/api/chains`.
#[derive(Deserialize)]
struct BootstrapChainBody {
    chain_key: String,
    content: String,
    agent_id: Option<String>,
    tags: Option<Vec<String>>,
    concepts: Option<Vec<String>>,
    importance: Option<f32>,
}

/// `POST /dashboard/api/chains`
///
/// Bootstraps a new chain (creates it and appends a bootstrap thought if it
/// is empty). Returns `{"bootstrapped": true/false, "chain_key": "..."}`.
async fn api_bootstrap_chain(
    State(state): State<DashboardState>,
    Json(body): Json<BootstrapChainBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let chain_key = body.chain_key.trim().to_string();
    if chain_key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "chain_key must not be empty"})),
        ));
    }
    let arc = match get_or_open_chain(&state, &chain_key).await {
        Ok(arc) => arc,
        Err((StatusCode::NOT_FOUND, _)) => {
            let mut chain = MentisDb::open_with_key_and_storage_kind(
                &state.mentisdb_dir,
                &chain_key,
                state.default_storage_adapter,
            )
            .map_err(internal_error)?;
            chain
                .set_auto_flush(state.auto_flush)
                .map_err(internal_error)?;
            chain
                .apply_persisted_managed_vector_sidecars()
                .map_err(internal_error)?;
            let arc = Arc::new(RwLock::new(chain));
            state.chains.insert(chain_key.clone(), arc.clone());
            arc
        }
        Err(err) => return Err(err),
    };
    let mut chain = arc.write().await;
    let bootstrapped = if chain.thoughts().is_empty() {
        let agent_id = body.agent_id.as_deref().unwrap_or("system");
        let input = ThoughtInput::new(ThoughtType::Summary, body.content.clone())
            .with_role(ThoughtRole::Checkpoint)
            .with_importance(body.importance.unwrap_or(1.0))
            .with_tags(body.tags.clone().unwrap_or_default())
            .with_concepts(body.concepts.clone().unwrap_or_default());
        chain
            .append_thought(agent_id, input)
            .map_err(internal_error)?;
        true
    } else {
        false
    };
    Ok(Json(
        json!({ "bootstrapped": bootstrapped, "chain_key": chain_key }),
    ))
}

// ── API: delete chain ─────────────────────────────────────────────────────────

/// `DELETE /dashboard/api/chains/:chain_key`
///
/// Permanently deletes a chain: removes its storage file, deregisters it from
/// the registry, and evicts it from the in-memory cache.
async fn api_delete_chain(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // Evict from in-memory cache first so no new writes can sneak in.
    if let Some((_, arc)) = state.chains.remove(&chain_key) {
        // Detach registry persistence before deleting files so any surviving
        // Arc clones cannot resurrect the chain during Drop.
        let mut chain = arc.write().await;
        chain.detach_persistence();
    }
    // Deregister + delete storage file.
    deregister_chain(&state.mentisdb_dir, &chain_key).map_err(internal_error)?;
    Ok(Json(json!({ "deleted": true, "chain_key": chain_key })))
}

// ── API: thoughts ─────────────────────────────────────────────────────────────

/// `GET /dashboard/api/chains/:chain_key/thoughts?page=1&per_page=50&types=Decision,Insight`
///
/// Returns a paginated list of thoughts from the requested chain, optionally
/// filtered by a comma-separated list of [`ThoughtType`] names.
async fn api_chain_thoughts(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
    Query(params): Query<ThoughtsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    let type_filter = parse_type_filter(params.types.as_deref());

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let reverse = params.order.as_deref().unwrap_or("desc") != "asc";

    Ok(Json(paginated_thoughts(
        chain.thoughts(),
        page,
        per_page,
        reverse,
        |t| {
            type_filter
                .as_ref()
                .map(|types| types.contains(&t.thought_type))
                .unwrap_or(true)
        },
    )))
}

/// `GET /dashboard/api/chains/:chain_key/search`
///
/// Returns a paginated, chain-scoped dashboard search result.
///
/// When `text` is present this returns a canonical ranked payload with bundled
/// context. Without `text`, it falls back to the explorer's legacy
/// chronological filtering semantics.
async fn api_chain_search(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
    Query(params): Query<DashboardSearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let reverse = params.order.as_deref().unwrap_or("desc") != "asc";
    let filter = dashboard_search_filter(&params);

    let Some(text) = dashboard_search_text(&params) else {
        let matched = chain.query(&filter);
        return Ok(Json(paginated_thought_refs(
            matched.as_slice(),
            page,
            per_page,
            reverse,
        )));
    };

    let offset = (page.saturating_sub(1)).saturating_mul(per_page);
    let ranked_limit = offset.saturating_add(per_page).max(1);
    let ranked_query = RankedSearchQuery::new()
        .with_filter(filter)
        .with_text(text)
        .with_graph(dashboard_ranked_graph())
        .with_limit(ranked_limit);
    let ranked = chain.query_ranked(&ranked_query);
    let total = ranked.total_candidates;
    let pages = dashboard_pages(total, per_page);
    let results: Vec<DashboardRankedHitResponse> = ranked
        .hits
        .into_iter()
        .skip(offset)
        .take(per_page)
        .map(|hit| ranked_hit_response(&chain, hit))
        .collect();
    let bundles: Vec<DashboardContextBundleResponse> = chain
        .query_context_bundles(&ranked_query)
        .bundles
        .into_iter()
        .skip(offset)
        .take(per_page)
        .map(|bundle| DashboardContextBundleResponse {
            seed: DashboardContextBundleSeedResponse {
                locator: bundle.seed.locator.clone(),
                lexical_score: bundle.seed.lexical_score,
                matched_terms: bundle.seed.matched_terms,
                thought: thought_json_for_locator(&chain, &bundle.seed.locator),
            },
            support: bundle
                .support
                .into_iter()
                .map(|support_hit| DashboardContextBundleHitResponse {
                    locator: support_hit.locator.clone(),
                    thought: thought_json_for_locator(&chain, &support_hit.locator),
                    depth: support_hit.depth,
                    seed_path_count: support_hit.seed_path_count,
                    relation_kinds: support_hit
                        .relation_kinds
                        .into_iter()
                        .map(relation_kind_label)
                        .map(str::to_string)
                        .collect(),
                    path: graph_path_to_json(&support_hit.path),
                })
                .collect(),
        })
        .collect();
    let response = DashboardSearchResponse {
        mode: "ranked".to_string(),
        backend: Some(ranked.backend.as_str().to_string()),
        thoughts: results.iter().map(|hit| hit.thought.clone()).collect(),
        results,
        bundles,
        total,
        page,
        per_page,
        pages,
    };
    Ok(Json(
        serde_json::to_value(response).map_err(internal_error)?,
    ))
}

/// `GET /dashboard/api/chains/:chain_key/search/bundles`
///
/// Returns paginated seed-anchored supporting context bundles for the current
/// dashboard search text query.
async fn api_chain_search_bundles(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
    Query(params): Query<DashboardSearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(text) = dashboard_search_text(&params) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "text query is required for context bundles"})),
        ));
    };
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let offset = (page.saturating_sub(1)).saturating_mul(per_page);
    let bundle_limit = offset.saturating_add(per_page).max(1);
    let result = chain.query_context_bundles(
        &RankedSearchQuery::new()
            .with_filter(dashboard_search_filter(&params))
            .with_text(text)
            .with_graph(dashboard_ranked_graph())
            .with_limit(bundle_limit),
    );
    let total_bundles = result.bundles.len();
    let pages = dashboard_pages(total_bundles, per_page);
    let bundles = result
        .bundles
        .into_iter()
        .skip(offset)
        .take(per_page)
        .map(|bundle| DashboardContextBundleResponse {
            seed: DashboardContextBundleSeedResponse {
                locator: bundle.seed.locator.clone(),
                lexical_score: bundle.seed.lexical_score,
                matched_terms: bundle.seed.matched_terms,
                thought: thought_json_for_locator(&chain, &bundle.seed.locator),
            },
            support: bundle
                .support
                .into_iter()
                .map(|support_hit| DashboardContextBundleHitResponse {
                    locator: support_hit.locator.clone(),
                    thought: thought_json_for_locator(&chain, &support_hit.locator),
                    depth: support_hit.depth,
                    seed_path_count: support_hit.seed_path_count,
                    relation_kinds: support_hit
                        .relation_kinds
                        .into_iter()
                        .map(relation_kind_label)
                        .map(str::to_string)
                        .collect(),
                    path: graph_path_to_json(&support_hit.path),
                })
                .collect(),
        })
        .collect();
    let response = DashboardContextBundlesResponse {
        total_bundles,
        consumed_hits: result.consumed_hits,
        page,
        per_page,
        pages,
        bundles,
    };
    Ok(Json(
        serde_json::to_value(response).map_err(internal_error)?,
    ))
}

/// `GET /dashboard/api/chains/:chain_key/search/agents`
///
/// Returns live thought authors for the chain, merged with registry display
/// names when available. Registry-only agents without thoughts are omitted so
/// the explorer search dropdown stays aligned with actual searchable content.
async fn api_chain_search_agents(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    let mut agents: Vec<(String, Option<String>, u64)> = thought_counts_by_agent(chain.thoughts())
        .into_iter()
        .map(|(agent_id, thought_count)| {
            let display_name = chain
                .agent_registry()
                .agents
                .get(agent_id)
                .map(|record| record.display_name.trim().to_string())
                .filter(|value| !value.is_empty());
            (agent_id.to_string(), display_name, thought_count)
        })
        .collect();

    agents.sort_by(|(left_id, left_name, _), (right_id, right_name, _)| {
        let left_key = left_name
            .as_deref()
            .unwrap_or(left_id.as_str())
            .to_ascii_lowercase();
        let right_key = right_name
            .as_deref()
            .unwrap_or(right_id.as_str())
            .to_ascii_lowercase();
        left_key.cmp(&right_key).then_with(|| {
            left_id
                .to_ascii_lowercase()
                .cmp(&right_id.to_ascii_lowercase())
        })
    });

    Ok(Json(json!(agents
        .into_iter()
        .map(|(agent_id, display_name, thought_count)| json!({
            "agent_id": agent_id,
            "display_name": display_name,
            "thought_count": thought_count,
        }))
        .collect::<Vec<_>>())))
}

/// `GET /dashboard/api/thoughts/:chain_key/:thought_id`
///
/// Returns a single thought identified by its UUID.
async fn api_get_thought(
    State(state): State<DashboardState>,
    Path((chain_key, thought_id_str)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let thought_id = thought_id_str.parse::<Uuid>().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    let thought = chain.get_thought_by_id(thought_id).ok_or_else(|| {
        not_found(format!(
            "thought '{thought_id}' not found in chain '{chain_key}'"
        ))
    })?;

    Ok(Json(serde_json::to_value(thought).map_err(internal_error)?))
}

/// `GET /dashboard/api/chains/:chain_key/agents/:agent_id/thoughts?page=1&per_page=50&types=...`
///
/// Returns a paginated list of thoughts authored by the given agent.
async fn api_agent_thoughts(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
    Query(params): Query<ThoughtsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    let type_filter = parse_type_filter(params.types.as_deref());

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let reverse = params.order.as_deref().unwrap_or("desc") != "asc";

    Ok(Json(paginated_thoughts(
        chain.thoughts(),
        page,
        per_page,
        reverse,
        |t| {
            t.agent_id == agent_id
                && type_filter
                    .as_ref()
                    .map(|types| types.contains(&t.thought_type))
                    .unwrap_or(true)
        },
    )))
}

fn parse_type_filter(raw: Option<&str>) -> Option<Vec<ThoughtType>> {
    raw.map(|raw| raw.split(',').filter_map(parse_thought_type).collect())
}

// ── API: agents ───────────────────────────────────────────────────────────────

/// `GET /dashboard/api/agents`
///
/// Returns all registered agents across all known chains, keyed by chain key.
async fn api_agents_all(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut chain_keys: BTreeSet<String> = {
        let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;
        registry.chains.into_keys().collect()
    };
    for entry in state.chains.iter() {
        chain_keys.insert(entry.key().clone());
    }

    let mut result: BTreeMap<String, Value> = BTreeMap::new();

    for chain_key in &chain_keys {
        match get_or_open_chain(&state, chain_key).await {
            Ok(arc) => {
                let chain = arc.read().await;
                let thoughts = chain.thoughts();
                let thought_counts = thought_counts_by_agent(thoughts);
                let agents: Vec<Value> = chain
                    .agent_registry()
                    .agents
                    .values()
                    .map(|a| {
                        let live_count = thought_counts
                            .get(a.agent_id.as_str())
                            .copied()
                            .unwrap_or(0);
                        let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
                        if let Value::Object(ref mut m) = v {
                            m.insert("thought_count".to_string(), live_count.into());
                        }
                        v
                    })
                    .collect();
                result.insert(
                    chain_key.to_string(),
                    json!({
                        "chain_key": chain_key,
                        "total_agents": chain.agent_registry().agents.len(),
                        "total_thoughts": thoughts.len(),
                        "agents": agents,
                    }),
                );
            }
            Err((StatusCode::NOT_FOUND, _)) => {
                continue;
            }
            Err(_) => {
                result.insert(
                    chain_key.to_string(),
                    json!({
                        "chain_key": chain_key,
                        "total_agents": 0,
                        "total_thoughts": 0,
                        "agents": [],
                    }),
                );
            }
        }
    }

    Ok(Json(serde_json::to_value(result).map_err(internal_error)?))
}

/// JSON body for `POST /dashboard/api/agents`.
#[derive(Debug, Deserialize)]
struct AgentCreateBody {
    /// Target chain key — must already exist or be bootstrappable.
    chain_key: String,
    /// Stable agent identifier (e.g. `"orion"`, `"my-rust-agent"`).
    agent_id: String,
    /// Human-readable display name shown in the dashboard.
    display_name: Option<String>,
    /// Owner or team label (e.g. `"@alice"`).
    agent_owner: Option<String>,
    /// Free-form description of the agent's role and capabilities.
    description: Option<String>,
}

/// `POST /dashboard/api/agents`
///
/// Creates or updates an agent registry entry on the specified chain.
/// This is the dashboard counterpart to `mentisdb_upsert_agent`.
///
/// The agent is registered with [`AgentStatus::Active`] by default.  If the
/// `agent_id` already exists the mutable fields (`display_name`,
/// `agent_owner`, `description`) are updated in-place and the status is left
/// unchanged.
async fn api_create_agent(
    State(state): State<DashboardState>,
    Json(body): Json<AgentCreateBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &body.chain_key).await?;
    let mut chain = arc.write().await;
    let agent = chain
        .upsert_agent(
            &body.agent_id,
            body.display_name.as_deref(),
            body.agent_owner.as_deref(),
            body.description.as_deref(),
            None, // preserve existing status; newly registered agents default to Active
        )
        .map_err(internal_error)?;
    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

/// `GET /dashboard/api/agents/:chain_key`
///
/// Returns all registered agents for the given chain.
async fn api_agents_by_chain(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;
    let thoughts = chain.thoughts();
    let thought_counts = thought_counts_by_agent(thoughts);
    let agents: Vec<Value> = chain
        .agent_registry()
        .agents
        .values()
        .map(|a| {
            let live_count = thought_counts
                .get(a.agent_id.as_str())
                .copied()
                .unwrap_or(0);
            let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
            if let Value::Object(ref mut m) = v {
                m.insert("thought_count".to_string(), live_count.into());
            }
            v
        })
        .collect();
    Ok(Json(serde_json::to_value(agents).map_err(internal_error)?))
}

/// `GET /dashboard/api/agents/:chain_key/:agent_id`
///
/// Returns a single agent record from the given chain.
async fn api_get_agent(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;
    let agent = chain
        .agent_registry()
        .agents
        .get(&agent_id)
        .ok_or_else(|| {
            not_found(format!(
                "agent '{agent_id}' not found in chain '{chain_key}'"
            ))
        })?;
    let thought_counts = thought_counts_by_agent(chain.thoughts());
    let live_count = thought_counts.get(agent_id.as_str()).copied().unwrap_or(0);
    let mut v = serde_json::to_value(agent).map_err(internal_error)?;
    if let Value::Object(ref mut m) = v {
        m.insert("thought_count".to_string(), live_count.into());
    }
    Ok(Json(v))
}

// ── Agent mutation helpers ────────────────────────────────────────────────────

/// JSON body for `PATCH /dashboard/api/agents/:chain_key/:agent_id`.
#[derive(Deserialize)]
struct AgentPatchBody {
    display_name: Option<String>,
    description: Option<String>,
    agent_owner: Option<String>,
}

/// `PATCH /dashboard/api/agents/:chain_key/:agent_id`
///
/// Updates one or more mutable fields on an agent record and persists the
/// registry to disk.
async fn api_patch_agent(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
    Json(body): Json<AgentPatchBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;

    let agent = chain
        .upsert_agent(
            &agent_id,
            body.display_name.as_deref(),
            body.agent_owner.as_deref(),
            body.description.as_deref(),
            None, // status not changed via PATCH
        )
        .map_err(internal_error)?;

    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

/// `POST /dashboard/api/agents/:chain_key/:agent_id/revoke`
///
/// Marks the agent as [`AgentStatus::Revoked`] and persists the registry.
async fn api_revoke_agent(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;

    let agent = chain
        .upsert_agent(&agent_id, None, None, None, Some(AgentStatus::Revoked))
        .map_err(internal_error)?;

    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

/// `POST /dashboard/api/agents/:chain_key/:agent_id/activate`
///
/// Marks the agent as [`AgentStatus::Active`] and persists the registry.
async fn api_activate_agent(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;

    let agent = chain
        .upsert_agent(&agent_id, None, None, None, Some(AgentStatus::Active))
        .map_err(internal_error)?;

    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

/// JSON body for `POST /dashboard/api/agents/:chain_key/:agent_id/keys`.
#[derive(Deserialize)]
struct AddKeyBody {
    key_id: String,
    algorithm: String,
    public_key_bytes: Vec<u8>,
}

/// `POST /dashboard/api/agents/:chain_key/:agent_id/keys`
///
/// Registers a new public verification key on the agent record.
async fn api_add_agent_key(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
    Json(body): Json<AddKeyBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let algorithm = body
        .algorithm
        .parse::<PublicKeyAlgorithm>()
        .map_err(|e| (StatusCode::BAD_REQUEST, Json(json!({ "error": e }))))?;

    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;

    let agent = chain
        .add_agent_key(&agent_id, &body.key_id, algorithm, body.public_key_bytes)
        .map_err(internal_error)?;

    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

/// `DELETE /dashboard/api/agents/:chain_key/:agent_id/keys/:key_id`
///
/// Revokes the specified public key on the agent record.
async fn api_delete_agent_key(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id, key_id)): Path<(String, String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;

    let agent = chain
        .revoke_agent_key(&agent_id, &key_id)
        .map_err(internal_error)?;

    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
}

// ── API: skills ───────────────────────────────────────────────────────────────

async fn refresh_skill_registry(state: &DashboardState) -> Result<(), (StatusCode, Json<Value>)> {
    let mut registry = state.skills.write().await;
    registry
        .refresh_from_disk_if_stale()
        .map_err(internal_error)?;
    Ok(())
}

/// `GET /dashboard/api/skills`
///
/// Returns a summary list of all registered skills.
async fn api_skills(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let skills = state.skills.read().await;
    let list = skills.list_skills();
    Ok(Json(serde_json::to_value(list).map_err(internal_error)?))
}

/// `GET /dashboard/api/skills/:skill_id`
///
/// Returns the summary and latest Markdown content for a skill.
async fn api_get_skill(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let skills = state.skills.read().await;

    let summary = skills
        .list_skills()
        .into_iter()
        .find(|s| s.skill_id == skill_id)
        .ok_or_else(|| not_found(format!("skill '{skill_id}' not found")))?;

    let output = skills
        .read_skill(&skill_id, None, SkillFormat::Markdown)
        .map_err(internal_error)?;

    Ok(Json(
        json!({ "summary": summary, "markdown": output.content, "warnings": output.warnings, "status": output.status }),
    ))
}

/// `GET /dashboard/api/skills/:skill_id/versions`
///
/// Returns the full version history for a skill.
async fn api_skill_versions(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let skills = state.skills.read().await;
    let versions = skills.skill_versions(&skill_id).map_err(internal_error)?;
    Ok(Json(
        serde_json::to_value(versions).map_err(internal_error)?,
    ))
}

/// `GET /dashboard/api/skills/:skill_id/diff?from=<version_id>&to=<version_id>`
///
/// Produces a unified diff between two versions of a skill.
/// When `from` or `to` are omitted the latest version is used for the
/// respective side.
async fn api_skill_diff(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
    Query(params): Query<DiffQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let skills = state.skills.read().await;

    let parse_version_id = |raw: Option<&str>| -> Result<Option<Uuid>, (StatusCode, Json<Value>)> {
        match raw {
            Some(s) => s.parse::<Uuid>().map(Some).map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": e.to_string() })),
                )
            }),
            None => Ok(None),
        }
    };

    let from_id = parse_version_id(params.from.as_deref())?;
    let to_id = parse_version_id(params.to.as_deref())?;

    let old_content = skills
        .read_skill(&skill_id, from_id, SkillFormat::Markdown)
        .map_err(internal_error)?;

    let new_content = skills
        .read_skill(&skill_id, to_id, SkillFormat::Markdown)
        .map_err(internal_error)?;

    let patch = diffy::create_patch(&old_content.content, &new_content.content);
    Ok(Json(json!({ "diff": patch.to_string() })))
}

#[derive(Deserialize)]
struct SkillStatusBody {
    reason: Option<String>,
}

/// `POST /dashboard/api/skills/:skill_id/revoke`
///
/// Marks the skill as revoked. The skill's content and version history are
/// preserved for auditability.
async fn api_revoke_skill(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
    Json(body): Json<SkillStatusBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let mut skills = state.skills.write().await;
    let summary = skills
        .revoke_skill(&skill_id, body.reason.as_deref())
        .map_err(internal_error)?;
    Ok(Json(serde_json::to_value(summary).map_err(internal_error)?))
}

/// `POST /dashboard/api/skills/:skill_id/deprecate`
///
/// Marks the skill as deprecated.
async fn api_deprecate_skill(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
    Json(body): Json<SkillStatusBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let mut skills = state.skills.write().await;
    let summary = skills
        .deprecate_skill(&skill_id, body.reason.as_deref())
        .map_err(internal_error)?;
    Ok(Json(serde_json::to_value(summary).map_err(internal_error)?))
}

/// Request body for `POST /dashboard/api/skills`.
///
/// All fields map directly to the [`SkillUpload`] builder.  `skill_id` is
/// optional; when omitted MentisDB derives a stable id from the skill name
/// found in the content.  `format` defaults to `"markdown"` when absent.
#[derive(Debug, Deserialize)]
struct DashboardUploadSkillBody {
    /// The agent that is uploading the skill.  Must already be registered.
    agent_id: String,
    /// Raw skill content — Markdown or JSON depending on `format`.
    content: String,
    /// Optional stable skill id.  Auto-derived from name when omitted.
    skill_id: Option<String>,
    /// Content format: `"markdown"` (default) or `"json"`.
    format: Option<String>,
}

/// `POST /dashboard/api/skills`
///
/// Uploads a new skill version from the dashboard form.
/// The uploading agent must already be registered in the agent registry.
///
/// # Errors
///
/// Returns `500 Internal Server Error` if the upload fails (e.g. the agent
/// is not registered, the content is malformed, or a storage error occurs).
async fn api_upload_skill(
    State(state): State<DashboardState>,
    Json(body): Json<DashboardUploadSkillBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    refresh_skill_registry(&state).await?;
    let fmt = match body
        .format
        .as_deref()
        .unwrap_or("markdown")
        .to_lowercase()
        .as_str()
    {
        "json" => SkillFormat::Json,
        _ => SkillFormat::Markdown,
    };

    let mut upload = SkillUpload::new(&body.agent_id, fmt, &body.content);
    if let Some(ref id) = body.skill_id {
        if !id.is_empty() {
            upload = upload.with_skill_id(id);
        }
    }

    let mut skills = state.skills.write().await;
    let summary = skills.upload_skill(upload).map_err(internal_error)?;
    Ok(Json(serde_json::to_value(summary).map_err(internal_error)?))
}

/// `GET /dashboard/api/version`
///
/// Returns the crate version baked in at compile time.
async fn api_version() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

/// `GET /dashboard/api/agents/{chain_key}/{agent_id}/memory-markdown`
///
/// Exports all thoughts attributed to `agent_id` on `chain_key` as a
/// `MEMORY.md`-style Markdown document. The response includes the rendered
/// markdown and a suggested filename for "Save As" download.
async fn api_agent_memory_markdown(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id)): Path<(String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let chain = arc.read().await;

    // Filter all thoughts to only this agent's contributions.
    let query = ThoughtQuery::new().with_agent_ids([agent_id.as_str()]);
    let markdown = chain.to_memory_markdown(Some(&query));

    // Build a filesystem-safe suggested filename:
    //   <agent_id>_<chain_key>_AGENT.md  (spaces → underscores, lowercased)
    let safe = |s: &str| {
        s.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let filename = format!("{}_{}_AGENT.md", safe(&agent_id), safe(&chain_key));

    Ok(Json(json!({ "markdown": markdown, "filename": filename })))
}

/// Request body for `POST /dashboard/api/chains/{chain_key}/import-markdown`.
#[derive(Debug, Deserialize)]
struct ImportMarkdownBody {
    /// MEMORY.md formatted markdown content to import.
    markdown: String,
    /// Agent ID to use when a parsed line contains no `agent` token.
    /// Defaults to `"default"` when absent.
    default_agent_id: Option<String>,
}

/// `POST /dashboard/api/chains/{chain_key}/import-markdown`
///
/// Import a MEMORY.md-formatted markdown string into the specified chain,
/// appending each successfully-parsed thought.  Lines that do not match the
/// expected bullet format are silently skipped.
///
/// # Request body
///
/// ```json
/// { "markdown": "...", "default_agent_id": "agent-123" }
/// ```
///
/// # Response
///
/// ```json
/// { "imported": [0, 1, 2], "count": 3 }
/// ```
async fn api_import_markdown(
    State(state): State<DashboardState>,
    Path(chain_key): Path<String>,
    Json(body): Json<ImportMarkdownBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let arc = get_or_open_chain(&state, &chain_key).await?;
    let mut chain = arc.write().await;
    let default_agent_id = body.default_agent_id.as_deref().unwrap_or("default");
    let imported = chain
        .import_from_memory_markdown(&body.markdown, default_agent_id)
        .map_err(internal_error)?;
    let count = imported.len();
    Ok(Json(json!({ "imported": imported, "count": count })))
}

/// `POST /dashboard/api/agents/{chain_key}/{agent_id}/copy-to/{target_chain_key}`
///
/// Copies every thought attributed to `agent_id` on the source chain
/// (`chain_key`) to `target_chain_key` as new append-only entries, preserving
/// all semantic fields (type, role, content, tags, concepts, confidence,
/// importance).
///
/// # Constraints
///
/// - If `agent_id` already has at least one thought on the target chain the
///   request is rejected with `409 Conflict`. This avoids the complexity of
///   syncing diverged histories whose hashes will never match.
/// - Cross-chain positional `refs` and typed `relations` are intentionally
///   dropped: they reference thought indices / UUIDs that belong to the
///   source chain and are meaningless on the target chain.
/// - The agent's display name and owner are propagated via the first appended
///   thought so the agent registry on the target chain is populated correctly.
/// - The agent's description is copied directly into the target chain's agent
///   registry so the Agent detail page continues to show the same metadata
///   after a cross-chain copy.
///
/// # Response
///
/// ```json
/// { "copied": 42 }
/// ```
async fn api_copy_agent_to_chain(
    State(state): State<DashboardState>,
    Path((chain_key, agent_id, target_chain_key)): Path<(String, String, String)>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if chain_key == target_chain_key {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source and target chain must differ" })),
        ));
    }

    // Open source chain (read-only snapshot).
    let src_arc = get_or_open_chain(&state, &chain_key).await?;
    let src_chain = src_arc.read().await;

    // Collect thoughts belonging to this agent (oldest first).
    let (agent_thoughts, agent_name, agent_owner, agent_description): (
        Vec<ThoughtInput>,
        String,
        Option<String>,
        Option<String>,
    ) = {
        // Retrieve agent metadata for name/owner propagation.
        let (agent_name, agent_owner, agent_description): (String, Option<String>, Option<String>) =
            src_chain
                .get_agent(&agent_id)
                .map(|a| {
                    (
                        a.display_name.clone(),
                        a.owner.clone(),
                        a.description.clone(),
                    )
                })
                .unwrap_or_else(|| (String::new(), None, None));

        let inputs = src_chain
            .thoughts()
            .iter()
            .filter(|t| t.agent_id == agent_id)
            .enumerate()
            .map(|(i, t)| {
                let mut input = ThoughtInput::new(t.thought_type, t.content.clone());
                input.role = t.role;
                input.importance = t.importance;
                input.confidence = t.confidence;
                input.tags = t.tags.clone();
                input.concepts = t.concepts.clone();
                // Propagate agent metadata on the first thought so the target
                // chain's agent registry entry is populated with the correct
                // display name and owner.
                if i == 0 {
                    if !agent_name.is_empty() {
                        input.agent_name = Some(agent_name.clone());
                    }
                    if let Some(ref owner) = agent_owner {
                        if !owner.is_empty() {
                            input.agent_owner = Some(owner.clone());
                        }
                    }
                }
                // refs and relations are positional/UUID references into the
                // source chain; they cannot be meaningfully carried over.
                input
            })
            .collect::<Vec<_>>();
        (inputs, agent_name, agent_owner, agent_description)
    };
    drop(src_chain);

    if agent_thoughts.is_empty() {
        return Ok(Json(json!({ "copied": 0 })));
    }

    // Open (or create) the target chain.
    let dst_arc = {
        // create_new=false: open if it exists, create if it doesn't
        let chain = MentisDb::open_with_key_and_storage_kind(
            &state.mentisdb_dir,
            &target_chain_key,
            state.default_storage_adapter,
        )
        .map_err(|e| internal_error(format!("open target chain '{target_chain_key}': {e}")))?;
        let mut chain = chain;
        chain
            .set_auto_flush(state.auto_flush)
            .map_err(internal_error)?;
        chain
            .apply_persisted_managed_vector_sidecars()
            .map_err(internal_error)?;
        let arc = Arc::new(RwLock::new(chain));
        state.chains.insert(target_chain_key.clone(), arc.clone());
        arc
    };

    let mut dst_chain = dst_arc.write().await;

    // Guard: reject if the agent already has thoughts on the target chain.
    let already_exists = dst_chain.thoughts().iter().any(|t| t.agent_id == agent_id);
    if already_exists {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!(
                    "agent '{agent_id}' already has thoughts on chain '{target_chain_key}'; \
                     copying would create a diverged history"
                )
            })),
        ));
    }

    if !agent_name.is_empty() || agent_owner.is_some() || agent_description.is_some() {
        dst_chain
            .upsert_agent(
                &agent_id,
                (!agent_name.is_empty()).then_some(agent_name.as_str()),
                agent_owner.as_deref(),
                agent_description.as_deref(),
                None,
            )
            .map_err(|e| internal_error(format!("upsert target agent metadata: {e}")))?;
    }

    let mut copied = 0usize;
    for input in agent_thoughts {
        dst_chain
            .append_thought(&agent_id, input)
            .map_err(|e| internal_error(format!("append thought: {e}")))?;
        copied += 1;
    }

    Ok(Json(json!({ "copied": copied })))
}

// ── API: merge chains ─────────────────────────────────────────────────────────

/// Request body for `POST /dashboard/api/chains/merge`.
#[derive(Deserialize)]
struct MergeChainsRequest {
    /// Chain key of the source chain whose thoughts will be moved to the target.
    source_chain_key: String,
    /// Chain key of the target chain that receives the merged thoughts.
    target_chain_key: String,
}

/// Success response for `POST /dashboard/api/chains/merge`.
#[derive(Serialize)]
struct MergeChainsResponse {
    /// Total number of thoughts successfully appended to the target chain.
    thoughts_copied: usize,
    /// Number of distinct source agents that were remapped to target agents.
    agents_remapped: usize,
    /// Always `true` when the response is 200 — the source chain has been deleted.
    source_deleted: bool,
}

/// `POST /dashboard/api/chains/merge`
///
/// Merges all thoughts from `source_chain_key` into `target_chain_key`, then
/// permanently deletes the source chain.
///
/// Agent identity remapping is autonomous: for each agent that wrote thoughts on
/// the source chain the handler finds the closest-matching agent already present
/// on the target chain (scored by character-set similarity between agent IDs).
/// No new agent identities are created on the target chain.
///
/// # Error conditions
///
/// - `400` when `source_chain_key == target_chain_key`.
/// - `400` when the target chain does not exist.
/// - `500` when any individual `append_thought` fails.  In that case the source
///   chain is **not** deleted so no data is lost.
async fn api_merge_chains(
    State(state): State<DashboardState>,
    Json(body): Json<MergeChainsRequest>,
) -> Result<Json<MergeChainsResponse>, (StatusCode, Json<Value>)> {
    let source_key = &body.source_chain_key;
    let target_key = &body.target_chain_key;

    if source_key == target_key {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "source and target chain must differ" })),
        ));
    }

    // The target chain must already exist — we never create it here.
    let target_arc = get_or_open_chain(&state, target_key).await.map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("target chain '{target_key}' does not exist") })),
        )
    })?;

    // Open the source chain (read-only traversal to collect agents).
    let source_arc = get_or_open_chain(&state, source_key).await?;

    // Collect the distinct source agent IDs from the source chain.
    let source_agent_ids: Vec<String> = {
        let src = source_arc.read().await;
        let mut ids: HashMap<String, ()> = HashMap::new();
        for t in src.thoughts() {
            ids.entry(t.agent_id.clone()).or_insert(());
        }
        ids.into_keys().collect()
    };

    // Build agent_id → thought_count map for the target chain.
    let target_agent_thought_counts: HashMap<String, u64> = {
        let tgt = target_arc.read().await;
        let mut counts: HashMap<String, u64> = HashMap::new();
        for t in tgt.thoughts() {
            *counts.entry(t.agent_id.clone()).or_insert(0) += 1;
        }
        counts
    };

    // Get the set of agent IDs present on the target chain.
    let target_agent_ids: Vec<String> = {
        let tgt = target_arc.read().await;
        tgt.agent_registry().agents.keys().cloned().collect()
    };

    // Build the source→target agent remapping.
    //
    // Scoring: Jaccard similarity on the character sets of the two agent IDs.
    // Tie-break: prefer the target agent with more thoughts on the target chain.
    // Guarantee: there is always a winner (at least one target agent exists
    // since the target chain must be non-empty to have been registered).
    let agent_remap: HashMap<String, String> = {
        let mut remap = HashMap::new();

        for src_id in &source_agent_ids {
            // Exact match wins immediately.
            if target_agent_ids.contains(src_id) {
                remap.insert(src_id.clone(), src_id.clone());
                continue;
            }

            // Jaccard similarity on character sets.
            let src_chars: std::collections::HashSet<char> = src_id.chars().collect();

            let best = target_agent_ids.iter().max_by(|a, b| {
                let score_a = jaccard_char_similarity(&src_chars, a);
                let score_b = jaccard_char_similarity(&src_chars, b);
                let cmp = score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal);
                if cmp == std::cmp::Ordering::Equal {
                    // Tie-break: higher thought count wins.
                    let count_a = target_agent_thought_counts.get(*a).copied().unwrap_or(0);
                    let count_b = target_agent_thought_counts.get(*b).copied().unwrap_or(0);
                    count_a.cmp(&count_b)
                } else {
                    cmp
                }
            });

            if let Some(tgt_id) = best {
                remap.insert(src_id.clone(), tgt_id.clone());
            }
            // If the target chain has no agents at all, we fall back: the
            // thought is written under the original source agent_id.  This
            // keeps the contract that we never return an error for unmapped
            // agents.
        }

        remap
    };

    let agents_remapped = agent_remap.iter().filter(|(src, tgt)| src != tgt).count();

    // Reconstruct a parallel list of (remapped_agent_id, ThoughtInput) pairs
    // from the original source thoughts, preserving order.
    let remapped_thoughts: Vec<(String, ThoughtInput)> = {
        let src = source_arc.read().await;
        src.thoughts()
            .iter()
            .map(|t| {
                let mapped_id = agent_remap
                    .get(&t.agent_id)
                    .cloned()
                    .unwrap_or_else(|| t.agent_id.clone());
                let mut input = ThoughtInput::new(t.thought_type, t.content.clone());
                input.role = t.role;
                input.importance = t.importance;
                input.confidence = t.confidence;
                input.tags = t.tags.clone();
                input.concepts = t.concepts.clone();
                (mapped_id, input)
            })
            .collect()
    };

    // Append all thoughts to the target chain.
    // On the first error we abort without deleting the source chain.
    let mut thoughts_copied = 0usize;
    {
        let mut tgt = target_arc.write().await;
        for (agent_id, input) in remapped_thoughts {
            tgt.append_thought(&agent_id, input)
                .map_err(|e| internal_error(format!("append thought to target chain: {e}")))?;
            thoughts_copied += 1;
        }
    }

    // All thoughts successfully appended — now delete the source chain.
    // Evict from the in-memory cache first so no new writes can sneak in.
    if let Some((_, arc)) = state.chains.remove(source_key) {
        let mut chain = arc.write().await;
        chain.detach_persistence();
    }
    deregister_chain(&state.mentisdb_dir, source_key).map_err(internal_error)?;

    Ok(Json(MergeChainsResponse {
        thoughts_copied,
        agents_remapped,
        source_deleted: true,
    }))
}

/// Compute Jaccard similarity between a pre-built character set for the source
/// string and the character set derived from `target`.
///
/// Returns a value in `[0.0, 1.0]` where `1.0` means identical character sets
/// and `0.0` means disjoint sets.
fn jaccard_char_similarity(src_chars: &std::collections::HashSet<char>, target: &str) -> f64 {
    let tgt_chars: std::collections::HashSet<char> = target.chars().collect();
    let intersection = src_chars.intersection(&tgt_chars).count();
    let union = src_chars.union(&tgt_chars).count();
    if union == 0 {
        1.0
    } else {
        intersection as f64 / union as f64
    }
}
