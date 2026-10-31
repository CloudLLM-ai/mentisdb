//! HTTP execution and output rendering for remote CLI commands.

use super::remote::{
    HttpMethod, RemoteInvocation, Render, Secret, SignWith, Transport, DASHBOARD_PIN_ENV,
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
    fill_stdin(&mut invocation, input)?;
    sign_locally(&mut invocation)?;
    let reply = send(&invocation)?;
    write_reply(&invocation, &reply, out)?;
    check_reply(&invocation, &reply)
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
        SignWith::KeyRegistration => sign_key_registration(&mut body, &secret),
        SignWith::Thought => sign_thought(invocation, &mut body, &secret),
        SignWith::AccessPolicy => sign_access_policy(invocation, &mut body, &secret),
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

fn sign_key_registration(
    body: &mut serde_json::Map<String, Value>,
    secret: &Secret,
) -> Result<(), String> {
    use ed25519_dalek::{Signer, SigningKey};
    let key = SigningKey::from_bytes(&secret.0);
    let public = key.verifying_key().to_bytes();
    let now = chrono::Utc::now().timestamp();
    let payload = crate::signing::key_registration_payload(
        &body_text(body, "chain_key"),
        &body_text(body, "agent_id"),
        &body_text(body, "key_id"),
        &public,
        now,
    );
    body.insert(
        "public_key_hex".into(),
        Value::String(crate::signing::to_hex(&public)),
    );
    body.insert("registered_at_unix_secs".into(), Value::from(now));
    body.insert(
        "registration_signature_hex".into(),
        Value::String(crate::signing::to_hex(&key.sign(&payload).to_bytes())),
    );
    Ok(())
}

/// POST `path` (relative to the REST base of `invocation`) and return the JSON reply.
fn fetch_json(invocation: &RemoteInvocation, path: &str, body: Value) -> Result<Value, String> {
    let base = invocation
        .url
        .find("/v1/")
        .map_or(invocation.url.as_str(), |at| &invocation.url[..at]);
    let mut derived = invocation.clone();
    derived.method = HttpMethod::Post;
    derived.url = format!("{base}{path}");
    derived.body = Some(body);
    derived.secret = None;
    match send(&derived)? {
        Reply::Json(value) => Ok(value),
        Reply::Text(_) => Err(format!("unexpected non-JSON reply from {path}")),
    }
}

fn current_head(invocation: &RemoteInvocation, chain_key: &str) -> Result<String, String> {
    let head = fetch_json(
        invocation,
        "/v1/head",
        serde_json::json!({ "chain_key": chain_key }),
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
    let head = current_head(invocation, &chain)?;
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
fn sign_access_policy(
    invocation: &RemoteInvocation,
    body: &mut serde_json::Map<String, Value>,
    secret: &Secret,
) -> Result<(), String> {
    use crate::access::{policy_thought_input, AccessMode, AllowedKey, PolicyRecord};
    use ed25519_dalek::{Signer, SigningKey};
    let chain = body_text(body, "chain_key");
    let agent = body_text(body, "agent_id");
    let key_id = body_text(body, "signing_key_id");
    let signing = SigningKey::from_bytes(&secret.0);
    let own_public = signing.verifying_key().to_bytes();

    let shown = fetch_json(
        invocation,
        "/v1/access/policy",
        serde_json::json!({ "chain_key": chain }),
    )?;
    let current: crate::access::AccessPolicy = serde_json::from_value(shown["policy"].clone())
        .map_err(|e| format!("cannot read the current policy: {e}"))?;
    let local = |field: &str| {
        invocation
            .local
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    let mode = match local("mode") {
        Some(raw) => raw.parse::<AccessMode>().map_err(|e| e.to_string())?,
        None => current.mode,
    };
    let mut keys = current.keys.clone();
    if let Some(Value::Array(listed)) = invocation.local.get("key") {
        keys = listed
            .iter()
            .filter_map(Value::as_str)
            .map(parse_key_spec)
            .collect::<Result<_, _>>()?;
    } else if invocation.command == "chain access set" && current.seq.is_none() {
        keys = vec![AllowedKey::new(&agent, &key_id, &own_public)];
    }
    if invocation.command == "chain access allow" {
        let (agent_id, key, public) = (
            local("agent_id").unwrap_or_default(),
            local("allow_key_id").unwrap_or_default(),
            local("public_key_hex").unwrap_or_default(),
        );
        let bytes = crate::signing::from_hex(&public).map_err(|e| e.to_string())?;
        keys.retain(|k| !(k.agent_id == agent_id && k.key_id == key));
        keys.push(AllowedKey::new(&agent_id, &key, &bytes));
    }
    if invocation.command == "chain access deny" {
        let (agent_id, key) = (
            local("agent_id").unwrap_or_default(),
            local("allow_key_id").unwrap_or_default(),
        );
        let before = keys.len();
        keys.retain(|k| !(k.agent_id == agent_id && k.key_id == key));
        if keys.len() == before {
            return Err(format!("'{agent_id}/{key}' is not in the chain's key set"));
        }
    }
    if matches!(
        invocation.command.as_str(),
        "chain access allow" | "chain access deny"
    ) && current.seq.is_none()
    {
        return Err(
            "this chain has no access policy yet; create one with 'mentisdb chain access set'"
                .to_string(),
        );
    }
    let seq = current.seq.map_or(0, |s| s + 1);
    let prev = current.head_record_hash.clone().unwrap_or_default();
    let record = PolicyRecord::new(seq, mode, keys, &prev).map_err(|e| e.to_string())?;
    let head = current_head(invocation, &chain)?;
    let payload =
        crate::signing::thought_payload_v2(&chain, &agent, &head, &policy_thought_input(&record));
    body.insert(
        "signature_hex".into(),
        Value::String(crate::signing::to_hex(&signing.sign(&payload).to_bytes())),
    );
    body.insert(
        "record".into(),
        serde_json::to_value(&record).map_err(|e| e.to_string())?,
    );
    Ok(())
}

/// Parse `AGENT:KEY_ID:PUBLIC_KEY_HEX` from `--key`.
fn parse_key_spec(spec: &str) -> Result<crate::access::AllowedKey, String> {
    let mut parts = spec.splitn(3, ':');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(agent), Some(key_id), Some(hex)) if !agent.is_empty() && !key_id.is_empty() => {
            let bytes =
                crate::signing::from_hex(hex).map_err(|e| format!("--key '{spec}': {e}"))?;
            Ok(crate::access::AllowedKey::new(agent, key_id, &bytes))
        }
        _ => Err(format!(
            "--key expects AGENT:KEY_ID:PUBLIC_KEY_HEX, got '{spec}'"
        )),
    }
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
    if invalid + rejected as u64 == 0 {
        Ok(())
    } else {
        Err(format!(
            "{invalid} signature(s) failed verification and {rejected} policy record(s) were rejected"
        ))
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
