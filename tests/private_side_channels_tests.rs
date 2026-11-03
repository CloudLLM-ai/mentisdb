#![cfg(feature = "server")]
//! Push channels must not carry private text: webhooks and the SSE stream.

use axum::body::Body;
use axum::http::Request;
use ed25519_dalek::{Signer, SigningKey};
use mentisdb::access::AccessMode;
use mentisdb::operator::{apply, OperatorIdentity, OperatorOp};
use mentisdb::server::{standard_mcp_router, MentisDbServiceConfig};
use mentisdb::signing::{thought_payload_v2, to_hex, StoredSignature};
use mentisdb::webhooks::{WebhookManager, WebhookUrlPolicy};
use mentisdb::{MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tower::util::ServiceExt;

fn dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mentisdb_side_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Everything `receiver` yields until `marker` shows up in it, rendered with
/// `render`. Events are broadcast in the order the calls ran, so a call made
/// after the one under test and recognisable by `marker` closes the window:
/// by the time its event arrives, every event of the earlier call has been
/// read. This replaces "wait until it has been quiet for 300 ms", which could
/// not tell "nothing more will come" from "not yet".
///
/// The only timeout is a generous failure detector for a marker that never
/// arrives.
async fn events_until<T: Clone + std::fmt::Debug>(
    receiver: &mut tokio::sync::broadcast::Receiver<T>,
    marker: &str,
    render: impl Fn(&T) -> String,
) -> String {
    let mut events = String::new();
    while !events.contains(marker) {
        match tokio::time::timeout(Duration::from_secs(30), receiver.recv()).await {
            Ok(Ok(message)) => events.push_str(&render(&message)),
            other => panic!("no event carrying {marker:?} arrived ({other:?}); saw: {events}"),
        }
    }
    events
}

async fn webhook_sink() -> (
    std::net::SocketAddr,
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<String>>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let bodies = Arc::new(std::sync::Mutex::new(String::new()));
    let (hits_task, bodies_task) = (hits.clone(), bodies.clone());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (hits, bodies) = (hits_task.clone(), bodies_task.clone());
            tokio::spawn(async move {
                let mut collected = Vec::new();
                let mut buffer = [0_u8; 8192];
                loop {
                    let read = tokio::io::AsyncReadExt::read(&mut socket, &mut buffer)
                        .await
                        .unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    collected.extend_from_slice(&buffer[..read]);
                    if let Some(end) = collected.windows(4).position(|w| w == b"\r\n\r\n") {
                        let text = String::from_utf8_lossy(&collected).to_string();
                        let length = text
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if collected.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                hits.fetch_add(1, Ordering::SeqCst);
                bodies
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&collected));
                let _ = tokio::io::AsyncWriteExt::write_all(
                    &mut socket,
                    b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
            });
        }
    });
    (address, hits, bodies)
}

/// Thoughts of a private segment and governance records are never pushed to a
/// webhook, whoever registered it; public thoughts still are.
#[tokio::test(flavor = "multi_thread")]
async fn webhooks_never_receive_private_thoughts() {
    let dir = dir("webhook");
    let (address, hits, bodies) = webhook_sink().await;
    let manager =
        WebhookManager::new_with_policy(dir.clone(), WebhookUrlPolicy::AllowPrivate).unwrap();
    manager
        .register_webhook(format!("http://{address}/hook"), None, None)
        .unwrap();
    let operator = OperatorIdentity::load_or_create(&dir).unwrap();
    let mut chain =
        MentisDb::open_with_key_and_storage_kind(&dir, "vault", StorageAdapterKind::Binary)
            .unwrap();
    chain.with_webhook_manager(manager);

    chain
        .append("alice", ThoughtType::Insight, "PUBLICWORD hello")
        .unwrap();
    for _ in 0..50 {
        if hits.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "the public thought is delivered"
    );

    let key = SigningKey::from_bytes(&[33u8; 32]);
    apply(
        &mut chain,
        &operator,
        &OperatorOp::SetMode {
            mode: AccessMode::Allowlist,
            keys: vec![],
        },
    )
    .unwrap();
    apply(
        &mut chain,
        &operator,
        &OperatorOp::AllowKey {
            agent_id: "builder".into(),
            key_id: "b1".into(),
            public_key: to_hex(&key.verifying_key().to_bytes()),
        },
    )
    .unwrap();
    let head = chain.thoughts().last().unwrap().hash.clone();
    let mut input = ThoughtInput::new(ThoughtType::Insight, "PRIVATEWORD launch codes");
    let payload = thought_payload_v2("vault", "builder", &head, &input);
    input.signing_key_id = Some("b1".into());
    input.thought_signature = Some(StoredSignature::encode_v2(&key.sign(&payload).to_bytes()));
    chain.append_thought("builder", input).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1, "nothing private was pushed");
    let seen = bodies.lock().unwrap().clone();
    assert!(seen.contains("PUBLICWORD"));
    assert!(
        !seen.contains("PRIVATEWORD") && !seen.contains("access-policy"),
        "{seen}"
    );
}

/// The SSE stream reports that a tool was called but not with which arguments.
#[tokio::test]
async fn sse_events_do_not_carry_tool_arguments() {
    let dir = dir("sse");
    let (router, broadcaster) = standard_mcp_router(
        MentisDbServiceConfig::new(dir, "default", StorageAdapterKind::Binary),
        std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    );
    let mut receiver = broadcaster.subscribe();
    // `GET /` is the library's stream (metadata only) plus MentisDB's own
    // `tool_called` events: none of them carries arguments.
    let mut stream_request = Request::get("/")
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    stream_request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            41001,
        ))));
    let streamed = router.clone().oneshot(stream_request).await.unwrap();
    assert_eq!(streamed.status(), axum::http::StatusCode::OK);
    assert_eq!(
        streamed.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {"name": "mentisdb_append", "arguments": {
            "chain_key": "c", "agent_id": "a", "thought_type": "Insight",
            "content": "SSESECRET private text", "tags": ["TAGSECRET"]
        }}
    });
    let mut request = Request::post("/")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            41000,
        ))));
    let response = router.clone().oneshot(request).await.unwrap();
    assert!(response.status().is_success());
    // A later, recognisable call closes the window on the one above.
    let sentinel = Request::post("/")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string(),
        ))
        .unwrap();
    let mut sentinel = sentinel;
    sentinel
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            41002,
        ))));
    assert!(router
        .oneshot(sentinel)
        .await
        .unwrap()
        .status()
        .is_success());
    let events = events_until(&mut receiver, "tools/list", |m| format!("{m:?}")).await;
    assert!(
        events.contains("tool_called") && events.contains("mentisdb_append"),
        "no event seen: {events}"
    );
    assert!(
        !events.contains("SSESECRET") && !events.contains("TAGSECRET"),
        "{events}"
    );

    // What a subscriber of `GET /` actually receives, frame by frame, carries
    // no argument or result either, and still names the tool.
    let mut stream = streamed.into_body();
    let mut wire = String::new();
    while !wire.contains("tools/list") {
        let frame = tokio::time::timeout(
            Duration::from_secs(30),
            std::future::poll_fn(|cx| {
                use axum::body::HttpBody;
                std::pin::Pin::new(&mut stream).poll_frame(cx)
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("the sentinel frame never arrived; saw: {wire}"));
        let Some(Ok(frame)) = frame else {
            panic!("the event stream ended before the sentinel; saw: {wire}")
        };
        wire.push_str(&String::from_utf8_lossy(
            &frame.into_data().unwrap_or_default(),
        ));
    }
    assert!(
        wire.contains("event: tool_called") && wire.contains("mentisdb_append"),
        "no frame on the wire: {wire}"
    );
    assert!(
        !wire.contains("SSESECRET") && !wire.contains("TAGSECRET"),
        "{wire}"
    );
}

/// The legacy `POST /tools/execute` route reports every call, with its raw
/// parameters, to the MCP event handler. The broadcaster that handler feeds
/// (the stream `GET /` serves) must hold the tool name and outcome but not
/// the thought text or the tags: `cloudllm_mcp` broadcasts metadata only
/// unless asked for more (`SsePayloadMode::Full`), and MentisDB never asks.
#[tokio::test]
async fn legacy_tool_calls_do_not_leak_arguments_into_the_event_stream() {
    let (mut server, broadcaster) = mentisdb::server::start_mcp_server(
        std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        MentisDbServiceConfig::new(dir("legacy_sse"), "default", StorageAdapterKind::Binary),
    )
    .await
    .unwrap();
    assert_eq!(broadcaster.payload_mode(), mcp::SsePayloadMode::Minimal);
    let mut receiver = broadcaster.subscribe();
    let response = reqwest::Client::new()
        .post(format!("http://{}/tools/execute", server.local_addr()))
        .json(&json!({"tool": "mentisdb_append", "parameters": {
            "chain_key": "c", "agent_id": "a", "thought_type": "Insight",
            "content": "LEGACYSECRET private text", "tags": ["LEGACYTAG"]
        }}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{:?}", response.status());
    // A later, recognisable call closes the window on the one above.
    let sentinel = reqwest::Client::new()
        .post(format!("http://{}/tools/execute", server.local_addr()))
        .json(&json!({"tool": "mentisdb_list_chains", "parameters": {}}))
        .send()
        .await
        .unwrap();
    assert!(sentinel.status().is_success(), "{:?}", sentinel.status());
    let events = events_until(&mut receiver, "mentisdb_list_chains", |m| m.format()).await;
    assert!(
        events.contains("mentisdb_append"),
        "the call is reported by name: {events}"
    );
    assert!(
        !events.contains("LEGACYSECRET") && !events.contains("LEGACYTAG"),
        "{events}"
    );
    server.shutdown().unwrap();
}
