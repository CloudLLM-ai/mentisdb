//! Declarative remote commands: one table drives parsing, validation, request
//! building and `--help` for every CLI command that talks to a running daemon.
//!
//! A [`Command`] names a subcommand path (`["chain", "branch"]`), the typed
//! parameters it accepts ([`Flag`]), and the REST or dashboard route it calls.
//! [`parse`] turns `argv` into a [`RemoteInvocation`] (method, URL, JSON body)
//! without touching the network, so the full command surface is testable
//! offline. Subcommand paths resolve by longest match, so groups can grow
//! deeper (`chain access set`) without touching the parser.

use serde_json::{Map, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Default REST base URL.
pub const DEFAULT_REST_URL: &str = "http://127.0.0.1:9472";
/// Default dashboard base URL (the dashboard is TLS-only).
pub const DEFAULT_DASHBOARD_URL: &str = "https://127.0.0.1:9475";
/// Environment variable holding the dashboard PIN the CLI presents.
pub const DASHBOARD_PIN_ENV: &str = "MENTISDB_DASHBOARD_PIN";

/// HTTP verb of a remote command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET`, parameters travel in the query string.
    Get,
    /// `POST`, parameters travel as a JSON body.
    Post,
    /// `DELETE`, parameters travel in the path and query string.
    Delete,
}

/// Which listener a command talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The REST API (bearer-token auth).
    Rest,
    /// The PIN-gated dashboard API.
    Dashboard,
}

/// Where a parameter's value goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A field of the JSON request body (dotted names nest objects).
    Body,
    /// Substituted into a `{field}` segment of the route.
    Path,
    /// A query-string parameter.
    Query,
    /// Consumed by the CLI only (for example the `--yes` confirmation).
    Local,
}

/// The type a parameter's raw text must satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Free text.
    Text,
    /// Signed integer.
    Int,
    /// Floating point number.
    Float,
    /// Floating point number between 0.0 and 1.0.
    Score,
    /// Presence flag without a value; sends `true`.
    Switch,
    /// Explicit `true` or `false`.
    Bool,
    /// One of a closed set (matched ignoring case, `-` and `_`).
    Choice(&'static [&'static str]),
    /// A [`crate::ThoughtType`] name.
    ThoughtType,
    /// A UUID.
    Uuid,
    /// An RFC 3339 timestamp.
    Time,
    /// A file path whose contents become the value (`-` reads stdin).
    FileText,
    /// Hex text decoded to an array of byte values.
    HexBytes,
    /// `Kind:uuid` or `Kind:uuid@chain`, a thought relation.
    Relation,
    /// `NAME=VALUE` pairs collected into an object.
    Pairs,
}

/// One accepted parameter of a command.
#[derive(Debug, Clone)]
pub struct Flag {
    /// Long name without dashes; for positionals, the display name.
    pub long: &'static str,
    /// Optional one-letter alias.
    pub short: Option<char>,
    /// Placeholder shown in help (`<KEY>`).
    pub metavar: &'static str,
    /// JSON field, path segment or query key this parameter fills.
    pub field: &'static str,
    /// Value type.
    pub kind: Kind,
    /// May be given more than once; values collect into an array.
    pub repeat: bool,
    /// Must be supplied.
    pub required: bool,
    /// Where the value goes.
    pub place: Place,
    /// Taken from the argument list by position instead of by name.
    pub positional: bool,
    /// Value used when the parameter is not supplied.
    pub default: Option<&'static str>,
    /// One-line description for `--help`.
    pub help: &'static str,
}

impl Flag {
    /// Named parameter `--long <METAVAR>` filling body field `field`.
    #[must_use]
    pub fn opt(
        long: &'static str,
        field: &'static str,
        kind: Kind,
        metavar: &'static str,
        help: &'static str,
    ) -> Self {
        Self {
            long,
            short: None,
            metavar,
            field,
            kind,
            repeat: false,
            required: false,
            place: Place::Body,
            positional: false,
            default: None,
            help,
        }
    }

    /// Positional parameter filling body field `field`.
    #[must_use]
    pub fn pos(long: &'static str, field: &'static str, kind: Kind, help: &'static str) -> Self {
        Self {
            positional: true,
            ..Self::opt(long, field, kind, "", help)
        }
    }

    /// Mark the parameter as required.
    #[must_use]
    pub fn req(mut self) -> Self {
        self.required = true;
        self
    }

    /// Allow the parameter to repeat.
    #[must_use]
    pub fn many(mut self) -> Self {
        self.repeat = true;
        self
    }

    /// Value applied when the parameter is omitted.
    #[must_use]
    pub fn default_to(mut self, value: &'static str) -> Self {
        self.default = Some(value);
        self
    }

    /// Add a one-letter alias.
    #[must_use]
    pub fn short(mut self, short: char) -> Self {
        self.short = Some(short);
        self
    }

    /// Send the value in the route path.
    #[must_use]
    pub fn in_path(mut self) -> Self {
        self.place = Place::Path;
        self
    }

    /// Send the value in the query string.
    #[must_use]
    pub fn in_query(mut self) -> Self {
        self.place = Place::Query;
        self
    }

    /// Keep the value inside the CLI.
    #[must_use]
    pub fn local(mut self) -> Self {
        self.place = Place::Local;
        self
    }
}

/// How a successful reply is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Render {
    /// Indented JSON (compact with `--json`).
    Json,
    /// The named string field as raw text (the whole envelope with `--json`).
    Text(&'static str),
}

/// A remote command definition.
#[derive(Debug, Clone)]
pub struct Command {
    /// Subcommand words, for example `["agent", "key", "add"]`.
    pub path: &'static [&'static str],
    /// Extra spellings that resolve to the same command.
    pub aliases: &'static [&'static [&'static str]],
    /// One line for command listings.
    pub summary: &'static str,
    /// Long description shown by `--help`.
    pub about: &'static str,
    /// Copy-paste examples shown by `--help`.
    pub examples: Vec<&'static str>,
    /// Accepted parameters.
    pub params: Vec<Flag>,
    /// String body fields every call sends (for example `status = active`).
    pub fixed: Vec<(&'static str, &'static str)>,
    /// Listener to call.
    pub transport: Transport,
    /// HTTP verb.
    pub method: HttpMethod,
    /// Route, with `{field}` placeholders for [`Place::Path`] parameters.
    pub route: &'static str,
    /// Output style.
    pub render: Render,
}

/// A fully parsed remote command, ready to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInvocation {
    /// Space-joined command path, for error messages.
    pub command: String,
    /// HTTP verb.
    pub method: HttpMethod,
    /// Listener to call.
    pub transport: Transport,
    /// Complete URL including the query string.
    pub url: String,
    /// JSON body for `POST` commands.
    pub body: Option<Value>,
    /// Output style.
    pub render: Render,
    /// Print compact JSON instead of the default presentation.
    pub json: bool,
    /// Bearer token or dashboard PIN given on the command line.
    pub credential: Option<String>,
    /// Skip TLS certificate verification.
    pub insecure: bool,
    /// Body field to fill from standard input at run time.
    pub stdin_field: Option<String>,
}

/// Result of resolving `argv` against the catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// Show this help text.
    Help(String),
    /// Run this command.
    Run(Box<RemoteInvocation>),
}

const COMMON_FLAGS_HELP: &str = "\
Connection options (every command):
  --url <URL>            REST base URL (default http://127.0.0.1:9472)
  --dashboard-url <URL>  Dashboard base URL for dashboard commands (default https://127.0.0.1:9475)
  --token <TOKEN>        Bearer token for the REST API (default: $MENTISDB_BEARER_TOKEN)
  --pin <PIN>            Dashboard PIN for dashboard commands (default: $MENTISDB_DASHBOARD_PIN)
  --insecure             Skip TLS certificate verification (always skipped for loopback)
  --json                 Print compact single-line JSON instead of the default presentation
  -h, --help             Show this help
";

fn normalize(label: &str) -> String {
    label
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn all_paths(command: &Command) -> impl Iterator<Item = &'static [&'static str]> + '_ {
    std::iter::once(command.path).chain(command.aliases.iter().copied())
}

fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

fn resolve<'a>(catalog: &'a [Command], parts: &[String]) -> Option<(&'a Command, usize)> {
    catalog
        .iter()
        .flat_map(|command| all_paths(command).map(move |path| (command, path)))
        .filter(|(_, path)| {
            path.len() <= parts.len() && path.iter().zip(parts).all(|(want, got)| want == got)
        })
        .max_by_key(|(_, path)| path.len())
        .map(|(command, path)| (command, path.len()))
}

fn group_commands<'a>(catalog: &'a [Command], group: &str) -> Vec<&'a Command> {
    catalog
        .iter()
        .filter(|command| all_paths(command).any(|path| path[0] == group))
        .collect()
}

/// Whether `word` is the first word of any command in the catalog.
#[must_use]
pub fn is_group(catalog: &[Command], word: &str) -> bool {
    !group_commands(catalog, word).is_empty()
}

/// Resolve `parts` (everything after the program name) against `catalog`.
///
/// Returns `None` when the first word is not a remote command group, so the
/// caller can try the local commands. Otherwise returns help text or a fully
/// built invocation, or an error message describing what is wrong.
pub fn parse(catalog: &[Command], parts: &[String]) -> Option<Result<Parsed, String>> {
    let group = parts.first()?;
    if !is_group(catalog, group) {
        return None;
    }
    let Some((command, consumed)) = resolve(catalog, parts) else {
        return Some(group_resolution(catalog, group, parts));
    };
    let rest = &parts[consumed..];
    if rest.iter().any(|arg| is_help_flag(arg)) || rest.first().is_some_and(|a| a == "help") {
        return Some(Ok(Parsed::Help(command_help(command))));
    }
    Some(build(command, rest).map(|inv| Parsed::Run(Box::new(inv))))
}

fn group_resolution(catalog: &[Command], group: &str, parts: &[String]) -> Result<Parsed, String> {
    let wants_help = parts.len() == 1 || parts[1..].iter().any(|a| is_help_flag(a) || a == "help");
    if wants_help {
        return Ok(Parsed::Help(group_help(catalog, group)));
    }
    Err(format!(
        "Unknown subcommand '{}' for 'mentisdb {group}'; run 'mentisdb {group} --help' for the list",
        parts[1]
    ))
}

struct Collected {
    body: Map<String, Value>,
    path_values: BTreeMap<&'static str, String>,
    query: Vec<(&'static str, String)>,
    stdin_field: Option<String>,
}

#[derive(Default)]
struct Common {
    url: Option<String>,
    dashboard_url: Option<String>,
    credential: Option<String>,
    insecure: bool,
    json: bool,
}

fn build(command: &Command, rest: &[String]) -> Result<RemoteInvocation, String> {
    let name = command.path.join(" ");
    let (common, positionals, named) = split_args(command, rest, &name)?;
    let collected = collect(command, positionals, named, &name)?;
    let base = match command.transport {
        Transport::Rest => common.url.as_deref().unwrap_or(DEFAULT_REST_URL),
        Transport::Dashboard => common
            .dashboard_url
            .as_deref()
            .unwrap_or(DEFAULT_DASHBOARD_URL),
    };
    let url = build_url(base, command.route, &collected)?;
    let body = (command.method == HttpMethod::Post).then_some(Value::Object(collected.body));
    Ok(RemoteInvocation {
        command: name,
        method: command.method,
        transport: command.transport,
        url,
        body,
        render: command.render,
        json: common.json,
        credential: common.credential,
        insecure: common.insecure,
        stdin_field: collected.stdin_field,
    })
}

type Named<'a> = Vec<(&'a Flag, Option<String>)>;

fn split_args<'a>(
    command: &'a Command,
    rest: &[String],
    name: &str,
) -> Result<(Common, Vec<String>, Named<'a>), String> {
    let mut common = Common::default();
    let mut positionals = Vec::new();
    let mut named: Named<'a> = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        let arg = &rest[index];
        index += 1;
        if arg == "--" {
            positionals.extend(rest[index..].iter().cloned());
            break;
        }
        let Some((key, inline)) = flag_key(arg) else {
            positionals.push(arg.clone());
            continue;
        };
        let mut take_value = |what: &str| -> Result<String, String> {
            match &inline {
                Some(value) => Ok(value.clone()),
                None => {
                    let value = rest
                        .get(index)
                        .ok_or_else(|| format!("{what} requires a value"))?;
                    index += 1;
                    Ok(value.clone())
                }
            }
        };
        match key.as_str() {
            "--json" => common.json = true,
            "--insecure" => common.insecure = true,
            "--url" => common.url = Some(take_value("--url")?),
            "--dashboard-url" => common.dashboard_url = Some(take_value("--dashboard-url")?),
            "--token" | "--pin" => common.credential = Some(take_value(&key)?),
            _ => {
                let flag = find_flag(command, &key).ok_or_else(|| {
                    format!(
                        "Unknown flag '{key}' for 'mentisdb {name}'; run 'mentisdb {name} --help'"
                    )
                })?;
                let value = if flag.kind == Kind::Switch {
                    if inline.is_some() {
                        return Err(format!("{key} does not take a value"));
                    }
                    None
                } else {
                    Some(take_value(&key)?)
                };
                named.push((flag, value));
            }
        }
    }
    Ok((common, positionals, named))
}

/// Split `--name=value` into its parts; `None` when `arg` is not a flag.
fn flag_key(arg: &str) -> Option<(String, Option<String>)> {
    if arg.len() < 2 || !arg.starts_with('-') || arg.parse::<f64>().is_ok() {
        return None;
    }
    match arg.split_once('=') {
        Some((key, value)) if key.starts_with("--") => Some((key.to_string(), Some(value.into()))),
        _ => Some((arg.to_string(), None)),
    }
}

fn find_flag<'a>(command: &'a Command, key: &str) -> Option<&'a Flag> {
    command
        .params
        .iter()
        .filter(|f| !f.positional)
        .find(|flag| {
            key.strip_prefix("--").is_some_and(|long| long == flag.long)
                || key
                    .strip_prefix('-')
                    .and_then(|s| s.chars().next().filter(|_| s.len() == 1))
                    .is_some_and(|c| Some(c) == flag.short)
        })
}

fn collect(
    command: &Command,
    positionals: Vec<String>,
    named: Named<'_>,
    name: &str,
) -> Result<Collected, String> {
    let mut collected = Collected {
        body: Map::new(),
        path_values: BTreeMap::new(),
        query: Vec::new(),
        stdin_field: None,
    };
    for (field, value) in &command.fixed {
        collected
            .body
            .insert((*field).to_string(), Value::String((*value).to_string()));
    }
    let mut seen: Vec<&'static str> = Vec::new();
    for (flag, raw) in named {
        apply(&mut collected, flag, raw.as_deref())?;
        seen.push(flag.long);
    }
    let positional_flags: Vec<&Flag> = command.params.iter().filter(|f| f.positional).collect();
    let variadic = positional_flags.last().is_some_and(|f| f.repeat);
    if positionals.len() > positional_flags.len() && !variadic {
        return Err(format!(
            "Unexpected argument '{}' for 'mentisdb {name}'; run 'mentisdb {name} --help'",
            positionals[positional_flags.len()]
        ));
    }
    for (offset, raw) in positionals.iter().enumerate() {
        let flag = positional_flags[offset.min(positional_flags.len().saturating_sub(1))];
        apply(&mut collected, flag, Some(raw))?;
        seen.push(flag.long);
    }
    for flag in command.params.iter().filter(|f| !seen.contains(&f.long)) {
        if let Some(default) = flag.default {
            apply(&mut collected, flag, Some(default))?;
        }
    }
    let missing: Vec<String> = command
        .params
        .iter()
        .filter(|f| f.required && !seen.contains(&f.long))
        .map(|f| {
            if f.positional {
                format!("<{}>", f.long)
            } else {
                format!("--{}", f.long)
            }
        })
        .collect();
    if missing.is_empty() {
        Ok(collected)
    } else {
        Err(format!(
            "'mentisdb {name}' is missing {}; run 'mentisdb {name} --help'",
            missing.join(", ")
        ))
    }
}

fn apply(collected: &mut Collected, flag: &Flag, raw: Option<&str>) -> Result<(), String> {
    let label = if flag.positional {
        format!("<{}>", flag.long)
    } else {
        format!("--{}", flag.long)
    };
    if matches!(flag.place, Place::Local) {
        return Ok(());
    }
    if raw == Some("-") && matches!(flag.kind, Kind::Text | Kind::FileText) && !flag.repeat {
        if collected.stdin_field.is_some() {
            return Err("only one argument may read standard input".to_string());
        }
        collected.stdin_field = Some(flag.field.to_string());
        return Ok(());
    }
    let value = convert(flag.kind, raw).map_err(|why| format!("{label}: {why}"))?;
    match flag.place {
        Place::Path => {
            collected
                .path_values
                .insert(flag.field, scalar_text(&value));
        }
        Place::Query => collected.query.push((flag.field, scalar_text(&value))),
        Place::Body => store(&mut collected.body, flag, value),
        Place::Local => {}
    }
    Ok(())
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn store(body: &mut Map<String, Value>, flag: &Flag, value: Value) {
    let mut segments = flag.field.split('.').peekable();
    let mut cursor = body;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            merge_leaf(cursor, segment, flag, value);
            return;
        }
        let entry = cursor
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        cursor = match entry {
            Value::Object(map) => map,
            _ => return,
        };
    }
}

fn merge_leaf(cursor: &mut Map<String, Value>, key: &str, flag: &Flag, value: Value) {
    if flag.kind == Kind::Pairs {
        let entry = cursor
            .entry(key.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if let (Value::Object(new), Value::Object(existing)) = (value, entry) {
            existing.extend(new);
        }
    } else if flag.repeat {
        match cursor
            .entry(key.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            Value::Array(items) => items.push(value),
            other => *other = Value::Array(vec![value]),
        }
    } else {
        cursor.insert(key.to_string(), value);
    }
}

fn convert(kind: Kind, raw: Option<&str>) -> Result<Value, String> {
    if kind == Kind::Switch {
        return Ok(Value::Bool(true));
    }
    let raw = raw.ok_or_else(|| "a value is required".to_string())?;
    match kind {
        Kind::Text => Ok(Value::String(raw.to_string())),
        Kind::Int => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("expected an integer, got '{raw}'")),
        Kind::Float => parse_float(raw),
        Kind::Score => parse_float(raw).and_then(|value| {
            match value.as_f64().is_some_and(|f| (0.0..=1.0).contains(&f)) {
                true => Ok(value),
                false => Err(format!(
                    "expected a number between 0.0 and 1.0, got '{raw}'"
                )),
            }
        }),
        Kind::Switch => Ok(Value::Bool(true)),
        Kind::Bool => match raw {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("expected true or false, got '{raw}'")),
        },
        Kind::Choice(options) => choice(options, raw),
        Kind::ThoughtType => raw
            .parse::<crate::ThoughtType>()
            .map(|t| Value::String(t.as_str().to_string()))
            .map_err(|e| e.to_string()),
        Kind::Uuid => Uuid::parse_str(raw)
            .map(|u| Value::String(u.to_string()))
            .map_err(|_| format!("expected a UUID, got '{raw}'")),
        Kind::Time => chrono::DateTime::parse_from_rfc3339(raw)
            .map(|t| Value::String(t.with_timezone(&chrono::Utc).to_rfc3339()))
            .map_err(|_| {
                format!("expected an RFC 3339 timestamp such as 2026-01-31T12:00:00Z, got '{raw}'")
            }),
        Kind::FileText => std::fs::read_to_string(raw)
            .map(Value::String)
            .map_err(|e| format!("cannot read '{raw}': {e}")),
        Kind::HexBytes => decode_hex(raw).map(Value::from),
        Kind::Relation => parse_relation(raw),
        Kind::Pairs => raw
            .split_once('=')
            .map(|(k, v)| {
                let mut map = Map::new();
                map.insert(k.to_string(), Value::String(v.to_string()));
                Value::Object(map)
            })
            .ok_or_else(|| format!("expected NAME=VALUE, got '{raw}'")),
    }
}

fn parse_float(raw: &str) -> Result<Value, String> {
    raw.parse::<f64>()
        .ok()
        .filter(|f| f.is_finite())
        .and_then(|f| serde_json::Number::from_f64(f).map(Value::Number))
        .ok_or_else(|| format!("expected a number, got '{raw}'"))
}

fn choice(options: &'static [&'static str], raw: &str) -> Result<Value, String> {
    let wanted = normalize(raw);
    options
        .iter()
        .find(|option| normalize(option) == wanted)
        .map(|option| Value::String((*option).to_string()))
        .ok_or_else(|| format!("'{raw}' is not one of: {}", options.join(", ")))
}

fn decode_hex(raw: &str) -> Result<Vec<u8>, String> {
    let digits: Vec<char> = raw
        .trim()
        .trim_start_matches("0x")
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if digits.is_empty() || !digits.len().is_multiple_of(2) {
        return Err("expected an even number of hex digits".to_string());
    }
    digits
        .chunks(2)
        .map(|pair| {
            let text: String = pair.iter().collect();
            u8::from_str_radix(&text, 16).map_err(|_| format!("'{text}' is not a hex byte"))
        })
        .collect()
}

fn parse_relation(raw: &str) -> Result<Value, String> {
    let (kind, target) = raw
        .split_once(':')
        .ok_or_else(|| format!("expected KIND:THOUGHT_UUID[@CHAIN], got '{raw}'"))?;
    let (id, chain) = match target.split_once('@') {
        Some((id, chain)) => (id, Some(chain)),
        None => (target, None),
    };
    let id = Uuid::parse_str(id).map_err(|_| format!("'{id}' is not a thought UUID"))?;
    let mut map = Map::new();
    map.insert("kind".into(), Value::String(kind.to_string()));
    map.insert("target_id".into(), Value::String(id.to_string()));
    if let Some(chain) = chain {
        map.insert("chain_key".into(), Value::String(chain.to_string()));
    }
    Ok(Value::Object(map))
}

fn percent_encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

fn build_url(base: &str, route: &str, collected: &Collected) -> Result<String, String> {
    let mut path = route.to_string();
    for (field, value) in &collected.path_values {
        path = path.replace(&format!("{{{field}}}"), &percent_encode(value));
    }
    if let Some(start) = path.find('{') {
        let end = path[start..]
            .find('}')
            .map_or(path.len(), |e| start + e + 1);
        return Err(format!(
            "missing value for route segment {}",
            &path[start..end]
        ));
    }
    let mut url = format!("{}{path}", base.trim_end_matches('/'));
    if !collected.query.is_empty() {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(collected.query.iter().map(|(k, v)| (*k, v.as_str())))
            .finish();
        url = format!("{url}?{query}");
    }
    Ok(url)
}

// ── help ─────────────────────────────────────────────────────────────────────

fn usage_line(command: &Command) -> String {
    let mut line = format!("mentisdb {}", command.path.join(" "));
    for flag in command.params.iter().filter(|f| f.positional) {
        let dots = if flag.repeat { "..." } else { "" };
        match flag.required {
            true => line.push_str(&format!(" <{}>{dots}", flag.long)),
            false => line.push_str(&format!(" [<{}>{dots}]", flag.long)),
        }
    }
    for flag in command
        .params
        .iter()
        .filter(|f| !f.positional && f.required)
    {
        line.push_str(&format!(" {}", flag_syntax(flag)));
    }
    if command.params.iter().any(|f| !f.positional && !f.required) {
        line.push_str(" [options]");
    }
    line
}

fn flag_syntax(flag: &Flag) -> String {
    let short = flag.short.map(|c| format!("-{c}, ")).unwrap_or_default();
    match flag.kind {
        Kind::Switch => format!("{short}--{}", flag.long),
        _ => format!("{short}--{} <{}>", flag.long, flag.metavar),
    }
}

fn kind_hint(flag: &Flag) -> String {
    let base = match flag.kind {
        Kind::Choice(options) => format!(" One of: {}.", options.join(", ")),
        Kind::ThoughtType => " A ThoughtType name such as Decision or Insight.".to_string(),
        Kind::Score => " Number from 0.0 to 1.0.".to_string(),
        Kind::Time => " RFC 3339 timestamp.".to_string(),
        Kind::Uuid => " A UUID.".to_string(),
        Kind::HexBytes => " Hex digits.".to_string(),
        Kind::Relation => " Format KIND:THOUGHT_UUID[@CHAIN].".to_string(),
        Kind::Pairs => " Format NAME=VALUE.".to_string(),
        Kind::FileText => " Use - for standard input.".to_string(),
        Kind::Text if flag.positional => " Use - for standard input.".to_string(),
        _ => String::new(),
    };
    let repeat = if flag.repeat { " May be repeated." } else { "" };
    format!("{base}{repeat}")
}

fn help_row(left: &str, right: &str) -> String {
    format!("  {left:<30} {right}\n")
}

/// Full `--help` text for one command.
#[must_use]
pub fn command_help(command: &Command) -> String {
    let mut text = format!(
        "mentisdb {} - {}\n\nUsage:\n  {}\n\n{}\n",
        command.path.join(" "),
        command.summary,
        usage_line(command),
        command.about
    );
    let positional: Vec<&Flag> = command.params.iter().filter(|f| f.positional).collect();
    if !positional.is_empty() {
        text.push_str("\nArguments:\n");
        for flag in positional {
            let req = if flag.required { "(required) " } else { "" };
            text.push_str(&help_row(
                &format!("<{}>", flag.long),
                &format!("{req}{}{}", flag.help, kind_hint(flag)),
            ));
        }
    }
    let named: Vec<&Flag> = command.params.iter().filter(|f| !f.positional).collect();
    if !named.is_empty() {
        text.push_str("\nOptions:\n");
        for flag in named {
            let req = if flag.required { "(required) " } else { "" };
            text.push_str(&help_row(
                &flag_syntax(flag),
                &format!("{req}{}{}", flag.help, kind_hint(flag)),
            ));
        }
    }
    text.push('\n');
    text.push_str(connection_help(command.transport));
    if !command.examples.is_empty() {
        text.push_str("\nExamples:\n");
        for example in &command.examples {
            text.push_str(&format!("  {example}\n"));
        }
    }
    text
}

fn connection_help(transport: Transport) -> &'static str {
    match transport {
        Transport::Rest | Transport::Dashboard => COMMON_FLAGS_HELP,
    }
}

/// `mentisdb <group> --help`: the commands in one group.
#[must_use]
pub fn group_help(catalog: &[Command], group: &str) -> String {
    let mut text = format!("mentisdb {group}\n\nCommands:\n");
    for command in group_commands(catalog, group) {
        text.push_str(&help_row(&command.path.join(" "), command.summary));
    }
    text.push_str(&format!(
        "\nRun 'mentisdb {group} <command> --help' for details and examples.\n\n{COMMON_FLAGS_HELP}"
    ));
    text
}

/// Overview of every remote command group, for `mentisdb --help`.
#[must_use]
pub fn overview(catalog: &[Command], group_summaries: &[(&str, &str)]) -> String {
    let mut text = String::from(
        "Daemon commands (talk to a running mentisdb over REST or the dashboard API):\n",
    );
    for (group, summary) in group_summaries {
        text.push_str(&help_row(group, summary));
    }
    text.push_str("\nAll commands:\n");
    for command in catalog {
        text.push_str(&help_row(&command.path.join(" "), command.summary));
    }
    text.push_str(
        "\nRun 'mentisdb <group> --help' or 'mentisdb <group> <command> --help' for details.\n\n",
    );
    text.push_str(COMMON_FLAGS_HELP);
    text
}
