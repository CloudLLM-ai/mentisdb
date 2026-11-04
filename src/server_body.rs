//! Admission control for request bodies: how many bytes of body the daemon is
//! willing to hold, for whom, and for how long.
//!
//! Reading a request body costs memory and a connection slot. Without limits
//! one unauthenticated peer can pin the largest body the daemon accepts, and
//! many peers (or one peer with many connections) can multiply that. Every REST
//! and MCP request therefore passes through `body_admission_middleware`, the
//! outermost layer, before any authentication step reads its body.
//!
//! # Caps
//!
//! Ordinary routes accept at most [`SMALL_BODY_BYTES`] (2 MiB). The routes in
//! [`LARGE_BODY_ROUTES`] accept up to `MENTISDB_MAX_BODY_BYTES` (default 64
//! MiB), but with bearer-token access on only from a caller with a valid token;
//! a request with no valid token keeps the small cap. A declared `Content-Length`
//! over the cap is refused with `413` at once; a body without one (chunked,
//! HTTP/2) is cut off with `413` when it crosses the cap. MCP bodies
//! (`tools/call` and the rest of the JSON-RPC traffic) are always small: a large
//! import goes through the REST import routes (or the CLI, which uses them).
//!
//! # Budgets
//!
//! Three classes of request draw on separate pools, so imports can never starve
//! ordinary traffic:
//!
//! * **Large** (an import route, from an allowed caller): the *large pool*
//!   (`MENTISDB_MAX_INFLIGHT_BODY_BYTES`, default four times the body cap), of
//!   which one peer may hold twice the cap and at most four bodies at a time.
//!   Its declared length is reserved when it arrives, so an import that cannot
//!   fit is refused at once with `503` and `Retry-After`.
//! * **Small** (everything else, from a caller that is not anonymous): the *small
//!   pool*, 64 MiB for all of them, charged only for the bytes that actually
//!   arrive, never for a declared length. One peer may hold at most half of it
//!   (32 MiB): on a loopback bind every local agent is the same peer, so the share
//!   is large, but with bearer-token access off (where every caller is alike) one
//!   peer cannot take the whole pool.
//! * **Anonymous** (bearer-token access on, no valid token): a pool of their own,
//!   16 MiB for all of them, so they can never draw on what authenticated traffic
//!   uses; one peer may hold at most 8 MiB of it, charged as bytes arrive.
//!
//! A reservation is returned as soon as the body has been read to its end (or
//! the request ends, whichever is first); a handler that goes on to hold the
//! parsed body is bounded by its route cap, not by the budget.
//!
//! # Time
//!
//! A body that delivers nothing for `MENTISDB_BODY_IDLE_TIMEOUT_SECS` (default
//! 30) fails. Because that timer restarts on every chunk, small and anonymous
//! bodies also get a total deadline of twice the idle timeout, and every body,
//! large ones included, must reach at least [`MIN_BODY_BYTES_PER_SEC`] (64 KiB/s)
//! in every window of one idle timeout once its grace period (twice the idle
//! timeout) is over. The rate is judged window by window, so credit cannot be
//! banked: a client that sends nearly everything at once and then drips one byte
//! per chunk, each just inside the idle timeout, fails with `408` at the end of
//! the next window instead of holding a connection and its charge for as long as
//! it likes.
//! Failures are `408` (too slow) and `503` with `Retry-After` (budget),
//! whether the failure surfaces in a bearer-token middleware or in a handler's own
//! body extractor (bearer-token access off).
//!
//! Who may use the large cap is decided from the request headers alone (a valid
//! bearer token), never from the body. A read proof (`X-Mentis-Auth`) is checked
//! per chain, and the chain is named inside the body, so a proof from a key
//! nobody registered looks the same as one from an authorized key until the body
//! has been read; proof requests are therefore bounded by the budgets above
//! rather than by a smaller cap.

use crate::server::MentisDbService;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::Semaphore;

/// Body cap, in bytes, of every route not in [`LARGE_BODY_ROUTES`], and of any
/// route for a caller without a valid token: 2 MiB (axum's own default).
pub const SMALL_BODY_BYTES: usize = 2 * 1024 * 1024;

/// REST routes that may carry a body up to `MENTISDB_MAX_BODY_BYTES`: the memory
/// import, skill upload and memory extraction endpoints, which take whole files.
/// Every other route is held to [`SMALL_BODY_BYTES`]. MCP is not listed: its
/// `tools/call` bodies (including `mentisdb_import_memory_markdown`) are capped at
/// 2 MiB, so a large import goes through these REST routes.
pub const LARGE_BODY_ROUTES: [&str; 3] = [
    "/v1/import-markdown",
    "/v1/skills/upload",
    "/v1/extract-memories",
];

/// Granularity of the budget: permits are KiB so a byte count always fits.
const UNIT: usize = 1024;

/// Bytes of body all small and anonymous requests may hold at once: 64 MiB.
pub const SMALL_POOL_BYTES: usize = 64 * 1024 * 1024;

/// Bytes of body all anonymous requests (bearer-token access on, no valid token)
/// may hold at once: 16 MiB, a pool of their own so they can never draw on the
/// small pool authenticated traffic uses.
pub const ANONYMOUS_POOL_BYTES: usize = 16 * 1024 * 1024;

/// Bytes of the anonymous pool one peer may hold: 8 MiB.
pub const ANONYMOUS_PEER_BYTES: usize = 8 * 1024 * 1024;

/// Bytes of the small pool one peer may hold: 32 MiB, so that with bearer-token
/// access off (where every caller is alike) one peer cannot take the whole pool.
pub const SMALL_PEER_BYTES: usize = 32 * 1024 * 1024;

/// Slowest average rate, in bytes per second, a body may arrive at once its grace
/// period (twice the idle timeout) is over: 64 KiB/s. A body that goes on arriving
/// just often enough to beat the idle timer, but slower than this, fails with `408`.
pub const MIN_BODY_BYTES_PER_SEC: usize = 64 * 1024;

/// Large bodies one peer may have in flight at once.
pub const LARGE_BODIES_PER_PEER: usize = 4;

/// Whether `path` may carry a large body, see [`LARGE_BODY_ROUTES`].
pub fn is_large_body_route(path: &str) -> bool {
    LARGE_BODY_ROUTES.contains(&path)
}

/// Which pool a request draws on, see the [module documentation](crate::server_body).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyClass {
    /// An import route from an allowed caller.
    Large,
    /// Anything else from a caller that is not anonymous.
    Small,
    /// Bearer-token access is on and the caller has no valid token.
    Anonymous,
}

#[derive(Debug, Default)]
struct PeerUse {
    large_units: usize,
    large_bodies: usize,
    anonymous_units: usize,
    small_units: usize,
}

impl PeerUse {
    /// Nothing is charged to this peer: its map entry can go.
    fn is_idle(&self) -> bool {
        self.large_units == 0
            && self.large_bodies == 0
            && self.anonymous_units == 0
            && self.small_units == 0
    }
}

/// The in-flight body budget shared by every request of one service.
#[derive(Debug)]
pub(crate) struct BodyBudget {
    large: Arc<Semaphore>,
    small: Arc<Semaphore>,
    anonymous: Arc<Semaphore>,
    per_peer_large_units: usize,
    peers: Mutex<HashMap<IpAddr, PeerUse>>,
}

fn units(bytes: usize) -> usize {
    bytes.div_ceil(UNIT).min(u32::MAX as usize)
}

impl BodyBudget {
    /// A large pool of `large_bytes`, of which one peer may hold `per_peer_bytes`;
    /// the small pool is fixed ([`SMALL_POOL_BYTES`]).
    pub(crate) fn new(large_bytes: usize, per_peer_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            large: Arc::new(Semaphore::new(units(large_bytes))),
            small: Arc::new(Semaphore::new(units(SMALL_POOL_BYTES))),
            anonymous: Arc::new(Semaphore::new(units(ANONYMOUS_POOL_BYTES))),
            per_peer_large_units: units(per_peer_bytes).min(units(large_bytes)),
            peers: Mutex::new(HashMap::new()),
        })
    }

    /// Start an (empty) reservation for a request of `class` from `peer`; `None`
    /// when the peer already has [`LARGE_BODIES_PER_PEER`] large bodies in flight.
    fn pool(&self, class: BodyClass) -> &Arc<Semaphore> {
        match class {
            BodyClass::Large => &self.large,
            BodyClass::Small => &self.small,
            BodyClass::Anonymous => &self.anonymous,
        }
    }

    /// Drop `peer`'s map entry if nothing is charged to it, so the map holds
    /// only peers with a body in flight (it cannot grow with address rotation).
    fn prune(peers: &mut HashMap<IpAddr, PeerUse>, peer: IpAddr) {
        if peers.get(&peer).is_some_and(PeerUse::is_idle) {
            peers.remove(&peer);
        }
    }

    fn reservation(
        self: &Arc<Self>,
        class: BodyClass,
        peer: Option<IpAddr>,
    ) -> Option<Arc<Reservation>> {
        if let (BodyClass::Large, Some(peer)) = (class, peer) {
            let mut peers = self.peers.lock().unwrap_or_else(|e| e.into_inner());
            let used = peers.entry(peer).or_default();
            if used.large_bodies >= LARGE_BODIES_PER_PEER {
                return None;
            }
            used.large_bodies += 1;
        }
        Some(Arc::new(Reservation {
            budget: Arc::clone(self),
            class,
            peer,
            held_units: AtomicUsize::new(0),
            stop: Mutex::new(None),
        }))
    }
}

/// The bytes one request has charged to the budget, returned when it is dropped
/// or [`released`](Reservation::release) after its body has been read.
#[derive(Debug)]
pub(crate) struct Reservation {
    budget: Arc<BodyBudget>,
    class: BodyClass,
    peer: Option<IpAddr>,
    held_units: AtomicUsize,
    /// Why this request's body was cut off, if it was. Lets the admission layer
    /// turn the generic `400` an extractor makes of a failed body into `408` or
    /// `503` when bearer-token access is off and no middleware read the body.
    stop: Mutex<Option<BodyStop>>,
}

impl Reservation {
    fn record_stop(&self, stop: BodyStop) -> BodyStop {
        *self.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop);
        stop
    }

    fn take_stop(&self) -> Option<BodyStop> {
        self.stop.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Make sure at least `bytes` are charged. `Err` when the pool or this
    /// peer's share cannot cover it; nothing more is charged then.
    fn ensure(&self, bytes: usize) -> Result<(), ()> {
        let want = units(bytes);
        let held = self.held_units.load(Ordering::Acquire);
        if want <= held {
            return Ok(());
        }
        let extra = want - held;
        if !self.charge_peer(extra) {
            return Err(());
        }
        match self.budget.pool(self.class).try_acquire_many(extra as u32) {
            Ok(permit) => {
                permit.forget();
                self.held_units.store(want, Ordering::Release);
                Ok(())
            }
            Err(_) => {
                self.uncharge_peer(extra);
                Err(())
            }
        }
    }

    /// The peer's counter for this reservation's class and the limit on it.
    fn peer_slot<'a>(&self, used: &'a mut PeerUse) -> (&'a mut usize, usize) {
        match self.class {
            BodyClass::Large => (&mut used.large_units, self.budget.per_peer_large_units),
            BodyClass::Anonymous => (&mut used.anonymous_units, units(ANONYMOUS_PEER_BYTES)),
            BodyClass::Small => (&mut used.small_units, units(SMALL_PEER_BYTES)),
        }
    }

    fn charge_peer(&self, extra: usize) -> bool {
        let Some(peer) = self.peer else { return true };
        let mut peers = self.budget.peers.lock().unwrap_or_else(|e| e.into_inner());
        let used = peers.entry(peer).or_default();
        let (slot, limit) = self.peer_slot(used);
        let fits = *slot + extra <= limit;
        if fits {
            *slot += extra;
        } else {
            BodyBudget::prune(&mut peers, peer);
        }
        fits
    }

    fn uncharge_peer(&self, units: usize) {
        let Some(peer) = self.peer else { return };
        let mut peers = self.budget.peers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(used) = peers.get_mut(&peer) {
            let (slot, _) = self.peer_slot(used);
            *slot = slot.saturating_sub(units);
        }
        BodyBudget::prune(&mut peers, peer);
    }

    /// Give back everything charged so far (idempotent). The concurrent-body
    /// count of a large body is returned when the reservation is dropped.
    fn release(&self) {
        let held = self.held_units.swap(0, Ordering::AcqRel);
        if held == 0 {
            return;
        }
        self.budget.pool(self.class).add_permits(held);
        self.uncharge_peer(held);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.release();
        if let (BodyClass::Large, Some(peer)) = (self.class, self.peer) {
            let mut peers = self.budget.peers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(used) = peers.get_mut(&peer) {
                used.large_bodies = used.large_bodies.saturating_sub(1);
            }
            BodyBudget::prune(&mut peers, peer);
        }
    }
}

/// Why a body stopped.
#[derive(Debug, Clone, Copy)]
pub(crate) enum BodyStop {
    /// No data for the idle timeout, or the total deadline passed.
    Idle,
    /// The in-flight budget cannot cover the bytes received so far.
    Budget,
}

impl std::fmt::Display for BodyStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => f.write_str("request body stalled (MENTISDB_BODY_IDLE_TIMEOUT_SECS)"),
            Self::Budget => {
                f.write_str("too many request bodies in flight (MENTISDB_MAX_INFLIGHT_BODY_BYTES)")
            }
        }
    }
}

impl std::error::Error for BodyStop {}

/// A body that times out when it goes quiet and charges what it receives to a
/// [`Reservation`].
struct MeteredBody {
    inner: Body,
    reservation: Arc<Reservation>,
    received: usize,
    idle: Duration,
    deadline: Pin<Box<tokio::time::Sleep>>,
    /// Absolute deadline for the whole body (small and anonymous requests).
    total: Option<Pin<Box<tokio::time::Sleep>>>,
    started: tokio::time::Instant,
    /// Start of the rate window in progress and the bytes that arrived in it.
    window_start: Option<tokio::time::Instant>,
    window_bytes: usize,
}

impl MeteredBody {
    /// Count `added` bytes that just arrived and report whether the body has
    /// fallen below [`MIN_BODY_BYTES_PER_SEC`] over the window they belong to.
    ///
    /// The rate is measured over consecutive windows, each as long as the idle
    /// timeout, starting when the grace period (twice the idle timeout) ends, and
    /// a window is judged on its own bytes. Credit cannot be banked: a body that
    /// sent most of its data at once and then trickles fails at the end of the
    /// next window, not after the whole transfer has averaged out.
    fn too_slow(&mut self, added: usize) -> bool {
        let now = tokio::time::Instant::now();
        let grace_end = self.started + self.idle.saturating_mul(2);
        if now <= grace_end {
            return false;
        }
        let start = *self.window_start.get_or_insert(grace_end);
        self.window_bytes = self.window_bytes.saturating_add(added);
        let elapsed = now.saturating_duration_since(start);
        if elapsed < self.idle {
            return false;
        }
        let required = elapsed.as_secs_f64() * MIN_BODY_BYTES_PER_SEC as f64;
        if (self.window_bytes as f64) < required {
            return true;
        }
        self.window_start = Some(now);
        self.window_bytes = 0;
        false
    }
}

impl http_body::Body for MeteredBody {
    type Data = axum::body::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.received = this.received.saturating_add(data.len());
                    if this.reservation.ensure(this.received).is_err() {
                        return Poll::Ready(Some(Err(Box::new(
                            this.reservation.record_stop(BodyStop::Budget),
                        ))));
                    }
                }
                let added = frame.data_ref().map_or(0, |data| data.len());
                if this.too_slow(added) {
                    return Poll::Ready(Some(Err(Box::new(
                        this.reservation.record_stop(BodyStop::Idle),
                    ))));
                }
                let next = tokio::time::Instant::now() + this.idle;
                this.deadline.as_mut().reset(next);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => {
                // Read to its end: the charge is no longer needed.
                this.reservation.release();
                Poll::Ready(None)
            }
            Poll::Pending => {
                let total_over = this
                    .total
                    .as_mut()
                    .is_some_and(|total| total.as_mut().poll(cx).is_ready());
                if total_over || this.deadline.as_mut().poll(cx).is_ready() {
                    Poll::Ready(Some(Err(Box::new(
                        this.reservation.record_stop(BodyStop::Idle),
                    ))))
                } else {
                    Poll::Pending
                }
            }
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        http_body::Body::size_hint(&self.inner)
    }
}

/// The body cap for `path`: the large cap for a large route when the caller is
/// allowed it, the small cap otherwise.
pub(crate) fn body_cap(service: &MentisDbService, path: &str, may_use_large: bool) -> usize {
    let max = service.max_body_bytes();
    if may_use_large && is_large_body_route(path) {
        max
    } else {
        max.min(SMALL_BODY_BYTES)
    }
}

/// Whether the caller named by the request headers may use the large cap: any
/// caller when bearer-token access is off, otherwise one with a valid bearer
/// token. Decided from headers only, before any body is read.
pub(crate) fn may_use_large_cap(service: &MentisDbService, request: &Request) -> bool {
    if !service.bearer_access_enabled() {
        return true;
    }
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| service.bearer_token_valid(token))
}

/// The status a failure while reading a body maps to, when it was one of this
/// module's: `408` for a body that stalled or dragged on, `503` with
/// `Retry-After` for an exhausted budget. `None` for any other error.
pub(crate) fn body_stop_response(error: &(dyn std::error::Error + 'static)) -> Option<Response> {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = current {
        if let Some(stop) = error.downcast_ref::<BodyStop>() {
            return Some(stop_response(*stop));
        }
        current = error.source();
    }
    None
}

/// The response for a body that was cut off: `408` for a stall or a transfer
/// that was too slow, `503` with `Retry-After` for an exhausted budget.
fn stop_response(stop: BodyStop) -> Response {
    match stop {
        BodyStop::Idle => refuse(StatusCode::REQUEST_TIMEOUT, &stop.to_string(), false),
        BodyStop::Budget => refuse(StatusCode::SERVICE_UNAVAILABLE, &stop.to_string(), true),
    }
}

fn refuse(status: StatusCode, message: &str, retry_after: bool) -> Response {
    let mut response = (status, Json(json!({ "error": message }))).into_response();
    if retry_after {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    }
    response
}

/// Outermost layer of the REST and MCP routers: refuse, cap, meter and time out
/// the request body before anything reads it. See the [module
/// documentation](crate::server_body).
pub(crate) async fn body_admission_middleware(
    axum::extract::State(service): axum::extract::State<Arc<MentisDbService>>,
    request: Request,
    next: Next,
) -> Response {
    let may_large = may_use_large_cap(&service, &request);
    let path = request.uri().path().to_owned();
    let cap = body_cap(&service, &path, may_large);
    let class = if may_large && is_large_body_route(&path) {
        BodyClass::Large
    } else if service.bearer_access_enabled() && !may_large {
        BodyClass::Anonymous
    } else {
        BodyClass::Small
    };
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > cap) {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds the maximum accepted size for this route \
             (MENTISDB_MAX_BODY_BYTES applies to import routes with a valid token)",
            false,
        );
    }
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip());
    let Some(reservation) = service.body_budget().reservation(class, peer) else {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many large request bodies in flight from this address; retry shortly",
            true,
        );
    };
    // Only a large body is charged its declared length up front (so an import
    // that cannot fit is refused at once); everything else is charged as bytes
    // arrive, so a declared length costs nothing by itself.
    if class == BodyClass::Large && reservation.ensure(declared.unwrap_or(0)).is_err() {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many request bodies in flight; retry shortly",
            true,
        );
    }
    let idle = service.body_idle_timeout();
    let total =
        (class != BodyClass::Large).then(|| Box::pin(tokio::time::sleep(idle.saturating_mul(2))));
    let (parts, body) = request.into_parts();
    let metered = MeteredBody {
        inner: body,
        reservation: Arc::clone(&reservation),
        received: 0,
        idle,
        deadline: Box::pin(tokio::time::sleep(idle)),
        total,
        started: tokio::time::Instant::now(),
        window_start: None,
        window_bytes: 0,
    };
    let body = Body::new(http_body_util::Limited::new(metered, cap));
    let response = next.run(Request::from_parts(parts, body)).await;
    // A handler's extractor reports a failed body as a generic client error
    // (`400`); when it was this layer that cut the body off, say so.
    let stopped = reservation.take_stop();
    drop(reservation);
    match stopped {
        Some(stop) if response.status().is_client_error() => stop_response(stop),
        _ => response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    const MIB: usize = 1024 * 1024;

    fn peer(n: u32) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(
            0x2001,
            0xdb8,
            0,
            0,
            0,
            0,
            (n >> 16) as u16,
            n as u16,
        ))
    }

    fn budget() -> Arc<BodyBudget> {
        BodyBudget::new(256 * MIB, 128 * MIB)
    }

    /// Anonymous bodies draw on a pool of their own: however many anonymous
    /// peers fill it, the small pool authenticated traffic uses is untouched.
    #[test]
    fn anonymous_bodies_cannot_drain_the_small_pool() {
        let budget = budget();
        // Four anonymous peers want 8 MiB each: only the 16 MiB pool's worth fits.
        let anonymous: Vec<_> = (0..4)
            .map(|n| {
                budget
                    .reservation(BodyClass::Anonymous, Some(peer(n)))
                    .unwrap()
            })
            .collect();
        let admitted = anonymous
            .iter()
            .filter(|r| r.ensure(8 * MIB).is_ok())
            .count();
        assert_eq!(admitted, 2, "the anonymous pool is 16 MiB");
        // The whole small pool is still there for a legitimate peer (up to its share).
        let small = budget
            .reservation(BodyClass::Small, Some(peer(100)))
            .unwrap();
        assert!(small.ensure(SMALL_PEER_BYTES).is_ok());
        let other = budget
            .reservation(BodyClass::Small, Some(peer(101)))
            .unwrap();
        assert!(other.ensure(SMALL_PEER_BYTES).is_ok());
    }

    /// One small-body peer cannot take the whole small pool (bearer access off:
    /// every caller is alike), while other peers are unaffected.
    #[test]
    fn one_peer_cannot_take_the_whole_small_pool() {
        let budget = budget();
        let first = budget.reservation(BodyClass::Small, Some(peer(1))).unwrap();
        assert!(first.ensure(SMALL_PEER_BYTES).is_ok());
        assert!(first.ensure(SMALL_PEER_BYTES + MIB).is_err());
        let second = budget.reservation(BodyClass::Small, Some(peer(1))).unwrap();
        assert!(
            second.ensure(MIB).is_err(),
            "the share is per peer, not per request"
        );
        let elsewhere = budget.reservation(BodyClass::Small, Some(peer(2))).unwrap();
        assert!(elsewhere.ensure(MIB).is_ok());
    }

    /// The per-peer map holds only peers with something charged: rotating through
    /// many addresses, charging and releasing or being refused, leaves it empty.
    #[test]
    fn the_peer_map_does_not_grow_with_address_rotation() {
        let budget = budget();
        for n in 0..5_000u32 {
            for class in [BodyClass::Small, BodyClass::Anonymous, BodyClass::Large] {
                let reservation = budget.reservation(class, Some(peer(n))).unwrap();
                let _ = reservation.ensure(MIB);
                // A charge the peer's share refuses must not leave an entry either.
                let _ = reservation.ensure(1024 * MIB);
                if n % 2 == 0 {
                    reservation.release();
                }
            }
        }
        assert!(
            budget.peers.lock().unwrap().is_empty(),
            "{} stale peer entries",
            budget.peers.lock().unwrap().len()
        );
    }

    /// A body that keeps arriving just often enough to beat the idle timer but
    /// slower than the minimum rate is cut off once its grace period is over; a
    /// body at a healthy rate is not.
    #[tokio::test]
    async fn a_slow_drip_that_beats_the_idle_timer_still_fails() {
        use http_body_util::BodyExt;

        /// `chunks` chunks of `size` bytes, one every `gap`.
        struct Drip {
            chunks: usize,
            size: usize,
            gap: Duration,
            sleep: Pin<Box<tokio::time::Sleep>>,
        }
        impl http_body::Body for Drip {
            type Data = axum::body::Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                if self.chunks == 0 {
                    return Poll::Ready(None);
                }
                if self.sleep.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                let next = tokio::time::Instant::now() + self.gap;
                self.sleep.as_mut().reset(next);
                self.chunks -= 1;
                let data = axum::body::Bytes::from(vec![b'x'; self.size]);
                Poll::Ready(Some(Ok(http_body::Frame::data(data))))
            }
        }
        let run = |size: usize, chunks: usize| async move {
            let budget = budget();
            let reservation = budget.reservation(BodyClass::Large, None).unwrap();
            let idle = Duration::from_millis(100);
            let drip = Drip {
                chunks,
                size,
                gap: Duration::from_millis(40),
                sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            };
            let body = Body::new(drip);
            let metered = MeteredBody {
                inner: body,
                reservation,
                received: 0,
                idle,
                deadline: Box::pin(tokio::time::sleep(idle)),
                total: None,
                started: tokio::time::Instant::now(),
                window_start: None,
                window_bytes: 0,
            };
            metered
                .collect()
                .await
                .map(|collected| collected.to_bytes().len())
        };
        // 1 byte every 40 ms is well inside the 100 ms idle timer, but far below
        // 64 KiB/s: it fails once the 200 ms grace period is over.
        let error = run(1, 50).await.expect_err("a 1 byte/40 ms drip must fail");
        assert!(
            matches!(error.downcast_ref::<BodyStop>(), Some(BodyStop::Idle)),
            "{error}"
        );
        // 8 KiB every 40 ms is 200 KiB/s: fine.
        assert_eq!(run(8 * 1024, 10).await.unwrap(), 80 * 1024);
    }

    /// Credit cannot be banked: a body that sends a large burst right away and
    /// then trickles still fails in the first window after the grace period.
    #[tokio::test]
    async fn a_burst_followed_by_a_trickle_does_not_bank_credit() {
        use http_body_util::BodyExt;

        struct Burst {
            sent: usize,
            sleep: Pin<Box<tokio::time::Sleep>>,
        }
        impl http_body::Body for Burst {
            type Data = axum::body::Bytes;
            type Error = std::convert::Infallible;
            fn poll_frame(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
                if self.sent >= 400 {
                    return Poll::Ready(None);
                }
                let first = self.sent == 0;
                if !first && self.sleep.as_mut().poll(cx).is_pending() {
                    return Poll::Pending;
                }
                let next = tokio::time::Instant::now() + Duration::from_millis(40);
                self.sleep.as_mut().reset(next);
                self.sent += 1;
                // 8 MiB in the first frame, then one byte every 40 ms.
                let size = if first { 8 * MIB } else { 1 };
                let data = axum::body::Bytes::from(vec![b'x'; size]);
                Poll::Ready(Some(Ok(http_body::Frame::data(data))))
            }
        }
        let budget = budget();
        let reservation = budget.reservation(BodyClass::Large, None).unwrap();
        let idle = Duration::from_millis(100);
        let metered = MeteredBody {
            inner: Body::new(Burst {
                sent: 0,
                sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            }),
            reservation,
            received: 0,
            idle,
            deadline: Box::pin(tokio::time::sleep(idle)),
            total: None,
            started: tokio::time::Instant::now(),
            window_start: None,
            window_bytes: 0,
        };
        // 8 MiB already averages far above 64 KiB/s over the 16 s a whole-transfer
        // average would allow; the window rule fails the trickle after ~300 ms.
        let began = std::time::Instant::now();
        let error = metered.collect().await.expect_err("the trickle must fail");
        assert!(matches!(
            error.downcast_ref::<BodyStop>(),
            Some(BodyStop::Idle)
        ));
        assert!(
            began.elapsed() < Duration::from_secs(2),
            "{:?}",
            began.elapsed()
        );
    }

    /// While a body is in flight its peer has an entry; releasing it removes it.
    #[test]
    fn an_entry_exists_only_while_something_is_charged() {
        let budget = budget();
        let reservation = budget.reservation(BodyClass::Large, Some(peer(7))).unwrap();
        assert_eq!(
            budget.peers.lock().unwrap().len(),
            1,
            "concurrent-body count"
        );
        reservation.ensure(MIB).unwrap();
        reservation.release();
        assert_eq!(
            budget.peers.lock().unwrap().len(),
            1,
            "the body is still open"
        );
        drop(reservation);
        assert!(budget.peers.lock().unwrap().is_empty());
    }
}
