//! Admission control for request bodies: how many bytes of body the daemon is
//! willing to hold, for whom, and for how long.
//!
//! Reading a request body costs memory and a connection slot. Without limits
//! one unauthenticated peer can pin the largest body the daemon accepts, and
//! many peers (or one peer with many connections) can multiply that. Every REST
//! and MCP request therefore passes through `body_admission_middleware`, the
//! outermost layer, before any authentication step reads its body:
//!
//! * **A per-route cap.** Ordinary routes accept at most
//!   [`SMALL_BODY_BYTES`] (2 MiB). The routes in [`LARGE_BODY_ROUTES`] (the
//!   `/v1/import-*` family, skill upload and memory extraction) accept up to
//!   `MENTISDB_MAX_BODY_BYTES` (default 64 MiB), but only from a caller that is
//!   authenticated when bearer-token access is on: a request with no valid
//!   token keeps the small cap. A declared `Content-Length` over the cap is
//!   refused with `413` at once; a body without one (chunked, HTTP/2) is cut off
//!   with `413` when it crosses the cap.
//! * **A global budget** (`MENTISDB_MAX_INFLIGHT_BODY_BYTES`, default four
//!   times the body cap) of body bytes held by requests in flight at once, and
//!   a **per-peer share** of it (twice the body cap, never more than the
//!   global). A request is charged for its declared length when it arrives and
//!   for each chunk as it streams; when the budget is gone it gets `503` with
//!   `Retry-After`. The charge is returned when the request finishes, not when
//!   its body is read: a handler that buffered the body still holds the memory.
//! * **An idle timeout** (`MENTISDB_BODY_IDLE_TIMEOUT_SECS`, default 30): a body
//!   that stops arriving for that long fails, so a slow-drip client cannot hold a
//!   connection and its reservation indefinitely.
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
/// Every other route is held to [`SMALL_BODY_BYTES`].
pub const LARGE_BODY_ROUTES: [&str; 3] = [
    "/v1/import-markdown",
    "/v1/skills/upload",
    "/v1/extract-memories",
];

/// Granularity of the budget: permits are KiB so a byte count always fits.
const UNIT: usize = 1024;

/// Whether `path` may carry a large body, see [`LARGE_BODY_ROUTES`].
pub fn is_large_body_route(path: &str) -> bool {
    LARGE_BODY_ROUTES.contains(&path)
}

/// The in-flight body budget shared by every request of one service.
#[derive(Debug)]
pub(crate) struct BodyBudget {
    global: Arc<Semaphore>,
    per_peer_units: usize,
    peers: Mutex<HashMap<IpAddr, usize>>,
}

impl BodyBudget {
    /// A budget of `global_bytes`, of which one peer may hold `per_peer_bytes`.
    pub(crate) fn new(global_bytes: usize, per_peer_bytes: usize) -> Arc<Self> {
        let units = |bytes: usize| bytes.div_ceil(UNIT).min(u32::MAX as usize);
        Arc::new(Self {
            global: Arc::new(Semaphore::new(units(global_bytes))),
            per_peer_units: units(per_peer_bytes).min(units(global_bytes)),
            peers: Mutex::new(HashMap::new()),
        })
    }

    /// Start an (empty) reservation for a request from `peer`.
    fn reservation(self: &Arc<Self>, peer: Option<IpAddr>) -> Arc<Reservation> {
        Arc::new(Reservation {
            budget: Arc::clone(self),
            peer,
            held_units: AtomicUsize::new(0),
        })
    }
}

/// The bytes one request has charged to the budget, returned when it is dropped.
#[derive(Debug)]
pub(crate) struct Reservation {
    budget: Arc<BodyBudget>,
    peer: Option<IpAddr>,
    held_units: AtomicUsize,
}

impl Reservation {
    /// Make sure at least `bytes` are charged. `Err` when the global budget or
    /// this peer's share cannot cover it; nothing is charged then.
    fn ensure(&self, bytes: usize) -> Result<(), ()> {
        let want = bytes.div_ceil(UNIT);
        let held = self.held_units.load(Ordering::Acquire);
        if want <= held {
            return Ok(());
        }
        let extra = want - held;
        if let Some(peer) = self.peer {
            let mut peers = self.budget.peers.lock().unwrap_or_else(|e| e.into_inner());
            let used = peers.entry(peer).or_insert(0);
            if *used + extra > self.budget.per_peer_units {
                return Err(());
            }
            *used += extra;
        }
        match self.budget.global.try_acquire_many(extra as u32) {
            Ok(permit) => {
                permit.forget();
                self.held_units.store(want, Ordering::Release);
                Ok(())
            }
            Err(_) => {
                self.release_peer(extra);
                Err(())
            }
        }
    }

    fn release_peer(&self, units: usize) {
        let Some(peer) = self.peer else { return };
        let mut peers = self.budget.peers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(used) = peers.get_mut(&peer) {
            *used = used.saturating_sub(units);
            if *used == 0 {
                peers.remove(&peer);
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let held = *self.held_units.get_mut();
        if held > 0 {
            self.budget.global.add_permits(held);
            self.release_peer(held);
        }
    }
}

/// Why a body stopped.
#[derive(Debug)]
enum BodyStop {
    /// No data for the idle timeout.
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
                        return Poll::Ready(Some(Err(Box::new(BodyStop::Budget))));
                    }
                }
                let next = tokio::time::Instant::now() + this.idle;
                this.deadline.as_mut().reset(next);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => match this.deadline.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(BodyStop::Idle)))),
                Poll::Pending => Poll::Pending,
            },
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
    let cap = body_cap(
        &service,
        request.uri().path(),
        may_use_large_cap(&service, &request),
    );
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
    let reservation = service.body_budget().reservation(peer);
    if reservation.ensure(declared.unwrap_or(0)).is_err() {
        return refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "too many request bodies in flight; retry shortly",
            true,
        );
    }
    let idle = service.body_idle_timeout();
    let (parts, body) = request.into_parts();
    let metered = MeteredBody {
        inner: body,
        reservation: Arc::clone(&reservation),
        received: 0,
        idle,
        deadline: Box::pin(tokio::time::sleep(idle)),
    };
    let body = Body::new(http_body_util::Limited::new(metered, cap));
    let response = next.run(Request::from_parts(parts, body)).await;
    // The reservation is held until the handler has finished with the body.
    drop(reservation);
    response
}
