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
    if invocation.generates_key {
        return run_generate_key(invocation, out);
    }
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
    let value = offline_apply(invocation, &mut |_| Ok(()))?;
    write_reply(invocation, &Reply::Json(value), out)
}

/// The chain an operator command administers, taken from its route.
fn operator_chain_key(invocation: &RemoteInvocation) -> Result<String, String> {
    let url = url::Url::parse(&invocation.url).map_err(|e| e.to_string())?;
    url.path_segments()
        .and_then(|mut segments| {
            segments
                .by_ref()
                .skip_while(|segment| *segment != "chains")
                .nth(1)
                .map(str::to_string)
        })
        .filter(|_| url.path().ends_with("/access"))
        .map(|chain| percent_decode(&chain))
        .ok_or_else(|| {
            format!(
                "'mentisdb {}' cannot run with --offline: only operator commands (chain access, agent key, agent disable/activate) can",
                invocation.command
            )
        })
}

/// Apply the operator command of `invocation` offline and return the chain's
/// state afterwards. `before_apply` runs once the data directory is locked and
/// the chain is open, and can still refuse (`agent key generate` uses it to
/// reject a taken key id and to save the key file before anything is changed).
fn offline_apply(
    invocation: &RemoteInvocation,
    before_apply: &mut dyn FnMut(&crate::MentisDb) -> Result<(), String>,
) -> Result<Value, String> {
    use crate::operator::{apply, OperatorIdentity, OperatorOp};
    let chain_key = operator_chain_key(invocation)?;
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
    let _lock = crate::operator::DataDirLock::claim(&dir).map_err(|e| e.to_string())?;
    let registered = crate::load_registered_chains(&dir).map_err(|e| e.to_string())?;
    if !registered.chains.contains_key(&chain_key) {
        return Err(format!(
            "no chain '{chain_key}' in {} (check --data-dir)",
            dir.display()
        ));
    }
    let operator = OperatorIdentity::load_or_create(&dir).map_err(|e| e.to_string())?;
    let mut chain = crate::MentisDb::open_with_key(&dir, &chain_key).map_err(|e| e.to_string())?;
    before_apply(&chain)?;
    let value = apply(&mut chain, &operator, &op).map_err(|e| e.to_string())?;
    chain.flush().map_err(|e| e.to_string())?;
    Ok(value)
}

/// `agent key generate`: create a key pair, save the seed, register the public key.
///
/// The order is fixed: generate, refuse a taken key id, write the seed file
/// (never over an existing file without `--force`), register the public key,
/// print. Registration is the step that can fail for reasons outside the CLI
/// (bad PIN, daemon down), so the file is written first and kept when it
/// fails: the error then names the file and the `agent key add` command that
/// registers it later. The seed is only ever written to that file; it is not
/// printed, logged, sent or part of any error.
fn run_generate_key(invocation: &RemoteInvocation, out: &mut dyn Write) -> Result<(), String> {
    use ed25519_dalek::SigningKey;
    let seed = crate::signing::fresh_seed().map_err(|e| format!("cannot generate a key: {e}"))?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    let public_hex = crate::signing::to_hex(&public);
    let Some(Value::Object(mut body)) = invocation.body.clone() else {
        return Err("invalid operator command".to_string());
    };
    let (agent, key_id) = (body_text(&body, "agent_id"), body_text(&body, "key_id"));
    if agent.is_empty() || key_id.is_empty() {
        return Err("an agent id and a --key-id are required".to_string());
    }
    let chain = operator_chain_key(invocation)?;
    body.insert("public_key".into(), Value::String(public_hex.clone()));
    let mut registration = invocation.clone();
    registration.body = Some(Value::Object(body));

    let out_path = invocation.local.get("out").and_then(Value::as_str);
    let force = invocation.local.get("force") == Some(&Value::Bool(true));
    let saved: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
    let save = || -> Result<(), String> {
        let path = super::key::save_new_seed(out_path, force, &seed, &public)?;
        *saved.borrow_mut() = Some(path);
        Ok(())
    };
    let registered = if invocation.offline {
        offline_apply(&registration, &mut |chain_db| {
            let taken = chain_db
                .get_agent(&agent)
                .is_some_and(|record| record.public_keys.iter().any(|k| k.key_id == key_id));
            refuse_taken_key(taken, &agent, &key_id, &chain)?;
            save()
        })
        .map(|_| ())
    } else {
        let taken = online_key_taken(invocation, &chain, &agent, &key_id)?;
        refuse_taken_key(taken, &agent, &key_id, &chain)?;
        save().and_then(|()| send(&registration).map(|_| ()))
    };
    let path = saved.into_inner();
    match (registered, path) {
        (Ok(()), Some(path)) => {
            write_generated_key(invocation, out, &chain, &agent, &key_id, &public_hex, &path)
        }
        (Ok(()), None) => Err("the key file was not written".to_string()),
        (Err(error), Some(path)) => Err(format!(
            "{error}\nthe key was saved at {path} (owner-only) and can be registered later with:\n  mentisdb agent key add {} --key-id {} --chain {} --public-key-hex {public_hex}\n(add the --pin or --offline and connection flags you used here)",
            shell_word(&agent),
            shell_word(&key_id),
            shell_word(&chain),
        )),
        (Err(error), None) => Err(error),
    }
}

fn refuse_taken_key(taken: bool, agent: &str, key_id: &str, chain: &str) -> Result<(), String> {
    if taken {
        Err(format!(
            "agent '{agent}' already has a key '{key_id}' on chain '{chain}' (a key id is never reused, even after revoking); pick a new --key-id. No key file was written"
        ))
    } else {
        Ok(())
    }
}

/// Whether the dashboard already lists `key_id` for `agent`. This also proves
/// the PIN and the daemon address before a key file is written; an agent the
/// registry has not seen yet has no keys.
fn online_key_taken(
    invocation: &RemoteInvocation,
    chain: &str,
    agent: &str,
    key_id: &str,
) -> Result<bool, String> {
    let base = invocation
        .url
        .find("/dashboard/")
        .map_or(invocation.url.as_str(), |at| &invocation.url[..at]);
    let mut lookup = invocation.clone();
    lookup.method = HttpMethod::Get;
    lookup.body = None;
    lookup.url = format!(
        "{base}/dashboard/api/agents/{}/{}",
        percent_encode_segment(chain),
        percent_encode_segment(agent)
    );
    match send(&lookup) {
        Ok(Reply::Json(record)) => Ok(record["public_keys"]
            .as_array()
            .is_some_and(|keys| keys.iter().any(|k| k["key_id"] == key_id))),
        Ok(Reply::Text(_)) => Err("unexpected non-JSON reply from the agent lookup".to_string()),
        Err(error) if error.starts_with("HTTP 404 ") && error.contains("agent '") => Ok(false),
        Err(error) => Err(error),
    }
}

fn percent_encode_segment(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Quote `text` for a POSIX shell unless it is plainly safe.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:@%+=".contains(c));
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// Print the outcome of `agent key generate`: the public key and where the
/// private one is, never the private key itself.
fn write_generated_key(
    invocation: &RemoteInvocation,
    out: &mut dyn Write,
    chain: &str,
    agent: &str,
    key_id: &str,
    public_hex: &str,
    path: &str,
) -> Result<(), String> {
    let text = if invocation.json {
        let reply = serde_json::json!({
            "registered": true,
            "chain_key": chain,
            "agent_id": agent,
            "key_id": key_id,
            "public_key_hex": public_hex,
            "key_file": path,
        });
        format!("{reply}\n")
    } else {
        let mode = if cfg!(unix) { "0600" } else { "owner-only" };
        format!(
            "Registered key {key_id} for agent {agent} on chain {chain}\npublic_key_hex: {public_hex}\nprivate key saved to {path} ({mode})\n\n!! This file is the only copy of the private key. MentisDB keeps only\n!! the public key and cannot recover it. Back it up somewhere safe now;\n!! never paste it into a chat or give it to an AI agent.\n"
        )
    };
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())
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
    // Never follow a redirect: the request carries the bearer token, the
    // dashboard PIN and the read proof (`X-Mentis-Auth`), and ureq forwards
    // custom headers to wherever a redirect points. A daemon does not redirect
    // its API; one that does is misconfigured or not the daemon.
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
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
    if (300..400).contains(&status) {
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        // The dashboard answers an unauthenticated request with a redirect to
        // its login page.
        if invocation.transport == Transport::Dashboard && location.contains("/dashboard/login") {
            return Err(format!(
                "the dashboard at {url} asked for a login (PIN missing or wrong); pass --pin or set {DASHBOARD_PIN_ENV}"
            ));
        }
        return Err(redirect_refusal(url, status, location));
    }
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

/// The error for a `3xx` reply: the CLI does not follow redirects, so a
/// credential never leaves for a host the operator did not name.
fn redirect_refusal(url: &str, status: u16, location: &str) -> String {
    format!(
        "HTTP {status} from {url}: the daemon redirected to '{location}'. The CLI does not \
         follow redirects, because the request carries your credentials; point --url at \
         the final address"
    )
}

/// Add the read proof for this exact request when a read key is configured.
///
/// Only REST requests carry it; the dashboard authenticates with its PIN. The
/// signature covers method, path and query, and the body bytes that are sent;
/// the body's SHA-256 travels in `X-Mentis-Content-Sha256` as well, which lets a
/// current daemon accept a body over 1 MiB.
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
    // Also declare the digest the proof signs, so the daemon can verify the
    // proof before it reads the body and accept one over 1 MiB (an older daemon
    // ignores the header and keeps its 1 MiB limit).
    let digest =
        crate::read_auth::content_digest_header_value(&crate::read_auth::body_digest(body));
    request
        .header(crate::read_auth::READ_AUTH_HEADER, header)
        .header(crate::read_auth::CONTENT_DIGEST_HEADER, digest)
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
