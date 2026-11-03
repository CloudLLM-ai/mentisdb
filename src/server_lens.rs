//! Who is reading: the per-request [`ReadAuthority`] and the [`Audience`] it
//! yields for each chain.
//!
//! The REST and MCP routers wrap every request in `read_auth_middleware` (in `server`).
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
//!
//! # Replay protection happens after authorization
//!
//! A proof is verified against the public key *embedded in its own header*, so
//! any fresh keypair produces a valid one. The one-time-use nonce is therefore
//! recorded only when a proof is about to grant something: the first time
//! [`audience_for`] finds the key authorized on a chain with a private segment,
//! once per request. A proof from a key no chain knows, or a proof that is
//! only used on chains without private segments, never touches the replay
//! cache, so self-signed floods cannot fill it. A proof whose nonce is already
//! spent is refused with `401` by a read-only lookup (which records nothing).
//! One that loses a race for its nonce, or whose key has used up its share of a
//! full cache, reads as [`Audience::Public`] instead of failing the request.

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
use std::sync::{Arc, OnceLock};

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
        Some(key)
            if chain.is_read_authorized(&key.agent_id, &key.key_id, &key.public_key)
                && proof_is_fresh() =>
        {
            Audience::Authorized
        }
        _ => Audience::Public,
    }
}

/// The one-time-use claim of the current request's read proof.
struct ProofClaim {
    cache: &'static NonceCache,
    public_key: [u8; 32],
    nonce: String,
    now: i64,
    /// Whether the nonce was accepted; decided once, on first use.
    accepted: OnceLock<bool>,
}

tokio::task_local! {
    static PROOF_CLAIM: Option<Arc<ProofClaim>>;
}

/// Spend the current request's nonce (once) and report whether it was fresh.
///
/// `true` outside the HTTP middleware, where there is no nonce to spend.
fn proof_is_fresh() -> bool {
    PROOF_CLAIM
        .try_with(|claim| {
            claim.as_ref().is_none_or(|claim| {
                *claim.accepted.get_or_init(|| {
                    claim
                        .cache
                        .check_and_insert(&claim.public_key, &claim.nonce, claim.now)
                        .is_ok()
                })
            })
        })
        .unwrap_or(true)
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

/// Live nonces remembered in total.
const NONCE_CACHE_CAPACITY: usize = 100_000;
/// Live nonces one key may hold (about 200 proofs per second sustained), so
/// one authorized key cannot crowd the others out of the cache.
const NONCE_CACHE_PER_KEY: usize = NONCE_CACHE_CAPACITY / 4;

fn nonces() -> &'static NonceCache {
    static NONCES: OnceLock<NonceCache> = OnceLock::new();
    NONCES.get_or_init(|| NonceCache::with_per_key_limit(NONCE_CACHE_CAPACITY, NONCE_CACHE_PER_KEY))
}

fn reject(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Verify the optional read proof and run the request with the resulting
/// authority.
///
/// Requests without the header pass through untouched with an empty authority.
/// A request that carries the header but fails verification (grammar, clock
/// window, signature) is rejected with `401` rather than silently downgraded,
/// so a client with a stale clock learns why it sees less than it expects.
/// Error text never includes key material.
///
/// A proof whose nonce was already spent is also refused with `401`, by a
/// read-only lookup. The nonce itself is *spent* later, when the proof first
/// grants access to a private segment (see the module docs); losing a race
/// against a concurrent use of the same proof, or a full cache, then yields the
/// public view.
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
    let proof = match verify_request(&header, &method, &target, &body, now) {
        Ok(proof) => proof,
        Err(error) => return fail(StatusCode::UNAUTHORIZED, &error.to_string()),
    };
    // Read-only: a proof already spent for a granted read is refused outright
    // (so a client that reuses one learns why). Nothing is recorded here.
    if nonces().is_replayed(&proof.public_key, &proof.nonce, now) {
        return fail(
            StatusCode::UNAUTHORIZED,
            &ReadAuthError::Replayed.to_string(),
        );
    }
    let claim = Arc::new(ProofClaim {
        cache: nonces(),
        public_key: proof.public_key,
        nonce: proof.nonce.clone(),
        now,
        accepted: OnceLock::new(),
    });
    let authority = ReadAuthority {
        key: Some(VerifiedReadKey {
            agent_id: proof.agent_id,
            key_id: proof.key_id,
            public_key: proof.public_key,
        }),
        operator: false,
    };
    let request = Request::from_parts(parts, Body::from(body));
    PROOF_CLAIM
        .scope(
            Some(claim),
            with_read_authority(authority, next.run(request)),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(cache: &'static NonceCache, key: u8, nonce: &str) -> Option<Arc<ProofClaim>> {
        Some(Arc::new(ProofClaim {
            cache,
            public_key: [key; 32],
            nonce: nonce.to_owned(),
            now: 1_000,
            accepted: OnceLock::new(),
        }))
    }

    /// A full replay cache never fails a request: the proof just stops
    /// granting access (the caller reads the public view), and the outcome is
    /// decided once per request.
    #[tokio::test]
    async fn a_full_cache_degrades_the_proof_instead_of_failing() {
        let cache: &'static NonceCache = Box::leak(Box::new(NonceCache::new(1)));
        cache
            .check_and_insert(&[9; 32], "ffffffffffffffff", 1_000)
            .unwrap();
        let outcome = PROOF_CLAIM
            .scope(claim(cache, 1, "aaaaaaaaaaaaaaaa"), async {
                (proof_is_fresh(), proof_is_fresh())
            })
            .await;
        assert_eq!(outcome, (false, false));
        assert_eq!(cache.len(), 1, "a refused proof is not stored");
    }

    #[tokio::test]
    async fn a_fresh_proof_is_spent_once_per_request() {
        let cache: &'static NonceCache = Box::leak(Box::new(NonceCache::new(8)));
        let first = PROOF_CLAIM
            .scope(claim(cache, 1, "aaaaaaaaaaaaaaaa"), async {
                (proof_is_fresh(), proof_is_fresh())
            })
            .await;
        assert_eq!(first, (true, true), "later chains of one request reuse it");
        assert_eq!(cache.len(), 1);
        let replay = PROOF_CLAIM
            .scope(claim(cache, 1, "aaaaaaaaaaaaaaaa"), async {
                proof_is_fresh()
            })
            .await;
        assert!(!replay, "the same proof on a second request is a replay");
    }

    #[tokio::test]
    async fn without_a_claim_there_is_nothing_to_spend() {
        assert!(proof_is_fresh());
        assert!(PROOF_CLAIM.scope(None, async { proof_is_fresh() }).await);
    }
}
