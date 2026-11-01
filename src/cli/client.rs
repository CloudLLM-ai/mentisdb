//! HTTP execution and output rendering for remote CLI commands.

use super::remote::{
    HttpMethod, ReadKey, RemoteInvocation, Render, Secret, SignWith, Transport, DASHBOARD_PIN_ENV,
};
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
    if invocation.offline {
        return run_offline(&invocation, out);
    }
    fill_stdin(&mut invocation, input)?;
    sign_locally(&mut invocation)?;
    let reply = send(&invocation)?;
    write_reply(&invocation, &reply, out)?;
    check_reply(&invocation, &reply)
}

/// Apply an operator command straight to the data directory on this host.
///
/// Filesystem access to the data directory is the operator's authority, so no
/// PIN is needed; the operator key in `<data dir>/operator/` signs the policy
/// record. It refuses while a daemon answers on the REST address, because two
/// processes must not append to one chain.
fn run_offline(invocation: &RemoteInvocation, out: &mut dyn Write) -> Result<(), String> {
    use crate::operator::{apply, OperatorIdentity, OperatorOp};
    let url = url::Url::parse(&invocation.url).map_err(|e| e.to_string())?;
    let chain_key = url
        .path_segments()
        .and_then(|mut segments| {
            segments
                .by_ref()
                .skip_while(|segment| *segment != "chains")
                .nth(1)
                .map(str::to_string)
        })
        .filter(|_| url.path().ends_with("/access"))
        .ok_or_else(|| {
            format!(
                "'mentisdb {}' cannot run with --offline: only operator commands (chain access, agent key, agent disable/activate) can",
                invocation.command
            )
        })?;
    let chain_key = percent_decode(&chain_key);
    let op: OperatorOp = match invocation.method {
        HttpMethod::Get => OperatorOp::Show,
        _ => serde_json::from_value(invocation.body.clone().unwrap_or(Value::Null))
            .map_err(|e| format!("invalid operator command: {e}"))?,
    };
    let dir = invocation
        .data_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("MENTISDB_DIR").map(std::path::PathBuf::from))
        .unwrap_or_else(crate::paths::default_mentisdb_dir);
    let Some(_lock) = crate::operator::DataDirLock::try_acquire(&dir).map_err(|e| e.to_string())?
    else {
        return Err(format!(
            "a mentisdb process is using {}: two processes must not write one chain. Use the dashboard route (--pin) or stop the daemon first",
            dir.display()
        ));
    };
    let registered = crate::load_registered_chains(&dir).map_err(|e| e.to_string())?;
    if !registered.chains.contains_key(&chain_key) {
        return Err(format!(
            "no chain '{chain_key}' in {} (check --data-dir)",
            dir.display()
        ));
    }
    let operator = OperatorIdentity::load_or_create(&dir).map_err(|e| e.to_string())?;
    let mut chain = crate::MentisDb::open_with_key(&dir, &chain_key).map_err(|e| e.to_string())?;
    let value = apply(&mut chain, &operator, &op).map_err(|e| e.to_string())?;
    chain.flush().map_err(|e| e.to_string())?;
    write_reply(invocation, &Reply::Json(value), out)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escaped = (bytes[i] == b'%' && i + 3 <= bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(value) => {
                out.push(value);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Perform the local signing a command asked for, filling the signature fields.
fn sign_locally(invocation: &mut RemoteInvocation) -> Result<(), String> {
    let (Some(sign), Some(secret)) = (invocation.sign, invocation.secret.clone()) else {
        return Ok(());
    };
    let Some(Value::Object(mut body)) = invocation.body.take() else {
        return Ok(());
    };
    let result = match sign {
        SignWith::Thought => sign_thought(invocation, &mut body, &secret),
    };
    invocation.body = Some(Value::Object(body));
    result
}

fn body_text(body: &serde_json::Map<String, Value>, field: &str) -> String {
    body.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// POST `path` (relative to the REST base of `invocation`) and return the JSON reply.
fn fetch_json(
    invocation: &RemoteInvocation,
    path: &str,
    body: Value,
    signer: Option<ReadKey>,
) -> Result<Value, String> {
    let base = invocation
        .url
        .find("/v1/")
        .map_or(invocation.url.as_str(), |at| &invocation.url[..at]);
    let mut derived = invocation.clone();
    derived.method = HttpMethod::Post;
    derived.url = format!("{base}{path}");
    derived.body = Some(body);
    derived.secret = None;
    if derived.read_key.is_none() {
        derived.read_key = signer;
    }
    match send(&derived)? {
        Reply::Json(value) => Ok(value),
        Reply::Text(_) => Err(format!("unexpected non-JSON reply from {path}")),
    }
}

/// The chain head a signature must name. A signer proves with its own key that
/// it may read the chain: on a private chain the head is not shown to anyone else.
fn current_head(
    invocation: &RemoteInvocation,
    chain_key: &str,
    signer: Option<ReadKey>,
) -> Result<String, String> {
    let head = fetch_json(
        invocation,
        "/v1/head",
        serde_json::json!({ "chain_key": chain_key }),
        signer,
    )?;
    Ok(head["head_hash"].as_str().unwrap_or_default().to_string())
}

fn sign_thought(
    invocation: &RemoteInvocation,
    body: &mut serde_json::Map<String, Value>,
    secret: &Secret,
) -> Result<(), String> {
    use ed25519_dalek::{Signer, SigningKey};
    let chain = body_text(body, "chain_key");
    let agent = body_text(body, "agent_id");
    let input = thought_input_from_body(body, invocation.command == "thought retro")?;
    let signer = ReadKey {
        seed: secret.clone(),
        agent_id: agent.clone(),
        key_id: body_text(body, "signing_key_id"),
    };
    let head = current_head(invocation, &chain, Some(signer))?;
    let payload = crate::signing::thought_payload_v2(&chain, &agent, &head, &input);
    let signature = SigningKey::from_bytes(&secret.0).sign(&payload).to_bytes();
    body.remove("thought_signature");
    body.insert(
        "thought_signature_hex".into(),
        Value::String(crate::signing::to_hex(
            &crate::signing::StoredSignature::encode_v2(&signature),
        )),
    );
    Ok(())
}

/// Rebuild the `ThoughtInput` the daemon will construct from an append body, so
/// the payload signed here equals the one it verifies.
fn thought_input_from_body(
    body: &serde_json::Map<String, Value>,
    retrospective: bool,
) -> Result<crate::ThoughtInput, String> {
    use crate::{MemoryScope, ThoughtInput, ThoughtRole, ThoughtType};
    let default_type = if retrospective {
        ThoughtType::LessonLearned
    } else {
        ThoughtType::FactLearned
    };
    let thought_type = match body.get("thought_type").and_then(Value::as_str) {
        Some(raw) => raw.parse::<ThoughtType>().map_err(|e| e.to_string())?,
        None => default_type,
    };
    let mut input = ThoughtInput::new(thought_type, body_text(body, "content"));
    input.role = if retrospective {
        ThoughtRole::Retrospective
    } else {
        body.get("role")
            .map(|v| serde_json::from_value::<ThoughtRole>(v.clone()).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or(ThoughtRole::Memory)
    };
    input.importance = body
        .get("importance")
        .and_then(Value::as_f64)
        .map_or(if retrospective { 0.7 } else { 0.5 }, |v| v as f32);
    input.confidence = body
        .get("confidence")
        .and_then(Value::as_f64)
        .map(|v| v as f32);
    let strings = |field: &str| -> Vec<String> {
        body.get(field)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    input.tags = strings("tags");
    input.concepts = strings("concepts");
    input.refs = body
        .get("refs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    for relation in body
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = relation["kind"].as_str().unwrap_or_default();
        input.relations.push(crate::ThoughtRelation {
            kind: relation_kind(kind)?,
            target_id: relation["target_id"]
                .as_str()
                .and_then(|id| id.parse().ok())
                .ok_or("relation target_id must be a UUID")?,
            chain_key: relation["chain_key"].as_str().map(str::to_string),
            valid_at: None,
            invalid_at: None,
        });
    }
    if !retrospective {
        if let Some(scope) = body.get("scope").and_then(Value::as_str) {
            let scope = match scope {
                "user" => MemoryScope::User,
                "session" => MemoryScope::Session,
                "agent" => MemoryScope::Agent,
                other => return Err(format!("unknown scope '{other}'")),
            };
            input = input.with_scope(scope);
        }
        input.entity_type = body
            .get("entity_type")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    Ok(input)
}

fn relation_kind(raw: &str) -> Result<crate::ThoughtRelationKind, String> {
    use crate::ThoughtRelationKind as K;
    let normalized: String = raw
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect();
    Ok(match normalized.as_str() {
        "references" => K::References,
        "summarizes" => K::Summarizes,
        "corrects" => K::Corrects,
        "invalidates" => K::Invalidates,
        "causedby" => K::CausedBy,
        "supports" => K::Supports,
        "contradicts" => K::Contradicts,
        "derivedfrom" => K::DerivedFrom,
        "continuesfrom" => K::ContinuesFrom,
        "branchesfrom" => K::BranchesFrom,
        "relatedto" => K::RelatedTo,
        "supersedes" => K::Supersedes,
        _ => return Err(format!("unknown relation kind '{raw}'")),
    })
}

/// Build, sign and fill the next policy record for `chain access set|allow|deny`.
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
        HttpMethod::Get => {
            let request = with_headers(agent.get(url), invocation, &auth);
            with_read_proof(request, invocation, "GET", &[]).call()
        }
        HttpMethod::Delete => {
            let request = with_headers(agent.delete(url), invocation, &auth);
            with_read_proof(request, invocation, "DELETE", &[]).call()
        }
        HttpMethod::Post => {
            let body = invocation.body.clone().unwrap_or(Value::Null);
            let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
            let request = with_headers(agent.post(url), invocation, &auth)
                .header("Content-Type", "application/json");
            with_read_proof(request, invocation, "POST", &bytes).send(&bytes[..])
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

/// Add the read proof for this exact request when a read key is configured.
///
/// Only REST requests carry it; the dashboard authenticates with its PIN. The
/// signature covers method, path and query, and the body bytes that are sent.
fn with_read_proof<B>(
    request: ureq::RequestBuilder<B>,
    invocation: &RemoteInvocation,
    method: &str,
    body: &[u8],
) -> ureq::RequestBuilder<B> {
    let (Some(key), Transport::Rest) = (&invocation.read_key, invocation.transport) else {
        return request;
    };
    let Ok(parsed) = url::Url::parse(&invocation.url) else {
        return request;
    };
    let target = match parsed.query() {
        Some(query) => format!("{}?{query}", parsed.path()),
        None => parsed.path().to_string(),
    };
    let header = crate::read_auth::sign_request(
        &key.seed.0,
        &key.agent_id,
        &key.key_id,
        method,
        &target,
        body,
        chrono::Utc::now().timestamp(),
        &crate::read_auth::fresh_nonce(),
    );
    request.header(crate::read_auth::READ_AUTH_HEADER, header)
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

/// Turn a verification report with failures into a non-zero exit.
fn check_reply(invocation: &RemoteInvocation, reply: &Reply) -> Result<(), String> {
    let Reply::Json(value) = reply else {
        return Ok(());
    };
    if !invocation.fail_on_invalid {
        return Ok(());
    }
    let invalid = value["invalid"].as_u64().unwrap_or(0);
    let rejected = value["policy"]["invalid_records"]
        .as_array()
        .map_or(0, Vec::len);
    let quarantined = value["quarantined"].as_array().map_or(0, Vec::len);
    if invalid + rejected as u64 + quarantined as u64 == 0 {
        Ok(())
    } else {
        Err(format!(
            "{invalid} signature(s) failed verification, {rejected} policy record(s) were rejected and {quarantined} thought(s) were quarantined at open"
        ))
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
