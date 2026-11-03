#![cfg(feature = "server")]
//! One `Host` policy for every MentisDB listener, now that `cloudllm_mcp` 0.4
//! checks `Host` itself.
//!
//! On a loopback bind the library turns away every `Host` but `localhost`,
//! `127.0.0.1` and `[::1]` unless it is told otherwise. MentisDB tells it
//! the dashboard's list: `MENTISDB_BIND_HOST`, `MENTISDB_DASHBOARD_ALLOWED_HOSTS`
//! and, on TLS listeners only, `my.mentisdb.com`. These tests pin that the MCP routers
//! (streamable HTTP, legacy `/tools/*` and `/resources/*`, the `GET /` event
//! stream) and the REST router agree with it, that a foreign `Host` is still
//! refused, and that the library's 401 text carries MentisDB's guidance.
//!
//! `MENTISDB_BIND_HOST` and `MENTISDB_DASHBOARD_ALLOWED_HOSTS` are process
//! global, so every test takes [`EnvGuard`], which serializes them and puts
//! the variables back.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mentisdb::auth::BearerTokenScope;
use mentisdb::server::{
    combined_mcp_router, rest_router, standard_mcp_router, MentisDbServiceConfig,
};
use mentisdb::StorageAdapterKind;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tower::util::ServiceExt;

const ALLOWED_HOSTS_ENV: &str = "MENTISDB_DASHBOARD_ALLOWED_HOSTS";
const BIND_HOST_ENV: &str = "MENTISDB_BIND_HOST";

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Holds the environment lock and clears both host variables; restores them
/// (to unset) when dropped, even when the test panics.
struct EnvGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    async fn new() -> Self {
        let lock = ENV_LOCK.lock().await;
        std::env::remove_var(ALLOWED_HOSTS_ENV);
        std::env::remove_var(BIND_HOST_ENV);
        Self { _lock: lock }
    }

    fn set(&self, name: &str, value: &str) {
        std::env::set_var(name, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        std::env::remove_var(ALLOWED_HOSTS_ENV);
        std::env::remove_var(BIND_HOST_ENV);
    }
}

fn config(name: &str) -> MentisDbServiceConfig {
    let dir = std::env::temp_dir().join(format!("mentisdb_hostpol_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary)
}

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const WILDCARD: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

async fn send(router: &Router, mut request: Request<Body>) -> (StatusCode, String) {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            44000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// A JSON-RPC `method` call to `POST /` with the given extra headers.
fn rpc(method: &str, params: Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::post("/")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    builder.body(Body::from(body.to_string())).unwrap()
}

/// A legacy `POST <path>` with an empty JSON body and the given headers.
fn legacy(path: &str, body: Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::post(path).header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

fn event_stream(headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::get("/").header("accept", "text/event-stream");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

fn rest_get(headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::get("/v1/chains");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

/// Status of every transport for one `Host` value: streamable POST, the
/// legacy tool list and resource list, and the event stream.
async fn mcp_statuses(host: &str, bind: IpAddr, name: &str) -> [StatusCode; 4] {
    let (router, _broadcaster) = standard_mcp_router(config(name), bind);
    let combined = combined_mcp_router(config(&format!("{name}_c")), bind);
    let host = [("host", host)];
    [
        send(&router, rpc("tools/list", json!({}), &host)).await.0,
        send(&combined, legacy("/tools/list", json!({}), &host))
            .await
            .0,
        send(&combined, legacy("/resources/list", json!({}), &host))
            .await
            .0,
        // Only the status line of the endless stream is read.
        {
            let response =
                router
                    .clone()
                    .oneshot({
                        let mut request = event_stream(&host);
                        request.extensions_mut().insert(axum::extract::ConnectInfo(
                            SocketAddr::from(([127, 0, 0, 1], 44001)),
                        ));
                        request
                    })
                    .await
                    .unwrap();
            response.status()
        },
    ]
}

fn assert_all(statuses: [StatusCode; 4], expected: StatusCode, what: &str) {
    assert_eq!(
        statuses, [expected; 4],
        "{what}: [POST /, /tools/list, /resources/list, GET /]"
    );
}

/// The `my.mentisdb.com` alias is a public DNS name: whoever controls it can
/// rebind it to `127.0.0.1`. A plain-HTTP listener therefore refuses it on
/// every MCP transport, and accepts it only when the operator lists it in
/// `MENTISDB_DASHBOARD_ALLOWED_HOSTS` (TLS listeners accept it by default; see
/// `alias_tls_scope_tests`).
#[tokio::test]
async fn the_mentisdb_alias_is_refused_on_plain_mcp_unless_listed() {
    let env = EnvGuard::new().await;
    for host in [
        "my.mentisdb.com",
        "my.mentisdb.com:9473",
        "MY.MentisDB.com:9471",
    ] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "alias").await,
            StatusCode::FORBIDDEN,
            host,
        );
    }
    env.set(ALLOWED_HOSTS_ENV, "my.mentisdb.com");
    for host in [
        "my.mentisdb.com",
        "my.mentisdb.com:9473",
        "MY.MentisDB.com:9471",
    ] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "alias_listed").await,
            StatusCode::OK,
            host,
        );
    }
}

/// The names the library already allowed keep working.
#[tokio::test]
async fn loopback_names_are_accepted_on_every_mcp_transport() {
    let _env = EnvGuard::new().await;
    for host in [
        "localhost",
        "localhost:9471",
        "127.0.0.1:9471",
        "[::1]:9471",
    ] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "loop").await,
            StatusCode::OK,
            host,
        );
    }
}

/// A rebinding page keeps its own name in `Host`; every transport refuses it,
/// along with look-alikes of the allowed names.
#[tokio::test]
async fn a_foreign_host_is_rejected_on_every_mcp_transport_over_loopback() {
    let _env = EnvGuard::new().await;
    for host in [
        "evil.example.com",
        "evil.example.com:9471",
        "my.mentisdb.com.evil.com",
        "127.0.0.1.evil.com",
        "localhost.evil.com",
        "203.0.113.7",
    ] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "foreign").await,
            StatusCode::FORBIDDEN,
            host,
        );
    }
}

/// Regression: a VPS reached by its public IP lists that IP in
/// `MENTISDB_DASHBOARD_ALLOWED_HOSTS`; MCP must follow the dashboard, IPv6
/// entries (which the library only matches in brackets) included.
#[tokio::test]
async fn allowlisted_hosts_and_public_ips_are_accepted_on_mcp_over_loopback() {
    let env = EnvGuard::new().await;
    env.set(
        ALLOWED_HOSTS_ENV,
        "203.0.113.7, vps.example.org , 2001:db8::7,,",
    );
    for host in [
        "203.0.113.7",
        "203.0.113.7:9471",
        "VPS.example.org:9473",
        "[2001:db8::7]:9471",
    ] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "allowed").await,
            StatusCode::OK,
            host,
        );
    }
    for host in ["203.0.113.8", "other.example.org", "[2001:db8::8]:9471"] {
        assert_all(
            mcp_statuses(host, LOOPBACK, "allowed_no").await,
            StatusCode::FORBIDDEN,
            host,
        );
    }
}

/// `MENTISDB_BIND_HOST` may be a DNS name (the daemon then binds loopback);
/// it is the name clients use, so MCP must accept it.
#[tokio::test]
async fn a_bind_host_name_is_accepted_on_mcp_over_loopback() {
    let env = EnvGuard::new().await;
    env.set(BIND_HOST_ENV, "mem.example.net");
    assert_all(
        mcp_statuses("mem.example.net:9471", LOOPBACK, "bindname").await,
        StatusCode::OK,
        "bind host name",
    );
    assert_all(
        mcp_statuses("other.example.net", LOOPBACK, "bindname_no").await,
        StatusCode::FORBIDDEN,
        "other name",
    );
}

/// A wildcard `MENTISDB_BIND_HOST` admits the machine's own interface IPs, as
/// the dashboard does, even when the listener under test is a loopback one.
#[tokio::test]
async fn a_wildcard_bind_host_admits_interface_ips_but_not_foreign_ips() {
    let env = EnvGuard::new().await;
    env.set(BIND_HOST_ENV, "0.0.0.0");
    let own = if_addrs::get_if_addrs()
        .unwrap()
        .into_iter()
        .map(|iface| iface.addr.ip())
        .find(IpAddr::is_ipv4)
        .expect("this machine has an IPv4 interface");
    assert_all(
        mcp_statuses(&format!("{own}:9471"), LOOPBACK, "wild").await,
        StatusCode::OK,
        "own interface IP",
    );
    assert_all(
        mcp_statuses("203.0.113.77", LOOPBACK, "wild_no").await,
        StatusCode::FORBIDDEN,
        "foreign IP",
    );
}

/// On a wildcard bind the library does no `Host` check (Docker and LAN
/// clients reach the daemon by service name or DHCP address), so a foreign
/// `Host` from a non-browser client is served, as in 0.3.
#[tokio::test]
async fn a_wildcard_bind_does_not_check_host_for_programmatic_clients() {
    let _env = EnvGuard::new().await;
    for host in ["mentisdb:9471", "192.168.1.50:9471", "my.mentisdb.com"] {
        assert_all(
            mcp_statuses(host, WILDCARD, "wildbind").await,
            StatusCode::OK,
            host,
        );
    }
}

/// On a wildcard bind a browser (`Sec-Fetch-Site` or `Origin` present) must
/// still name an allowed host while bearer enforcement is off: MentisDB's
/// browser guard, not the library, closes DNS rebinding there. The alias is
/// not one of them on a plain listener unless it is listed.
#[tokio::test]
async fn browser_requests_to_a_wildcard_bind_need_an_allowed_host() {
    let env = EnvGuard::new().await;
    let (router, _b) = standard_mcp_router(config("browser_wild"), WILDCARD);
    let browser = |host: &'static str| {
        rpc(
            "tools/list",
            json!({}),
            &[("host", host), ("sec-fetch-site", "same-origin")],
        )
    };
    assert_eq!(
        send(&router, browser("mem.example.org:9473")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&router, browser("my.mentisdb.com:9473")).await.0,
        StatusCode::FORBIDDEN,
        "the alias is not an own host on a plain listener"
    );
    assert_eq!(
        send(&router, browser("evil.example.com")).await.0,
        StatusCode::FORBIDDEN
    );
    env.set(ALLOWED_HOSTS_ENV, "mem.example.org, my.mentisdb.com");
    let (router, _b) = standard_mcp_router(config("browser_wild_listed"), WILDCARD);
    assert_eq!(
        send(&router, browser("mem.example.org:9473")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        send(&router, browser("my.mentisdb.com:9473")).await.0,
        StatusCode::OK,
        "listed in the allowlist"
    );
}

/// Regression: the library's own `Origin` check admits loopback origins only,
/// which refused a page served from an allowlisted name although its `Host` is
/// allowed. MentisDB's guard decides `Origin` against the same policy, so a
/// listed name (here the alias too, which a plain listener accepts only when
/// listed) passes, and look-alikes and foreign pages fail (bearer enforcement
/// off).
#[tokio::test]
async fn origin_follows_the_host_policy_on_every_mcp_transport() {
    let env = EnvGuard::new().await;
    env.set(ALLOWED_HOSTS_ENV, "vps.example.org, my.mentisdb.com");
    let (router, _b) = standard_mcp_router(config("origin"), LOOPBACK);
    let combined = combined_mcp_router(config("origin_c"), LOOPBACK);
    for (origin, allowed) in [
        ("https://my.mentisdb.com:9475", true),
        ("https://vps.example.org", true),
        ("http://localhost:3000", true),
        ("http://127.0.0.1", true),
        ("https://evil.example.com", false),
        ("http://my.mentisdb.com.evil.com", false),
        ("http://localhost.evil.com", false),
        ("null", false),
    ] {
        let headers = [("host", "127.0.0.1:9471"), ("origin", origin)];
        let expected = if allowed {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(
            send(&router, rpc("tools/list", json!({}), &headers))
                .await
                .0,
            expected,
            "POST / from {origin}"
        );
        assert_eq!(
            send(&combined, legacy("/tools/list", json!({}), &headers))
                .await
                .0,
            expected,
            "/tools/list from {origin}"
        );
        assert_eq!(
            send(&combined, legacy("/resources/list", json!({}), &headers))
                .await
                .0,
            expected,
            "/resources/list from {origin}"
        );
    }
}

/// REST has no library check; it uses the same policy through the browser
/// guard: a browser must name an allowed `Host` and `Origin`, a programmatic
/// client sends neither header and is unaffected.
#[tokio::test]
async fn rest_follows_the_same_host_policy_for_browsers() {
    let env = EnvGuard::new().await;
    env.set(ALLOWED_HOSTS_ENV, "203.0.113.7");
    let router = rest_router(config("rest"));
    for (host, allowed) in [
        ("my.mentisdb.com:9474", false),
        ("203.0.113.7:9472", true),
        ("localhost:9472", true),
        ("evil.example.com", false),
        ("203.0.113.8", false),
    ] {
        let (status, body) = send(
            &router,
            rest_get(&[("host", host), ("sec-fetch-site", "same-origin")]),
        )
        .await;
        let expected = if allowed {
            StatusCode::OK
        } else {
            StatusCode::FORBIDDEN
        };
        assert_eq!(status, expected, "{host}: {body}");
    }
    let (status, _) = send(
        &router,
        rest_get(&[
            ("host", "203.0.113.7"),
            ("origin", "http://203.0.113.7:9472"),
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "listed origin");
    let (status, _) = send(
        &router,
        rest_get(&[("host", "203.0.113.7"), ("origin", "https://evil.test")]),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign origin");
    let (status, _) = send(&router, rest_get(&[("host", "evil.example.com")])).await;
    assert_eq!(status, StatusCode::OK, "no browser signal, no check");
}

/// With bearer enforcement on, the event stream needs a token: none, a wrong
/// one and an empty one get 401, any active token (global or chain-scoped)
/// subscribes, and the allowed `Host` rules still apply first.
#[tokio::test]
async fn the_event_stream_needs_a_token_and_an_allowed_host() {
    let env = EnvGuard::new().await;
    env.set(ALLOWED_HOSTS_ENV, "vps.example.org");
    let config = config("sse_auth").with_bearer_token_access(true);
    let global = config
        .bearer_token_store
        .create("global", BearerTokenScope::Global)
        .unwrap()
        .token;
    let scoped = config
        .bearer_token_store
        .create("scoped", BearerTokenScope::chain("only-this").unwrap())
        .unwrap()
        .token;
    let (router, _broadcaster) = standard_mcp_router(config, LOOPBACK);

    let status = |headers: Vec<(&'static str, String)>| subscribe(router.clone(), headers);

    for bad in ["", "Bearer ", "Bearer not-a-token", "Bearer"] {
        let response = status(vec![("authorization", bad.to_string())]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{bad:?}");
    }
    assert_eq!(status(vec![]).await.status(), StatusCode::UNAUTHORIZED);
    for token in [&global, &scoped] {
        let response = status(vec![("authorization", format!("Bearer {token}"))]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/event-stream"
        );
    }
    let response = status(vec![
        ("authorization", format!("Bearer {global}")),
        ("host", "vps.example.org:9473".to_string()),
    ])
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "allowlisted host with token"
    );
    let response = status(vec![
        ("authorization", format!("Bearer {global}")),
        ("host", "evil.example.com".to_string()),
    ])
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "a valid token does not excuse a foreign host"
    );
}

/// Open `GET /` and return the response head; the stream itself never ends.
async fn subscribe(
    router: Router,
    headers: Vec<(&'static str, String)>,
) -> axum::response::Response {
    let mut builder = Request::get("/").header("accept", "text/event-stream");
    for (name, value) in &headers {
        builder = builder.header(*name, value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(SocketAddr::from((
            [127, 0, 0, 1],
            44002,
        ))));
    router.oneshot(request).await.unwrap()
}

/// The listed MCP resource is a spec `Resource`: `uri`, `name`, `mimeType`
/// (camel case, no `mime_type`), and it reads back as the skill text, through
/// the streamable-HTTP methods and the legacy routes alike.
#[tokio::test]
async fn the_skill_resource_lists_and_reads_with_the_spec_shape() {
    let _env = EnvGuard::new().await;
    let (router, _b) = standard_mcp_router(config("resources"), LOOPBACK);
    let (status, body) = send(&router, rpc("resources/list", json!({}), &[])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    let resources = listed["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1, "{body}");
    assert_resource_shape(&resources[0]);

    let (status, body) = send(
        &router,
        rpc(
            "resources/read",
            json!({"uri": "mentisdb://skill/core"}),
            &[],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let read: Value = serde_json::from_str(&body).unwrap();
    let text = read["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("MentisDB"), "{body}");

    let combined = combined_mcp_router(config("resources_c"), LOOPBACK);
    let (status, body) = send(&combined, legacy("/resources/list", json!({}), &[])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Value = serde_json::from_str(&body).unwrap();
    assert_resource_shape(&listed["resources"][0]);

    let (status, body) = send(
        &combined,
        legacy(
            "/resources/read",
            json!({"uri": "mentisdb://skill/core"}),
            &[],
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("MentisDB"), "{body}");
}

fn assert_resource_shape(resource: &Value) {
    assert_eq!(resource["uri"], "mentisdb://skill/core", "{resource}");
    assert!(
        resource["name"].as_str().is_some_and(|n| !n.is_empty()),
        "a spec Resource needs a name: {resource}"
    );
    assert_eq!(resource["mimeType"], "text/markdown", "{resource}");
    assert!(resource.get("mime_type").is_none(), "{resource}");
}

/// `cloudllm_mcp` 0.4's 401 body is product-neutral; MentisDB puts its own
/// remediation (the env switch and the CLI command) back, on the JSON-RPC
/// error, the legacy body and the event stream.
#[tokio::test]
async fn mcp_401s_carry_mentisdb_guidance() {
    let _env = EnvGuard::new().await;
    let config = config("guidance").with_bearer_token_access(true);
    let (router, _b) = standard_mcp_router(config.clone(), LOOPBACK);
    let combined = combined_mcp_router(config, LOOPBACK);

    let (status, body) = send(&router, rpc("tools/list", json!({}), &[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_guidance(&body);
    let (status, body) = send(&combined, legacy("/tools/list", json!({}), &[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_guidance(&body);
    let (status, body) = send(&router, event_stream(&[])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_guidance(&body);
}

fn assert_guidance(body: &str) {
    assert!(
        body.contains("MENTISDB_BEARER_TOKEN_ACCESS=false"),
        "{body}"
    );
    assert!(body.contains("mentisdb bearertoken create"), "{body}");
    assert!(
        !body.contains("ask the server operator"),
        "library text left in: {body}"
    );
}
