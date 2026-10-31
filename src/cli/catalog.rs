//! The command table: every daemon-facing `mentisdb` subcommand.
//!
//! Each entry mirrors one MCP tool, REST route or dashboard action; the map
//! lives in `docs/CLI_PARITY.md`. Parameter names follow the REST request
//! structs in `src/server.rs`, so a flag's `field` is the JSON key the daemon
//! reads.

use super::remote::{Command, Flag, HttpMethod, Kind, Render, Transport};

const ROLES: &[&str] = &[
    "Memory",
    "WorkingMemory",
    "Summary",
    "Compression",
    "Checkpoint",
    "Handoff",
    "Audit",
    "Retrospective",
];
const SCOPES: &[&str] = &["user", "session", "agent"];
const GRAPH_MODES: &[&str] = &["outgoing", "incoming", "bidirectional"];
const BOUNDARIES: &[&str] = &["genesis", "head"];
const DIRECTIONS: &[&str] = &["forward", "backward"];
const WINDOW_UNITS: &[&str] = &["seconds", "milliseconds"];
const STORAGE_ADAPTERS: &[&str] = &["binary", "jsonl"];
const AGENT_STATUSES: &[&str] = &["active", "revoked"];
const KEY_ALGORITHMS: &[&str] = &["ed25519"];
const SKILL_FORMATS: &[&str] = &["markdown", "json"];
const SKILL_STATUSES: &[&str] = &["active", "deprecated", "revoked"];
/// `mentisdb agents` is the historical spelling of `agent list`.
const AGENTS_ALIAS: &[&[&str]] = &[&["agents"]];
const VECTOR_PROVIDERS: &[&str] = &["local-text-v1", "fastembed-minilm"];

/// One line per command group, in display order.
pub const GROUP_SUMMARIES: &[(&str, &str)] = &[
    ("thought", "append, read and walk thoughts"),
    (
        "search",
        "ranked, lexical, filtered, federated and bundle search",
    ),
    (
        "chain",
        "list, inspect, bootstrap, branch, merge and delete chains",
    ),
    (
        "agent",
        "the agent registry: identities, aliases, keys, lifecycle",
    ),
    ("entity-type", "entity types for thoughts"),
    (
        "memory",
        "export, import and extract MEMORY.md style memory",
    ),
    ("skill", "the versioned skill registry"),
    ("webhook", "thought-appended webhooks"),
    ("vectors", "managed vector sidecars"),
    ("daemon", "health, version, flush, restart and settings"),
];

fn cmd(
    path: &'static [&'static str],
    summary: &'static str,
    about: &'static str,
    transport: Transport,
    method: HttpMethod,
    route: &'static str,
) -> Command {
    Command {
        path,
        aliases: &[],
        summary,
        about,
        examples: Vec::new(),
        params: Vec::new(),
        fixed: Vec::new(),
        transport,
        method,
        route,
        render: Render::Json,
    }
}

fn rest_post(
    path: &'static [&'static str],
    summary: &'static str,
    about: &'static str,
    route: &'static str,
) -> Command {
    cmd(
        path,
        summary,
        about,
        Transport::Rest,
        HttpMethod::Post,
        route,
    )
}

fn rest_get(
    path: &'static [&'static str],
    summary: &'static str,
    about: &'static str,
    route: &'static str,
) -> Command {
    cmd(
        path,
        summary,
        about,
        Transport::Rest,
        HttpMethod::Get,
        route,
    )
}

fn dash_get(
    path: &'static [&'static str],
    summary: &'static str,
    about: &'static str,
    route: &'static str,
) -> Command {
    cmd(
        path,
        summary,
        about,
        Transport::Dashboard,
        HttpMethod::Get,
        route,
    )
}

fn dash_post(
    path: &'static [&'static str],
    summary: &'static str,
    about: &'static str,
    route: &'static str,
) -> Command {
    cmd(
        path,
        summary,
        about,
        Transport::Dashboard,
        HttpMethod::Post,
        route,
    )
}

impl Command {
    fn with(mut self, params: Vec<Flag>) -> Self {
        self.params.extend(params);
        self
    }

    fn example(mut self, example: &'static str) -> Self {
        self.examples.push(example);
        self
    }

    fn text(mut self, field: &'static str) -> Self {
        self.render = Render::Text(field);
        self
    }

    fn fixed(mut self, field: &'static str, value: &'static str) -> Self {
        self.fixed.push((field, value));
        self
    }

    fn aliases(mut self, aliases: &'static [&'static [&'static str]]) -> Self {
        self.aliases = aliases;
        self
    }
}

// ── shared parameters ────────────────────────────────────────────────────────

fn chain() -> Flag {
    Flag::opt(
        "chain",
        "chain_key",
        Kind::Text,
        "KEY",
        "Chain key (default: the daemon's default chain).",
    )
    .short('c')
}

fn chain_path() -> Flag {
    Flag::pos("chain", "chain_key", Kind::Text, "Chain key.")
        .req()
        .in_path()
}

fn agent_id() -> Flag {
    Flag::pos("agent-id", "agent_id", Kind::Text, "Agent id.").req()
}

fn confirm(what: &'static str) -> Flag {
    Flag::opt("yes", "", Kind::Switch, "", what).req().local()
}

fn agent_identity() -> Vec<Flag> {
    vec![
        Flag::opt("agent", "agent_id", Kind::Text, "ID", "Producing agent id."),
        Flag::opt(
            "agent-name",
            "agent_name",
            Kind::Text,
            "NAME",
            "Producing agent display name.",
        ),
        Flag::opt(
            "agent-owner",
            "agent_owner",
            Kind::Text,
            "OWNER",
            "Producing agent owner or tenant label.",
        ),
    ]
}

fn limit_offset() -> Vec<Flag> {
    vec![
        Flag::opt(
            "limit",
            "limit",
            Kind::Int,
            "N",
            "Maximum number of results.",
        ),
        Flag::opt(
            "offset",
            "offset",
            Kind::Int,
            "N",
            "Skip this many results.",
        ),
    ]
}

/// Filters shared by every search-like request.
fn thought_filters() -> Vec<Flag> {
    vec![
        Flag::opt(
            "type",
            "thought_types",
            Kind::ThoughtType,
            "TYPE",
            "Only thoughts of this ThoughtType.",
        )
        .many(),
        Flag::opt(
            "role",
            "roles",
            Kind::Choice(ROLES),
            "ROLE",
            "Only thoughts with this role.",
        )
        .many(),
        Flag::opt(
            "tag",
            "tags_any",
            Kind::Text,
            "TAG",
            "Only thoughts carrying any of these tags.",
        )
        .many(),
        Flag::opt(
            "concept",
            "concepts_any",
            Kind::Text,
            "CONCEPT",
            "Only thoughts mentioning any of these concepts.",
        )
        .many(),
        Flag::opt(
            "agent",
            "agent_ids",
            Kind::Text,
            "ID",
            "Only thoughts written by this agent id.",
        )
        .many(),
        Flag::opt(
            "agent-name",
            "agent_names",
            Kind::Text,
            "NAME",
            "Only thoughts written by this agent name.",
        )
        .many(),
        Flag::opt(
            "agent-owner",
            "agent_owners",
            Kind::Text,
            "OWNER",
            "Only thoughts written by agents of this owner.",
        )
        .many(),
        Flag::opt(
            "min-importance",
            "min_importance",
            Kind::Score,
            "SCORE",
            "Minimum importance.",
        ),
        Flag::opt(
            "min-confidence",
            "min_confidence",
            Kind::Score,
            "SCORE",
            "Minimum confidence.",
        ),
        Flag::opt(
            "since",
            "since",
            Kind::Time,
            "TIME",
            "Only thoughts at or after this time.",
        ),
        Flag::opt(
            "until",
            "until",
            Kind::Time,
            "TIME",
            "Only thoughts at or before this time.",
        ),
        Flag::opt(
            "entity-type",
            "entity_type",
            Kind::Text,
            "NAME",
            "Only thoughts of this entity type.",
        ),
    ]
}

fn include_invalidated() -> Flag {
    Flag::opt(
        "include-invalidated",
        "include_invalidated",
        Kind::Switch,
        "",
        "Include thoughts that a later thought invalidated.",
    )
}

fn graph_flags() -> Vec<Flag> {
    vec![
        Flag::opt(
            "graph-depth",
            "graph.max_depth",
            Kind::Int,
            "N",
            "Expand the relation graph this many hops from each seed.",
        ),
        Flag::opt(
            "graph-max-visited",
            "graph.max_visited",
            Kind::Int,
            "N",
            "Stop graph expansion after visiting this many thoughts.",
        ),
        Flag::opt(
            "graph-mode",
            "graph.mode",
            Kind::Choice(GRAPH_MODES),
            "MODE",
            "Which relation edges to follow.",
        ),
        Flag::opt(
            "graph-seeds",
            "graph.include_seeds",
            Kind::Bool,
            "BOOL",
            "Include the seed thoughts among graph results.",
        ),
    ]
}

fn ranked_flags() -> Vec<Flag> {
    let mut flags = limit_offset();
    flags.extend(graph_flags());
    flags.extend(thought_filters());
    flags.push(include_invalidated());
    flags.extend([
        Flag::opt(
            "as-of",
            "as_of",
            Kind::Time,
            "TIME",
            "Search the chain as it was at this time.",
        ),
        Flag::opt(
            "scope",
            "scope",
            Kind::Choice(SCOPES),
            "SCOPE",
            "Restrict to a memory scope.",
        ),
        Flag::opt(
            "rerank",
            "enable_reranking",
            Kind::Switch,
            "",
            "Rerank the top results.",
        ),
        Flag::opt(
            "rerank-k",
            "rerank_k",
            Kind::Int,
            "N",
            "How many top results to rerank.",
        ),
    ]);
    flags
}

fn signature_flags(key_help: &'static str) -> Vec<Flag> {
    vec![
        Flag::opt(
            "signing-key-id",
            "signing_key_id",
            Kind::Text,
            "KEY_ID",
            key_help,
        ),
        Flag::opt(
            "signature-hex",
            "thought_signature",
            Kind::HexBytes,
            "HEX",
            "Ed25519 signature over the thought payload.",
        ),
    ]
}

fn content(help: &'static str) -> Flag {
    Flag::pos("content", "content", Kind::Text, help).req()
}

// ── catalog ──────────────────────────────────────────────────────────────────

/// Every daemon-facing command, in help order.
#[must_use]
pub fn commands() -> Vec<Command> {
    [
        thought_commands(),
        search_commands(),
        chain_commands(),
        agent_commands(),
        entity_commands(),
        memory_commands(),
        skill_commands(),
        webhook_commands(),
        vector_commands(),
        daemon_commands(),
    ]
    .concat()
}

fn thought_commands() -> Vec<Command> {
    let mut append_params = vec![
        content("Thought text. Use - to read standard input."),
        chain(),
    ];
    append_params.extend(agent_identity());
    append_params.extend([
        Flag::opt(
            "type",
            "thought_type",
            Kind::ThoughtType,
            "TYPE",
            "What the thought means.",
        )
        .short('t')
        .default_to("FactLearned"),
        Flag::opt(
            "role",
            "role",
            Kind::Choice(ROLES),
            "ROLE",
            "How the thought is meant to be used.",
        ),
        Flag::opt(
            "importance",
            "importance",
            Kind::Score,
            "SCORE",
            "Importance from 0.0 to 1.0.",
        ),
        Flag::opt(
            "confidence",
            "confidence",
            Kind::Score,
            "SCORE",
            "Confidence from 0.0 to 1.0.",
        ),
        Flag::opt("tag", "tags", Kind::Text, "TAG", "Tag the thought.").many(),
        Flag::opt(
            "concept",
            "concepts",
            Kind::Text,
            "CONCEPT",
            "Concept the thought mentions.",
        )
        .many(),
        Flag::opt(
            "ref",
            "refs",
            Kind::Int,
            "INDEX",
            "Index of an earlier thought this one refers to.",
        )
        .many(),
        Flag::opt(
            "relation",
            "relations",
            Kind::Relation,
            "REL",
            "Typed relation to another thought, e.g. Corrects:<uuid>.",
        )
        .many(),
        Flag::opt(
            "scope",
            "scope",
            Kind::Choice(SCOPES),
            "SCOPE",
            "Memory scope.",
        ),
        Flag::opt(
            "entity-type",
            "entity_type",
            Kind::Text,
            "NAME",
            "Entity type the thought is about.",
        ),
    ]);
    append_params.extend(signature_flags(
        "Registered agent key id that signed the thought.",
    ));

    let mut retro_params = vec![
        content("What went wrong and the lesson. Use - for standard input."),
        chain(),
    ];
    retro_params.extend(agent_identity());
    retro_params.extend([
        Flag::opt(
            "type",
            "thought_type",
            Kind::ThoughtType,
            "TYPE",
            "ThoughtType (default LessonLearned).",
        )
        .short('t'),
        Flag::opt(
            "importance",
            "importance",
            Kind::Score,
            "SCORE",
            "Importance from 0.0 to 1.0.",
        ),
        Flag::opt(
            "confidence",
            "confidence",
            Kind::Score,
            "SCORE",
            "Confidence from 0.0 to 1.0.",
        ),
        Flag::opt("tag", "tags", Kind::Text, "TAG", "Tag the thought.").many(),
        Flag::opt(
            "concept",
            "concepts",
            Kind::Text,
            "CONCEPT",
            "Concept the thought mentions.",
        )
        .many(),
        Flag::opt(
            "ref",
            "refs",
            Kind::Int,
            "INDEX",
            "Index of an earlier thought this one refers to.",
        )
        .many(),
    ]);
    retro_params.extend(signature_flags(
        "Registered agent key id that signed the thought.",
    ));

    let mut traverse_params = vec![
        chain(),
        Flag::opt(
            "id",
            "anchor_id",
            Kind::Uuid,
            "UUID",
            "Anchor at the thought with this id.",
        ),
        Flag::opt(
            "hash",
            "anchor_hash",
            Kind::Text,
            "HASH",
            "Anchor at the thought with this hash.",
        ),
        Flag::opt(
            "index",
            "anchor_index",
            Kind::Int,
            "N",
            "Anchor at the thought with this index.",
        ),
        Flag::opt(
            "from",
            "anchor_boundary",
            Kind::Choice(BOUNDARIES),
            "END",
            "Anchor at the start or the tip of the chain.",
        ),
        Flag::opt(
            "direction",
            "direction",
            Kind::Choice(DIRECTIONS),
            "DIR",
            "Walk toward newer or older thoughts (default forward).",
        ),
        Flag::opt(
            "include-anchor",
            "include_anchor",
            Kind::Bool,
            "BOOL",
            "Include the anchor thought itself.",
        ),
        Flag::opt(
            "chunk-size",
            "chunk_size",
            Kind::Int,
            "N",
            "Thoughts per page.",
        ),
        Flag::opt(
            "text",
            "text",
            Kind::Text,
            "TEXT",
            "Only thoughts containing this text.",
        ),
    ];
    traverse_params.extend(thought_filters());
    traverse_params.extend([
        Flag::opt(
            "window-start",
            "time_window.start",
            Kind::Int,
            "N",
            "Start of a numeric time window.",
        ),
        Flag::opt(
            "window-delta",
            "time_window.delta",
            Kind::Int,
            "N",
            "Length of the time window.",
        ),
        Flag::opt(
            "window-unit",
            "time_window.unit",
            Kind::Choice(WINDOW_UNITS),
            "UNIT",
            "Unit of the time window.",
        ),
    ]);

    let mut candidates_params = vec![
        Flag::pos(
            "text",
            "text",
            Kind::Text,
            "Only thoughts containing this text.",
        ),
        chain(),
        Flag::opt(
            "window-size",
            "config.window_size",
            Kind::Int,
            "N",
            "Thoughts per candidate window.",
        ),
        Flag::opt(
            "overlap",
            "config.overlap",
            Kind::Int,
            "N",
            "Thoughts shared by neighbouring windows.",
        ),
        Flag::opt(
            "by-session",
            "config.by_session",
            Kind::Bool,
            "BOOL",
            "Group windows by session.",
        ),
        Flag::opt(
            "by-agent",
            "config.by_agent",
            Kind::Bool,
            "BOOL",
            "Group windows by agent.",
        ),
        Flag::opt(
            "by-entity-type",
            "config.by_entity_type",
            Kind::Bool,
            "BOOL",
            "Group windows by entity type.",
        ),
    ];
    candidates_params.extend(limit_offset());
    candidates_params.extend(thought_filters());

    vec![
        rest_post(
            &["thought", "append"],
            "Append a thought",
            "Appends one durable thought to a chain. The text may come from the argument or from standard input (-). Signed chains need --signing-key-id and --signature-hex.",
            "/v1/thoughts",
        )
        .with(append_params)
        .example("mentisdb thought append \"Use UTC everywhere\" --type Decision --tag time --importance 0.9")
        .example("echo \"deploy finished\" | mentisdb thought append - --type Summary --role Checkpoint"),
        rest_post(
            &["thought", "retro"],
            "Append a retrospective lesson",
            "Appends a lesson learned after a mistake or a hard fight with a problem. Retrospectives are retrieved ahead of ordinary memories.",
            "/v1/retrospectives",
        )
        .with(retro_params)
        .example("mentisdb thought retro \"Migrations must run before the seed step\" --tag deploy"),
        rest_post(
            &["thought", "get"],
            "Read one thought",
            "Reads a thought by id, hash or index. Give exactly one of --id, --hash and --index.",
            "/v1/thought",
        )
        .with(vec![
            chain(),
            Flag::opt("id", "thought_id", Kind::Uuid, "UUID", "Thought id."),
            Flag::opt("hash", "thought_hash", Kind::Text, "HASH", "Thought hash."),
            Flag::opt("index", "thought_index", Kind::Int, "N", "Thought index in the chain."),
        ])
        .example("mentisdb thought get --index 0")
        .example("mentisdb thought get --id 7c0d... --chain project"),
        rest_post(
            &["thought", "genesis"],
            "Read the first thought of a chain",
            "Returns the genesis thought, the first entry every other thought hashes back to.",
            "/v1/thoughts/genesis",
        )
        .with(vec![chain()])
        .example("mentisdb thought genesis --chain project"),
        rest_post(
            &["thought", "traverse"],
            "Page through a chain in append order",
            "Walks the chain forward or backward from an anchor (id, hash, index, or either end) in pages, applying optional filters. Feed next_cursor from the reply back in with --id/--index to continue.",
            "/v1/thoughts/traverse",
        )
        .with(traverse_params)
        .example("mentisdb thought traverse --from head --direction backward --chunk-size 20")
        .example("mentisdb thought traverse --from genesis --type Decision --json"),
        rest_post(
            &["thought", "recent"],
            "Print the recent-context prompt",
            "Renders the last N thoughts as a prompt block an agent can load to resume work. Prints the prompt text; --json prints the full reply.",
            "/v1/recent-context",
        )
        .with({
            let mut p = vec![
                chain(),
                Flag::opt("last-n", "last_n", Kind::Int, "N", "How many thoughts to include."),
                Flag::opt("agent", "agent_id", Kind::Text, "ID", "Only thoughts of this agent."),
            ];
            p.push(include_invalidated());
            p
        })
        .text("prompt")
        .example("mentisdb thought recent --last-n 20 --chain project"),
        rest_post(
            &["thought", "candidates"],
            "List summary candidates",
            "Groups thoughts into windows that are good candidates for a Summary thought, optionally by session, agent or entity type.",
            "/v1/summary-candidates",
        )
        .with(candidates_params)
        .example("mentisdb thought candidates --window-size 12 --by-agent true"),
    ]
}

fn search_commands() -> Vec<Command> {
    let text_opt = |help: &'static str| Flag::pos("query", "text", Kind::Text, help);
    let mut ranked = vec![text_opt("What to look for."), chain()];
    ranked.extend(ranked_flags());

    let mut filter = vec![text_opt("Substring to look for (optional)."), chain()];
    filter.push(Flag::opt(
        "limit",
        "limit",
        Kind::Int,
        "N",
        "Maximum number of results.",
    ));
    filter.extend(thought_filters());
    filter.push(include_invalidated());

    let lexical = vec![
        Flag::pos("query", "text", Kind::Text, "Words to match.").req(),
        chain(),
        Flag::opt(
            "limit",
            "limit",
            Kind::Int,
            "N",
            "Maximum number of results.",
        ),
        Flag::opt(
            "offset",
            "offset",
            Kind::Int,
            "N",
            "Skip this many results.",
        ),
        Flag::opt(
            "agent",
            "agent_ids",
            Kind::Text,
            "ID",
            "Only thoughts written by this agent id.",
        )
        .many(),
        Flag::opt(
            "type",
            "thought_types",
            Kind::ThoughtType,
            "TYPE",
            "Only thoughts of this ThoughtType.",
        )
        .many(),
    ];

    let mut federated = vec![
        text_opt("What to look for."),
        Flag::opt(
            "chain",
            "chain_keys",
            Kind::Text,
            "KEY",
            "Chain to search; repeat for each chain.",
        )
        .short('c')
        .many()
        .req(),
    ];
    federated.extend(ranked_flags());

    let mut bundles = vec![text_opt("What to look for."), chain()];
    bundles.extend(limit_offset());
    bundles.extend(graph_flags());
    bundles.extend(thought_filters());
    bundles.push(include_invalidated());

    vec![
        rest_post(
            &["search"],
            "Ranked search (lexical + vector + graph)",
            "Searches a chain with the full ranking pipeline and returns scored hits. A query that is exactly the name of a search mode (lexical, filter, federated, bundles) must follow --.",
            "/v1/ranked-search",
        )
        .with(ranked)
        .example("mentisdb search \"why did we pick Postgres\" --limit 5")
        .example("mentisdb search \"retry policy\" --type Decision --since 2026-01-01T00:00:00Z --json"),
        rest_post(
            &["search", "lexical"],
            "Keyword search with matched terms",
            "Lexical-only search returning each hit's score, matched terms and match sources.",
            "/v1/lexical-search",
        )
        .with(lexical)
        .example("mentisdb search lexical \"connection pool\" --limit 10"),
        rest_post(
            &["search", "filter"],
            "Structured filter search",
            "Filters thoughts by type, role, tags, agent, time range and importance without ranking. The query is an optional substring.",
            "/v1/search",
        )
        .with(filter)
        .example("mentisdb search filter --type Mistake --agent builder --since 2026-06-01T00:00:00Z"),
        rest_post(
            &["search", "federated"],
            "Ranked search across several chains",
            "Runs one ranked search over every --chain given and merges the results by reciprocal rank fusion.",
            "/v1/federated-search",
        )
        .with(federated)
        .example("mentisdb search federated \"auth bug\" --chain backend --chain frontend"),
        rest_post(
            &["search", "bundles"],
            "Search results grouped with their graph support",
            "Returns each seed hit together with the related thoughts that support it, grouped as context bundles.",
            "/v1/context-bundles",
        )
        .with(bundles)
        .example("mentisdb search bundles \"release checklist\" --graph-depth 2"),
    ]
}

fn chain_commands() -> Vec<Command> {
    let mut bootstrap = vec![
        content("Bootstrap summary stored when the chain is empty."),
        chain(),
        Flag::opt(
            "storage-adapter",
            "storage_adapter",
            Kind::Choice(STORAGE_ADAPTERS),
            "KIND",
            "Storage format for a new chain.",
        ),
    ];
    bootstrap.extend(agent_identity());
    bootstrap.extend([
        Flag::opt(
            "importance",
            "importance",
            Kind::Score,
            "SCORE",
            "Importance from 0.0 to 1.0.",
        ),
        Flag::opt(
            "tag",
            "tags",
            Kind::Text,
            "TAG",
            "Tag the bootstrap thought.",
        )
        .many(),
        Flag::opt(
            "concept",
            "concepts",
            Kind::Text,
            "CONCEPT",
            "Concept for the bootstrap thought.",
        )
        .many(),
    ]);

    vec![
        rest_get(
            &["chain", "list"],
            "List chains",
            "Lists every chain with its storage adapter, thought count and agent count, plus the daemon's default chain.",
            "/v1/chains",
        )
        .example("mentisdb chain list --json"),
        rest_post(
            &["chain", "head"],
            "Show a chain's head and integrity",
            "Reports the thought count, head hash, latest thought and whether the hash chain verifies.",
            "/v1/head",
        )
        .with(vec![chain()])
        .example("mentisdb chain head --chain project"),
        rest_post(
            &["chain", "bootstrap"],
            "Create a chain and write its bootstrap thought",
            "Creates the chain if needed and appends the bootstrap summary when the chain is empty. Calling it again on a non-empty chain only returns the existing skills.",
            "/v1/bootstrap",
        )
        .with(bootstrap)
        .example("mentisdb chain bootstrap \"Project Atlas memory\" --chain atlas"),
        rest_post(
            &["chain", "branch"],
            "Branch a chain at a thought",
            "Creates a new chain whose genesis points at the given thought of the source chain; the source is not changed.",
            "/v1/chains/branch",
        )
        .with(vec![
            Flag::opt("from", "source_chain_key", Kind::Text, "KEY", "Chain to branch from.").req(),
            Flag::opt("at", "branch_thought_id", Kind::Uuid, "UUID", "Thought of the source chain to branch at.").req(),
            Flag::opt("name", "branch_chain_key", Kind::Text, "KEY", "Key of the new chain.").req(),
        ])
        .example("mentisdb chain branch --from main --at 7c0d3a52-1b5e-4f4a-9a31-0a8b1d2e3f40 --name experiment"),
        rest_post(
            &["chain", "merge"],
            "Merge one chain into another",
            "Copies every thought of the source chain into the target chain and then deletes the source chain permanently.",
            "/v1/chains/merge",
        )
        .with(vec![
            Flag::opt("from", "source_chain_key", Kind::Text, "KEY", "Chain to merge and delete.").req(),
            Flag::opt("into", "target_chain_key", Kind::Text, "KEY", "Chain that receives the thoughts.").req(),
            confirm("Confirm that the source chain is deleted after the merge."),
        ])
        .example("mentisdb chain merge --from scratch --into main --yes"),
        dash_get(
            &["chain", "info"],
            "Show a chain's detail and vector sidecar status",
            "Reads the dashboard's detail view of one chain: storage, counts, sizes and vector sidecar status. Needs the dashboard PIN when one is set.",
            "/dashboard/api/chains/{chain_key}",
        )
        .with(vec![chain_path()])
        .example("mentisdb chain info project --pin 123456"),
        cmd(
            &["chain", "delete"],
            "Delete a chain permanently",
            "Removes the chain's storage files and registry entry. This cannot be undone; back up first with 'mentisdb backup'. Needs the dashboard PIN when one is set.",
            Transport::Dashboard,
            HttpMethod::Delete,
            "/dashboard/api/chains/{chain_key}",
        )
        .with(vec![chain_path(), confirm("Confirm the permanent deletion.")])
        .example("mentisdb chain delete scratch --yes"),
    ]
}

fn agent_commands() -> Vec<Command> {
    vec![
        rest_post(
            &["agent", "list"],
            "List the agents that wrote to a chain",
            "Lists the agent identities seen on a chain. 'mentisdb agents' is the same command.",
            "/v1/agents",
        )
        .aliases(AGENTS_ALIAS)
        .with(vec![chain()])
        .example("mentisdb agent list --chain project"),
        rest_post(
            &["agent", "get"],
            "Show one agent record",
            "Shows the registry record of an agent: names, owner, aliases, keys and status.",
            "/v1/agent",
        )
        .with(vec![agent_id(), chain()])
        .example("mentisdb agent get builder"),
        rest_post(
            &["agent", "registry"],
            "List the full agent registry",
            "Lists every registry record of a chain, including revoked agents and their keys.",
            "/v1/agent-registry",
        )
        .with(vec![chain()])
        .example("mentisdb agent registry --json"),
        rest_post(
            &["agent", "upsert"],
            "Create or update an agent record",
            "Registers an agent or changes its display name, owner, description or status.",
            "/v1/agents/upsert",
        )
        .with(vec![
            agent_id(),
            chain(),
            Flag::opt("name", "display_name", Kind::Text, "NAME", "Display name."),
            Flag::opt("owner", "agent_owner", Kind::Text, "OWNER", "Owner or tenant label."),
            Flag::opt("description", "description", Kind::Text, "TEXT", "What the agent does."),
            Flag::opt("status", "status", Kind::Choice(AGENT_STATUSES), "STATUS", "Lifecycle status."),
        ])
        .example("mentisdb agent upsert builder --name \"Build bot\" --owner platform"),
        rest_post(
            &["agent", "describe"],
            "Set an agent's description",
            "Replaces the description stored in the agent's registry record.",
            "/v1/agents/description",
        )
        .with(vec![
            agent_id(),
            Flag::pos("description", "description", Kind::Text, "New description.").req(),
            chain(),
        ])
        .example("mentisdb agent describe builder \"Runs CI and posts summaries\""),
        rest_post(
            &["agent", "alias"],
            "Add an alias to an agent",
            "Adds another id the agent is known by.",
            "/v1/agents/aliases",
        )
        .with(vec![
            agent_id(),
            Flag::pos("alias", "alias", Kind::Text, "Alias to add.").req(),
            chain(),
        ])
        .example("mentisdb agent alias builder ci-bot"),
        rest_post(
            &["agent", "key", "add"],
            "Register a public key for an agent",
            "Registers an Ed25519 public key (hex) the agent can sign thoughts and skills with.",
            "/v1/agents/keys",
        )
        .with(vec![
            agent_id(),
            Flag::opt("key-id", "key_id", Kind::Text, "KEY_ID", "Identifier for the key.").req(),
            Flag::opt("public-key-hex", "public_key_bytes", Kind::HexBytes, "HEX", "Ed25519 public key, 32 bytes as hex.").req(),
            Flag::opt("algorithm", "algorithm", Kind::Choice(KEY_ALGORITHMS), "ALG", "Key algorithm.").default_to("ed25519"),
            chain(),
        ])
        .example("mentisdb agent key add builder --key-id ci-2026 --public-key-hex 9f86d081..."),
        rest_post(
            &["agent", "key", "revoke"],
            "Revoke an agent's key",
            "Marks the key revoked; thoughts signed with it before revocation stay valid.",
            "/v1/agents/keys/revoke",
        )
        .with(vec![
            agent_id(),
            Flag::opt("key-id", "key_id", Kind::Text, "KEY_ID", "Key to revoke.").req(),
            chain(),
        ])
        .example("mentisdb agent key revoke builder --key-id ci-2026"),
        rest_post(
            &["agent", "disable"],
            "Disable (revoke) an agent",
            "Marks the agent revoked. Its history is kept.",
            "/v1/agents/disable",
        )
        .with(vec![agent_id(), chain()])
        .example("mentisdb agent disable builder"),
        rest_post(
            &["agent", "activate"],
            "Re-activate a revoked agent",
            "Sets the agent's status back to active.",
            "/v1/agents/upsert",
        )
        .fixed("status", "active")
        .with(vec![agent_id(), chain()])
        .example("mentisdb agent activate builder"),
        dash_post(
            &["agent", "copy"],
            "Copy an agent's memories to another chain",
            "Copies every thought written by the agent from the source chain into a new or existing target chain. Needs the dashboard PIN when one is set.",
            "/dashboard/api/agents/{chain_key}/{agent_id}/copy-to/{target_chain_key}",
        )
        .with(vec![
            Flag::pos("agent-id", "agent_id", Kind::Text, "Agent whose memories are copied.").req().in_path(),
            Flag::opt("from", "chain_key", Kind::Text, "KEY", "Source chain.").req().in_path(),
            Flag::opt("to", "target_chain_key", Kind::Text, "KEY", "Target chain.").req().in_path(),
        ])
        .example("mentisdb agent copy builder --from main --to archive"),
    ]
}

fn entity_commands() -> Vec<Command> {
    vec![
        rest_post(
            &["entity-type", "list"],
            "List entity types",
            "Lists the entity types registered on a chain.",
            "/v1/entity-types",
        )
        .with(vec![chain()])
        .example("mentisdb entity-type list"),
        rest_post(
            &["entity-type", "upsert"],
            "Register an entity type",
            "Registers an entity type so thoughts can be tagged with it.",
            "/v1/entity-types/upsert",
        )
        .with(vec![
            Flag::pos("name", "entity_type", Kind::Text, "Entity type name.").req(),
            chain(),
        ])
        .example("mentisdb entity-type upsert customer"),
    ]
}

fn memory_commands() -> Vec<Command> {
    let mut export = vec![
        Flag::pos(
            "query",
            "text",
            Kind::Text,
            "Only thoughts containing this text.",
        ),
        chain(),
    ];
    export.push(Flag::opt(
        "limit",
        "limit",
        Kind::Int,
        "N",
        "Maximum number of thoughts.",
    ));
    export.extend(thought_filters());

    vec![
        rest_post(
            &["memory", "export"],
            "Export thoughts as MEMORY.md Markdown",
            "Renders matching thoughts as a MEMORY.md style document on standard output; redirect it to a file. --json prints the full reply.",
            "/v1/memory-markdown",
        )
        .with(export)
        .text("markdown")
        .example("mentisdb memory export --agent builder > MEMORY.md")
        .example("mentisdb memory export --type Decision --limit 50"),
        rest_post(
            &["memory", "import"],
            "Import a MEMORY.md file",
            "Parses a MEMORY.md style file and appends its entries as thoughts. Use - for standard input.",
            "/v1/import-markdown",
        )
        .with(vec![
            Flag::pos("file", "markdown", Kind::FileText, "MEMORY.md file to import.").req(),
            chain(),
            Flag::opt("default-agent", "default_agent_id", Kind::Text, "ID", "Agent id for entries that name none."),
        ])
        .example("mentisdb memory import MEMORY.md --default-agent builder"),
        rest_post(
            &["memory", "extract"],
            "Extract memories from free text with the configured LLM",
            "Sends the text to the daemon's LLM extraction pipeline and prints the thoughts it proposes; nothing is appended. Requires an LLM provider configured on the daemon.",
            "/v1/extract-memories",
        )
        .with(vec![
            Flag::pos("file", "text", Kind::FileText, "File with the text to extract from (- for standard input).").req(),
            chain(),
            Flag::opt("agent", "agent_id", Kind::Text, "ID", "Agent the extraction is for."),
            Flag::opt("prompt-template", "prompt_template", Kind::Text, "TEXT", "Override the extraction prompt template."),
        ])
        .example("mentisdb memory extract meeting-notes.txt"),
    ]
}

fn skill_commands() -> Vec<Command> {
    let mut search = vec![
        Flag::pos("query", "text", Kind::Text, "Words to look for in skills."),
        chain(),
    ];
    search.extend([
        Flag::opt("id", "skill_ids", Kind::Text, "ID", "Only this skill id.").many(),
        Flag::opt(
            "name",
            "names",
            Kind::Text,
            "NAME",
            "Only skills with this name.",
        )
        .many(),
        Flag::opt(
            "tag",
            "tags_any",
            Kind::Text,
            "TAG",
            "Only skills with any of these tags.",
        )
        .many(),
        Flag::opt(
            "trigger",
            "triggers_any",
            Kind::Text,
            "TRIGGER",
            "Only skills with any of these triggers.",
        )
        .many(),
        Flag::opt(
            "uploaded-by",
            "uploaded_by_agent_ids",
            Kind::Text,
            "ID",
            "Only skills uploaded by this agent id.",
        )
        .many(),
        Flag::opt(
            "uploaded-by-name",
            "uploaded_by_agent_names",
            Kind::Text,
            "NAME",
            "Only skills uploaded by this agent name.",
        )
        .many(),
        Flag::opt(
            "uploaded-by-owner",
            "uploaded_by_agent_owners",
            Kind::Text,
            "OWNER",
            "Only skills uploaded by this owner.",
        )
        .many(),
        Flag::opt(
            "status",
            "statuses",
            Kind::Choice(SKILL_STATUSES),
            "STATUS",
            "Only skills with this status.",
        )
        .many(),
        Flag::opt(
            "format",
            "formats",
            Kind::Choice(SKILL_FORMATS),
            "FORMAT",
            "Only skills stored in this format.",
        )
        .many(),
        Flag::opt(
            "schema-version",
            "schema_versions",
            Kind::Int,
            "N",
            "Only this skill schema version.",
        )
        .many(),
        Flag::opt(
            "since",
            "since",
            Kind::Time,
            "TIME",
            "Only versions uploaded at or after this time.",
        ),
        Flag::opt(
            "until",
            "until",
            Kind::Time,
            "TIME",
            "Only versions uploaded at or before this time.",
        ),
        Flag::opt(
            "limit",
            "limit",
            Kind::Int,
            "N",
            "Maximum number of results.",
        ),
    ]);

    let lifecycle = |id_help: &'static str| {
        vec![
            Flag::pos("skill-id", "skill_id", Kind::Text, id_help).req(),
            Flag::opt(
                "reason",
                "reason",
                Kind::Text,
                "TEXT",
                "Why, recorded with the change.",
            ),
            chain(),
        ]
    };

    vec![
        rest_get(
            &["skill", "guide"],
            "Print the core MentisDB operating instructions",
            "Prints the skill Markdown agents load after bootstrap (the same text as the mentisdb_skill_md tool).",
            "/mentisdb_skill_md",
        )
        .example("mentisdb skill guide"),
        rest_get(
            &["skill", "manifest"],
            "Show the skill registry manifest",
            "Shows the registry's schema version and supported formats.",
            "/v1/skills/manifest",
        )
        .example("mentisdb skill manifest"),
        rest_get(
            &["skill", "list"],
            "List skills",
            "Lists the skills in the registry with their status and latest version.",
            "/v1/skills",
        )
        .with(vec![chain().in_query()])
        .example("mentisdb skill list --json"),
        rest_post(
            &["skill", "search"],
            "Search skills",
            "Searches skill names, descriptions, tags and triggers, filtered by uploader, status, format and time.",
            "/v1/skills/search",
        )
        .with(search)
        .example("mentisdb skill search \"code review\" --status active"),
        rest_post(
            &["skill", "read"],
            "Read a skill",
            "Prints the skill content in the requested format (default markdown), for the latest version or a given version. --json adds safety warnings and status.",
            "/v1/skills/read",
        )
        .with(vec![
            Flag::pos("skill-id", "skill_id", Kind::Text, "Skill id.").req(),
            Flag::opt("version", "version_id", Kind::Uuid, "UUID", "Version to read (default latest)."),
            Flag::opt("format", "format", Kind::Choice(SKILL_FORMATS), "FORMAT", "Output format."),
            chain(),
        ])
        .text("content")
        .example("mentisdb skill read rust-engineer")
        .example("mentisdb skill read rust-engineer --format json --json"),
        rest_post(
            &["skill", "versions"],
            "List a skill's versions",
            "Lists every uploaded version of a skill with its upload time and uploader.",
            "/v1/skills/versions",
        )
        .with(vec![Flag::pos("skill-id", "skill_id", Kind::Text, "Skill id.").req(), chain()])
        .example("mentisdb skill versions rust-engineer"),
        rest_post(
            &["skill", "upload"],
            "Upload a skill version",
            "Uploads a skill file as a new version. The first upload creates the skill. Use - for standard input. Signed deployments need --signing-key-id and --signature-hex.",
            "/v1/skills/upload",
        )
        .with({
            let mut p = vec![
                Flag::pos("file", "content", Kind::FileText, "Skill file (- for standard input).").req(),
                Flag::opt("agent", "agent_id", Kind::Text, "ID", "Uploading agent id.").req(),
                Flag::opt("id", "skill_id", Kind::Text, "ID", "Skill id (default: derived from the skill name)."),
                Flag::opt("format", "format", Kind::Choice(SKILL_FORMATS), "FORMAT", "Format of the uploaded file (default markdown)."),
                chain(),
            ];
            p.push(Flag::opt("signing-key-id", "signing_key_id", Kind::Text, "KEY_ID", "Registered key id that signed the skill."));
            p.push(Flag::opt("signature-hex", "skill_signature", Kind::HexBytes, "HEX", "Ed25519 signature over the skill content."));
            p
        })
        .example("mentisdb skill upload skills/rust-engineer.md --agent builder"),
        rest_post(
            &["skill", "deprecate"],
            "Deprecate a skill",
            "Marks the skill deprecated: still readable, but a newer skill should be preferred.",
            "/v1/skills/deprecate",
        )
        .with(lifecycle("Skill to deprecate."))
        .example("mentisdb skill deprecate old-checklist --reason \"replaced by release-pipeline\""),
        rest_post(
            &["skill", "revoke"],
            "Revoke a skill",
            "Marks the skill revoked because it is unsafe or wrong. Content and history stay for audit.",
            "/v1/skills/revoke",
        )
        .with(lifecycle("Skill to revoke."))
        .example("mentisdb skill revoke bad-skill --reason \"leaks credentials\""),
        rest_post(
            &["skill", "delete"],
            "Delete a skill",
            "Removes the skill and its versions from the registry.",
            "/v1/skills/delete",
        )
        .with({
            let mut p = lifecycle("Skill to delete.");
            p.retain(|f| f.long != "reason");
            p.push(confirm("Confirm the deletion."));
            p
        })
        .example("mentisdb skill delete scratch-skill --yes"),
        dash_get(
            &["skill", "diff"],
            "Diff two versions of a skill",
            "Prints a unified diff between two versions of a skill's Markdown (default: the first and the latest version). Needs the dashboard PIN when one is set.",
            "/dashboard/api/skills/{skill_id}/diff",
        )
        .with(vec![
            Flag::pos("skill-id", "skill_id", Kind::Text, "Skill id.").req().in_path(),
            Flag::opt("from", "from", Kind::Uuid, "UUID", "Older version id.").in_query(),
            Flag::opt("to", "to", Kind::Uuid, "UUID", "Newer version id.").in_query(),
        ])
        .text("diff")
        .example("mentisdb skill diff rust-engineer --from <version> --to <version>"),
    ]
}

fn webhook_commands() -> Vec<Command> {
    vec![
        rest_get(
            &["webhook", "list"],
            "List webhooks",
            "Lists registered webhooks, their filters and delivery state.",
            "/v1/webhooks",
        )
        .example("mentisdb webhook list"),
        rest_post(
            &["webhook", "register"],
            "Register a webhook",
            "Registers a URL that receives a POST for every matching appended thought. Private and loopback targets are refused unless MENTISDB_WEBHOOK_ALLOWED_CIDRS allows them.",
            "/v1/webhooks",
        )
        .with(vec![
            Flag::pos("url", "url", Kind::Text, "Delivery URL.").req(),
            Flag::opt("chain-filter", "chain_key_filter", Kind::Text, "KEY", "Only thoughts appended to this chain."),
            Flag::opt("type", "thought_type_filter", Kind::ThoughtType, "TYPE", "Only thoughts of this ThoughtType.").many(),
        ])
        .example("mentisdb webhook register https://example.com/hook --type Decision"),
        cmd(
            &["webhook", "delete"],
            "Delete a webhook",
            "Removes a webhook registration by id (see 'mentisdb webhook list').",
            Transport::Rest,
            HttpMethod::Delete,
            "/v1/webhooks/{id}",
        )
        .with(vec![Flag::pos("id", "id", Kind::Uuid, "Webhook id.").req().in_path()])
        .example("mentisdb webhook delete 5b1c0f6e-2d3a-4c8e-9f10-7a6b5c4d3e2f"),
    ]
}

fn vector_commands() -> Vec<Command> {
    let provider = || {
        Flag::opt(
            "provider",
            "provider_key",
            Kind::Choice(VECTOR_PROVIDERS),
            "PROVIDER",
            "Vector provider.",
        )
        .req()
        .in_path()
    };
    let sidecar = |path: &'static [&'static str],
                   summary: &'static str,
                   about: &'static str,
                   route: &'static str| {
        dash_post(path, summary, about, route).with(vec![
            Flag::pos("chain", "chain_key", Kind::Text, "Chain key.")
                .req()
                .in_path(),
            provider(),
        ])
    };
    vec![
        sidecar(
            &["vectors", "enable"],
            "Enable a vector sidecar",
            "Turns on a managed vector sidecar for the chain. Needs the dashboard PIN when one is set.",
            "/dashboard/api/chains/{chain_key}/vectors/{provider_key}/enable",
        )
        .example("mentisdb vectors enable project --provider local-text-v1"),
        sidecar(
            &["vectors", "disable"],
            "Disable a vector sidecar",
            "Turns off a managed vector sidecar; its files are kept.",
            "/dashboard/api/chains/{chain_key}/vectors/{provider_key}/disable",
        )
        .example("mentisdb vectors disable project --provider local-text-v1"),
        sidecar(
            &["vectors", "sync"],
            "Bring a vector sidecar up to date",
            "Embeds the thoughts the sidecar has not seen yet.",
            "/dashboard/api/chains/{chain_key}/vectors/{provider_key}/sync",
        )
        .example("mentisdb vectors sync project --provider local-text-v1"),
        rest_post(
            &["vectors", "rebuild"],
            "Rebuild a vector sidecar from scratch",
            "Deletes the sidecar's index and re-embeds every thought. Slow on large chains.",
            "/v1/vectors/rebuild",
        )
        .with(vec![
            chain(),
            Flag::opt("provider", "provider_key", Kind::Choice(VECTOR_PROVIDERS), "PROVIDER", "Vector provider (default: the chain's default)."),
            confirm("Confirm that the existing index is deleted."),
        ])
        .example("mentisdb vectors rebuild --chain project --yes"),
    ]
}

fn daemon_commands() -> Vec<Command> {
    vec![
        rest_get(
            &["daemon", "health"],
            "Check that the daemon is up",
            "Calls the REST health endpoint; exits non-zero when the daemon cannot be reached.",
            "/health",
        )
        .example("mentisdb daemon health"),
        dash_get(
            &["daemon", "version"],
            "Show the daemon's version",
            "Reads the running daemon's crate version from the dashboard API.",
            "/dashboard/api/version",
        )
        .example("mentisdb daemon version"),
        rest_post(
            &["daemon", "flush"],
            "Flush buffered writes to disk",
            "Flushes every open chain's write buffer; run it before copying the data directory.",
            "/v1/admin/flush",
        )
        .example("mentisdb daemon flush"),
        dash_post(
            &["daemon", "restart"],
            "Restart the daemon",
            "Asks the daemon to restart itself so it re-reads its settings. Needs the dashboard PIN when one is set.",
            "/dashboard/api/restart",
        )
        .with(vec![confirm("Confirm the restart.")])
        .example("mentisdb daemon restart --yes"),
        dash_get(
            &["daemon", "settings", "list"],
            "List the daemon's settings",
            "Lists every MENTISDB_* setting with its current value, default and whether it hot-reloads.",
            "/dashboard/api/settings",
        )
        .example("mentisdb daemon settings list --json"),
        dash_post(
            &["daemon", "settings", "set"],
            "Change daemon settings",
            "Updates one or more MENTISDB_* settings and persists them to the daemon's .env. Settings that cannot hot-reload report restart_required.",
            "/dashboard/api/settings",
        )
        .with(vec![Flag::pos("setting", "settings", Kind::Pairs, "NAME=VALUE setting to change.").req().many()])
        .example("mentisdb daemon settings set MENTISDB_AUTO_FLUSH=false"),
    ]
}
