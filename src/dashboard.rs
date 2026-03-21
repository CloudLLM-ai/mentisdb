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
    load_registered_chains, AgentRecord, AgentStatus, MentisDb, PublicKeyAlgorithm, SkillFormat,
    SkillRegistry, StorageAdapterKind, Thought, ThoughtType,
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
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
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
        // Thoughts for a chain
        .route("/chains/{chain_key}/thoughts", get(api_chain_thoughts))
        // Single thought lookup
        .route("/thoughts/{chain_key}/{thought_id}", get(api_get_thought))
        // Thoughts for an agent within a chain
        .route(
            "/chains/{chain_key}/agents/{agent_id}/thoughts",
            get(api_agent_thoughts),
        )
        // Agent listing — all chains
        .route("/agents", get(api_agents_all))
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
        // Skill listing and reading
        .route("/skills", get(api_skills))
        .route("/skills/{skill_id}", get(api_get_skill))
        .route("/skills/{skill_id}/versions", get(api_skill_versions))
        .route("/skills/{skill_id}/diff", get(api_skill_diff));

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

/// Form body for the `/dashboard/login` POST.
#[derive(Deserialize)]
struct LoginForm {
    pin: String,
}

/// Handle a login form submission.
///
/// On success sets the `mentisdb_pin` cookie and redirects to `/dashboard`.
/// On failure redirects back to `/dashboard/login?error=1`.
async fn handle_login(
    State(state): State<DashboardState>,
    Form(form): Form<LoginForm>,
) -> Response {
    let pin_matches = state
        .dashboard_pin
        .as_deref()
        .map(|required| form.pin == required)
        .unwrap_or(true); // No PIN configured → any submission succeeds.

    if pin_matches {
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

/// Look up a chain in the live cache; fall back to opening it from disk.
///
/// The opened chain is inserted into `state.chains` so subsequent requests
/// can reuse it without touching the file system.
async fn get_or_open_chain(
    state: &DashboardState,
    chain_key: &str,
) -> Result<Arc<RwLock<MentisDb>>, (StatusCode, Json<Value>)> {
    // Try the live cache first (clone the Arc to avoid holding the DashMap shard lock across an await).
    if let Some(arc) = state.chains.get(chain_key).map(|r| r.value().clone()) {
        return Ok(arc);
    }

    // Open from disk.
    let chain = MentisDb::open_with_key_and_storage_kind(
        &state.mentisdb_dir,
        chain_key,
        state.default_storage_adapter,
    )
    .map_err(|e| not_found(format!("chain '{chain_key}': {e}")))?;

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
        "Surprise" => Some(ThoughtType::Surprise),
        _ => None,
    }
}

// ── API response shape helpers ────────────────────────────────────────────────

/// Serialise a page of thoughts alongside pagination metadata.
///
/// When `reverse` is `true` the slice is returned newest-first (descending by
/// append index). Pagination is applied *after* reversing so that page 1 is
/// always the logical "first" page in the chosen order.
fn paginated_thoughts(mut thoughts: Vec<&Thought>, page: usize, per_page: usize, reverse: bool) -> Value {
    if reverse {
        thoughts.reverse();
    }
    let total = thoughts.len();
    let pages = if per_page == 0 {
        0
    } else {
        total.div_ceil(per_page)
    };

    let start = ((page.saturating_sub(1)) * per_page).min(total);
    let slice: Vec<&Thought> = thoughts.into_iter().skip(start).take(per_page).collect();

    json!({
        "thoughts": slice,
        "total": total,
        "page": page,
        "per_page": per_page,
        "pages": pages,
    })
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
/// Returns a JSON array of chain summaries loaded from the on-disk registry.
/// Live counts are pulled from the in-memory cache when available.
async fn api_chains(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;

    let mut chains = Vec::with_capacity(registry.chains.len());

    for (chain_key, reg) in &registry.chains {
        // Clone the Arc before awaiting to avoid holding the DashMap shard lock.
        let cached = state.chains.get(chain_key).map(|r| r.value().clone());

        let (thought_count, agent_count, head_hash) = if let Some(arc) = cached {
            let chain = arc.read().await;
            (
                chain.thoughts().len(),
                chain.agent_registry().agents.len(),
                chain.head_hash().map(ToString::to_string),
            )
        } else {
            (reg.thought_count as usize, reg.agent_count, None)
        };

        chains.push(json!({
            "chain_key": chain_key,
            "thought_count": thought_count,
            "agent_count": agent_count,
            "head_hash": head_hash,
        }));
    }

    Ok(Json(json!(chains)))
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

    let type_filter: Option<Vec<ThoughtType>> = params
        .types
        .as_deref()
        .map(|raw| raw.split(',').filter_map(parse_thought_type).collect());

    let thoughts: Vec<&Thought> = chain
        .thoughts()
        .iter()
        .filter(|t| {
            type_filter
                .as_ref()
                .map(|types| types.contains(&t.thought_type))
                .unwrap_or(true)
        })
        .collect();

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let reverse = params.order.as_deref().unwrap_or("desc") != "asc";

    Ok(Json(paginated_thoughts(thoughts, page, per_page, reverse)))
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

    let thought = chain
        .thoughts()
        .iter()
        .find(|t| t.id == thought_id)
        .ok_or_else(|| {
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

    let type_filter: Option<Vec<ThoughtType>> = params
        .types
        .as_deref()
        .map(|raw| raw.split(',').filter_map(parse_thought_type).collect());

    let thoughts: Vec<&Thought> = chain
        .thoughts()
        .iter()
        .filter(|t| {
            t.agent_id == agent_id
                && type_filter
                    .as_ref()
                    .map(|types| types.contains(&t.thought_type))
                    .unwrap_or(true)
        })
        .collect();

    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(50).max(1);
    let reverse = params.order.as_deref().unwrap_or("desc") != "asc";

    Ok(Json(paginated_thoughts(thoughts, page, per_page, reverse)))
}

// ── API: agents ───────────────────────────────────────────────────────────────

/// `GET /dashboard/api/agents`
///
/// Returns all registered agents across all known chains, keyed by chain key.
async fn api_agents_all(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let registry = load_registered_chains(&state.mentisdb_dir).map_err(internal_error)?;

    let mut result: BTreeMap<String, Vec<AgentRecord>> = BTreeMap::new();

    for chain_key in registry.chains.keys() {
        match get_or_open_chain(&state, chain_key).await {
            Ok(arc) => {
                let chain = arc.read().await;
                let agents: Vec<AgentRecord> =
                    chain.agent_registry().agents.values().cloned().collect();
                result.insert(chain_key.clone(), agents);
            }
            Err(_) => {
                result.insert(chain_key.clone(), Vec::new());
            }
        }
    }

    Ok(Json(serde_json::to_value(result).map_err(internal_error)?))
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
    let agents: Vec<&AgentRecord> = chain.agent_registry().agents.values().collect();
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
    Ok(Json(serde_json::to_value(agent).map_err(internal_error)?))
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

/// `GET /dashboard/api/skills`
///
/// Returns a summary list of all registered skills.
async fn api_skills(
    State(state): State<DashboardState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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
    let skills = state.skills.read().await;

    let summary = skills
        .list_skills()
        .into_iter()
        .find(|s| s.skill_id == skill_id)
        .ok_or_else(|| not_found(format!("skill '{skill_id}' not found")))?;

    let markdown = skills
        .read_skill(&skill_id, None, SkillFormat::Markdown)
        .map_err(internal_error)?;

    Ok(Json(json!({ "summary": summary, "markdown": markdown })))
}

/// `GET /dashboard/api/skills/:skill_id/versions`
///
/// Returns the full version history for a skill.
async fn api_skill_versions(
    State(state): State<DashboardState>,
    Path(skill_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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

    let patch = diffy::create_patch(&old_content, &new_content);
    Ok(Json(json!({ "diff": patch.to_string() })))
}
