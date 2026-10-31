//! HTTP execution and output rendering for remote CLI commands.

use super::remote::{HttpMethod, RemoteInvocation, Render, Transport, DASHBOARD_PIN_ENV};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::time::Duration;

/// Header the dashboard requires from non-browser clients on mutating requests.
const DASHBOARD_CSRF_HEADER: &str = "X-MentisDB-Dashboard";

/// Execute `invocation` and print the reply to `out`.
///
/// Standard input is read only when the invocation asked for it (`-` as a
/// content argument).
pub fn run(
    invocation: &RemoteInvocation,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<(), String> {
    let mut invocation = invocation.clone();
    fill_stdin(&mut invocation, input)?;
    let reply = send(&invocation)?;
    write_reply(&invocation, &reply, out)
}

fn fill_stdin(invocation: &mut RemoteInvocation, input: &mut dyn BufRead) -> Result<(), String> {
    let Some(field) = invocation.stdin_field.clone() else {
        return Ok(());
    };
    let mut text = String::new();
    input
        .read_to_string(&mut text)
        .map_err(|e| format!("reading standard input: {e}"))?;
    if let Some(Value::Object(body)) = invocation.body.as_mut() {
        insert_dotted(body, &field, Value::String(text));
    }
    Ok(())
}

fn insert_dotted(body: &mut serde_json::Map<String, Value>, field: &str, value: Value) {
    match field.split_once('.') {
        None => {
            body.insert(field.to_string(), value);
        }
        Some((head, tail)) => {
            let entry = body
                .entry(head.to_string())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Value::Object(map) = entry {
                insert_dotted(map, tail, value);
            }
        }
    }
}

fn is_loopback(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(|c| c == '[' || c == ']')
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

fn agent_for(invocation: &RemoteInvocation) -> ureq::Agent {
    let skip_verify = invocation.insecure || is_loopback(&invocation.url);
    let tls = ureq::tls::TlsConfig::builder()
        .disable_verification(skip_verify)
        .build();
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(300)))
        .tls_config(tls)
        .build()
        .into()
}

fn credential(invocation: &RemoteInvocation) -> Option<String> {
    let env_name = match invocation.transport {
        Transport::Rest => super::BEARER_TOKEN_ENV,
        Transport::Dashboard => DASHBOARD_PIN_ENV,
    };
    invocation
        .credential
        .clone()
        .or_else(|| std::env::var(env_name).ok())
        .and_then(|value| super::bearer_authorization_header(Some(&value)))
}

/// A decoded daemon reply.
enum Reply {
    Json(Value),
    Text(String),
}

fn send(invocation: &RemoteInvocation) -> Result<Reply, String> {
    let agent = agent_for(invocation);
    let url = invocation.url.as_str();
    let auth = credential(invocation);
    let response = match invocation.method {
        HttpMethod::Get => with_headers(agent.get(url), invocation, &auth).call(),
        HttpMethod::Delete => with_headers(agent.delete(url), invocation, &auth).call(),
        HttpMethod::Post => {
            let body = invocation.body.clone().unwrap_or(Value::Null);
            with_headers(agent.post(url), invocation, &auth).send_json(&body)
        }
    }
    .map_err(|e| format!("cannot reach {url}: {e} (is the daemon running? use --url)"))?;

    let status = response.status().as_u16();
    let is_json = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    let is_html = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("html"));
    let text = response
        .into_body()
        .read_to_string()
        .map_err(|e| format!("reading response from {url}: {e}"))?;
    if is_html && invocation.transport == Transport::Dashboard {
        return Err(format!(
            "the dashboard at {url} asked for a login (PIN missing or wrong); pass --pin or set {DASHBOARD_PIN_ENV}"
        ));
    }
    if status >= 400 {
        return Err(format!(
            "HTTP {status} from {url}: {}",
            error_message(&text)
        ));
    }
    if is_json {
        serde_json::from_str(&text)
            .map(Reply::Json)
            .map_err(|e| format!("parse response from {url}: {e}"))
    } else {
        Ok(Reply::Text(text))
    }
}

fn with_headers<B>(
    request: ureq::RequestBuilder<B>,
    invocation: &RemoteInvocation,
    auth: &Option<String>,
) -> ureq::RequestBuilder<B> {
    let request = match auth {
        Some(value) => request.header("Authorization", value),
        None => request,
    };
    match invocation.transport {
        Transport::Dashboard => request.header(DASHBOARD_CSRF_HEADER, "1"),
        Transport::Rest => request,
    }
}

fn error_message(body: &str) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(body) else {
        return body.trim().to_string();
    };
    let field = |name: &str| map.get(name).and_then(Value::as_str);
    match (field("error"), field("message")) {
        (Some(error), Some(message)) => format!("{error}: {message}"),
        (Some(error), None) => error.to_string(),
        _ => body.trim().to_string(),
    }
}

fn write_reply(
    invocation: &RemoteInvocation,
    reply: &Reply,
    out: &mut dyn Write,
) -> Result<(), String> {
    let text = match (reply, invocation.render, invocation.json) {
        (Reply::Text(text), _, _) => text.clone(),
        (Reply::Json(value), Render::Text(field), false) => value
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| pretty(value)),
        (Reply::Json(value), _, true) => value.to_string(),
        (Reply::Json(value), _, false) => pretty(value),
    };
    let newline = if text.ends_with('\n') { "" } else { "\n" };
    write!(out, "{text}{newline}").map_err(|e| e.to_string())
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
