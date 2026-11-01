//! Who is reading: the per-request [`ReadAuthority`] and the [`Audience`] it
//! yields for each chain.
//!
//! The REST and MCP routers wrap every request in [`read_auth_middleware`].
//! When the request carries a valid read proof ([`crate::read_auth`]) the
//! verified key becomes the request's authority for the duration of the
//! handler; otherwise the authority is empty. Handlers never look at headers.
//! They ask [`audience_for`] for the chain they are about to read and pass the
//! result down to the library (`ThoughtQuery::audience` and friends).
//!
//! The default is the public view: code that runs outside the middleware
//! (a unit test calling a service method, a new transport nobody wired up)
//! reads with [`Audience::Public`], so forgetting to authenticate loses data
//! instead of leaking it.
//!
//! The authority proves *who* is asking. Whether that key may read a chain is
//! decided per chain by [`MentisDb::is_read_authorized`], against the chain's
//! current policy and registry.

use crate::audience::Audience;
use crate::auth::BearerTokenScope;
use crate::read_auth::{
    verify_request, NonceCache, ReadAuthError, MAX_READ_AUTH_HEADER_LEN, READ_AUTH_HEADER,
};
use crate::MentisDb;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::OnceLock;

/// Largest body the middleware buffers to hash a signed request (matches the
/// bearer-token middleware limit).
pub(crate) const MAX_SIGNED_BODY_BYTES: usize = 1024 * 1024;

/// A key whose possession the request proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReadKey {
    /// Agent the key is registered to.
    pub agent_id: String,
    /// Key id within the agent.
    pub key_id: String,
    /// Raw Ed25519 public key.
    pub public_key: [u8; 32],
}

/// What a request proved about its caller.
///
/// `ReadAuthority::default()` proves nothing and reads as [`Audience::Public`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadAuthority {
    /// The key the caller proved possession of, if any.
    pub key: Option<VerifiedReadKey>,
    /// `true` for the local operator (proved with the operator credential);
    /// reads everything, like anyone with the data directory can.
    pub operator: bool,
}

tokio::task_local! {
    static READ_AUTHORITY: ReadAuthority;
}

/// Run `future` with `authority` as the request authority.
///
/// The REST/MCP middleware does this for every request; embedders that call
/// service methods themselves use it to read private segments.
pub async fn with_read_authority<F: std::future::Future>(
    authority: ReadAuthority,
    future: F,
) -> F::Output {
    READ_AUTHORITY.scope(authority, future).await
}

/// The authority of the current request; empty outside [`with_read_authority`].
pub fn current_authority() -> ReadAuthority {
    READ_AUTHORITY.try_with(Clone::clone).unwrap_or_default()
}

/// The audience the current request has on `chain`.
///
/// [`Audience::Authorized`] when the chain has no private segment (every
/// reader is equivalent), when the caller is the operator, or when the key the
/// caller proved is authorized to read `chain`; otherwise [`Audience::Public`].
pub fn audience_for(chain: &MentisDb) -> Audience {
    if !chain.segments().has_private() {
        return Audience::Authorized;
    }
    let authority = current_authority();
    if authority.operator {
        return Audience::Authorized;
    }
    match &authority.key {
        Some(key) if chain.is_read_authorized(&key.agent_id, &key.key_id, &key.public_key) => {
            Audience::Authorized
        }
        _ => Audience::Public,
    }
}

tokio::task_local! {
    static REQUEST_SCOPE: Option<BearerTokenScope>;
}

/// Run `future` with the bearer-token scope the request authenticated with.
///
/// The bearer-token middlewares do this once a token is resolved, so code deep
/// in a handler (ancestor-chain search) can still honor the token's chains.
pub(crate) async fn with_request_scope<F: std::future::Future>(
    scope: Option<BearerTokenScope>,
    future: F,
) -> F::Output {
    REQUEST_SCOPE.scope(scope, future).await
}

/// Whether the token the current request authenticated with may touch
/// `chain_key`. Always `true` when no scope is in effect (bearer enforcement
/// off, or the caller is not behind the HTTP middleware).
pub fn scope_allows_chain(chain_key: &str) -> bool {
    REQUEST_SCOPE
        .try_with(|scope| {
            scope
                .as_ref()
                .is_none_or(|scope| scope.allows_chain(chain_key))
        })
        .unwrap_or(true)
}

/// Invalid read proofs allowed per peer address per minute before further
/// requests from it are refused outright. A valid proof costs the server one
/// signature check; this bounds what a peer can make it spend on invalid ones.
const MAX_INVALID_PROOFS_PER_MINUTE: u32 = 30;
/// Peer addresses tracked at once; the table is cleared when it fills.
const MAX_TRACKED_PEERS: usize = 4096;

fn invalid_proofs(
) -> &'static std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (u32, std::time::Instant)>>
{
    static TABLE: OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (u32, std::time::Instant)>>,
    > = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

fn proof_budget_exhausted(peer: std::net::IpAddr) -> bool {
    let table = invalid_proofs().lock().unwrap_or_else(|e| e.into_inner());
    table.get(&peer).is_some_and(|(count, since)| {
        since.elapsed() < std::time::Duration::from_secs(60)
            && *count >= MAX_INVALID_PROOFS_PER_MINUTE
    })
}

fn record_invalid_proof(peer: std::net::IpAddr) {
    let mut table = invalid_proofs().lock().unwrap_or_else(|e| e.into_inner());
    if table.len() >= MAX_TRACKED_PEERS && !table.contains_key(&peer) {
        table.clear();
    }
    let entry = table.entry(peer).or_insert((0, std::time::Instant::now()));
    if entry.1.elapsed() >= std::time::Duration::from_secs(60) {
        *entry = (0, std::time::Instant::now());
    }
    entry.0 += 1;
}

fn nonces() -> &'static NonceCache {
    static NONCES: OnceLock<NonceCache> = OnceLock::new();
    NONCES.get_or_init(|| NonceCache::new(100_000))
}

fn reject(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Verify the optional read proof and run the request with the resulting
/// authority.
///
/// Requests without the header pass through untouched with an empty authority.
/// A request that carries the header but fails verification is rejected with
/// `401` rather than silently downgraded, so a client with a stale clock learns
/// why it sees less than it expects. Error text never includes key material.
pub(crate) async fn read_auth_middleware(request: Request, next: Next) -> Response {
    let Some(value) = request.headers().get(READ_AUTH_HEADER) else {
        return with_read_authority(ReadAuthority::default(), next.run(request)).await;
    };
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip());
    if peer.is_some_and(proof_budget_exhausted) {
        return reject(
            StatusCode::TOO_MANY_REQUESTS,
            "too many invalid read proofs from this address; try again in a minute",
        );
    }
    let fail = |status: StatusCode, message: &str| {
        if let Some(peer) = peer {
            record_invalid_proof(peer);
        }
        reject(status, message)
    };
    let Ok(header) = value.to_str().map(str::to_owned) else {
        return fail(StatusCode::UNAUTHORIZED, "malformed read proof");
    };
    if header.len() > MAX_READ_AUTH_HEADER_LEN {
        return fail(StatusCode::UNAUTHORIZED, "malformed read proof");
    }
    let (parts, body) = request.into_parts();
    let body = match axum::body::to_bytes(body, MAX_SIGNED_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return reject(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request body too large to authenticate",
            )
        }
    };
    let method = parts.method.as_str().to_owned();
    let target = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str().to_owned())
        .unwrap_or_else(|| parts.uri.path().to_owned());
    let now = chrono::Utc::now().timestamp();
    let verified = verify_request(&header, &method, &target, &body, now)
        .and_then(|proof| {
            nonces().check_and_insert(&proof.public_key, &proof.nonce, now)?;
            Ok(proof)
        })
        .map_err(|error| match error {
            ReadAuthError::CacheFull => (StatusCode::SERVICE_UNAVAILABLE, error.to_string()),
            other => (StatusCode::UNAUTHORIZED, other.to_string()),
        });
    let proof = match verified {
        Ok(proof) => proof,
        Err((status, message)) => return fail(status, &message),
    };
    let authority = ReadAuthority {
        key: Some(VerifiedReadKey {
            agent_id: proof.agent_id,
            key_id: proof.key_id,
            public_key: proof.public_key,
        }),
        operator: false,
    };
    let request = Request::from_parts(parts, Body::from(body));
    with_read_authority(authority, next.run(request)).await
}
