//! Empirical recall audit for a MentisDB chain.
//!
//! "Users have to repeat themselves" is a retrieval complaint, so this tool
//! measures retrieval instead of arguing about it. It opens a **copy** of a
//! chain, builds synthetic recall queries from the chain's own thoughts, runs
//! them through the production [`MentisDb::query_ranked`] path with default
//! settings (limit 10, thesaurus synonyms applied the way the server applies
//! them) and reports where the answer lands, bucketed by the age of the
//! target thought.
//!
//! It deliberately lives in `examples/` rather than `benches/`: the output is a
//! diagnostic report with several tables, not a timing that should be tracked
//! by Criterion baselines.
//!
//! # Usage
//!
//! ```text
//! # 1. Copy every file that shares the chain stem (never point this at a live
//! #    chain directory: opening a chain may rewrite derived sidecars).
//! cp ~/.cloudllm/mentisdb/frostwire-69728eeb81bcf84a.* /tmp/audit/chains/
//! # 2. Give fastembed a model cache it may write lock files into.
//! cp -R ~/.cloudllm/mentisdb/.fastembed_cache /tmp/audit/fastembed_cache
//! # 3. Run.
//! FASTEMBED_CACHE_DIR=/tmp/audit/fastembed_cache \
//!   cargo run --release --example recall_audit -- /tmp/audit/chains frostwire \
//!   [--sample 300] [--seed 7] [--query-tokens 32] [--rebuild-stale] [--show-misses N] [--df-scope global|field] [--df-cutoff X] [--tail] [--skip-dilution] [--no-embed]
//! ```
//!
//! Arguments: the chain directory, the chain key (`frostwire`, not the file
//! stem), then optional flags:
//!
//! * `--sample N`: targets per query kind (default 300, capped by chain size).
//! * `--seed S`: seed of the deterministic sampler (default 7).
//! * `--query-tokens N`: cap on words in a synthetic query (default 32).
//! * `--rebuild-stale`: when the fastembed sidecar is missing or older than the
//!   chain, re-embed it **in the chain directory you passed** (so only use this on
//!   a copy) before measuring. Without it a stale sidecar skips the hybrid half.
//! * `--show-misses N`: print N concrete lexical-only rank-1 misses (query,
//!   target and the thought that beat it) per probe kind.
//! * `--df-scope global|field`: document-frequency scope of the lexical gate
//!   (default `global`, the shipped behaviour).
//! * `--df-cutoff X`: override the content/tags/concepts gate cutoff
//!   (default 0.30; `1.0` disables the gate).
//! * `--tail`: add `tail` probes (see below).
//! * `--skip-dilution`: skip section `f` (it re-embeds the whole chain twice).
//! * `--no-embed`: skip the sections that load the ONNX model (embedding
//!   dilution) and the hybrid half of self-recall.
//!
//! # Report sections
//!
//! 1. `a` overview: size, age, type by role, invalidation and which kinds of
//!    thought auto-dedup `Supersedes` edges retire.
//! 2. `b` vector coverage: sidecar rows versus thoughts.
//! 3. `c` embedding window: model-token length versus the 512 token cap.
//! 4. `d` document-frequency gating: how much of the vocabulary and of the
//!    query-like terms the `Bm25DfCutoffs` cliff removes.
//! 5. `e` self-recall by age decile, lexical-only versus hybrid, for three
//!    query kinds (`drop50`, `rare`, `ref`), plus near-duplicate displacement.
//! 6. `f` embedding dilution: the agent-registry suffix appended to every
//!    embedded text, with and without.
//!
//! # Query kinds
//!
//! * `drop50`: the target's own words with half of them dropped at random.
//! * `rare`: only the target's six rarest words, in a stable order.
//! * `ref`: the opening words of a *later* thought that `refs` the target,
//!   which is the closest synthetic stand-in for a user paraphrasing.
//!
//! * `sem`: the opening words of the target's nearest neighbour by embedding
//!   among thoughts sharing few words with it (same topic, different words).
//! * `tail` (`--tail`): words from the part of a long thought that falls
//!   beyond the embedding window.
//!
//! The sampler is a fixed xorshift generator, so two runs over the same chain
//! and seed produce identical queries.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use mentisdb::search::fastembed_provider::{cache_dir_from_env, FastEmbedProvider};
use mentisdb::search::lexical::{
    normalize_lexical_tokens, Bm25DfCutoffs, DfScope, LexicalField, LexicalIndex,
};
use mentisdb::search::thesaurus;
use mentisdb::search::{EmbeddingInput, EmbeddingMetadata, EmbeddingProvider};
use mentisdb::{
    EmbeddingsMode, MentisDb, RankedSearchQuery, Thought, ThoughtRelationKind, ThoughtType,
};
use uuid::Uuid;

/// Model-token window of the fastembed provider (`max_length` in fastembed).
const EMBED_WINDOW: usize = 512;
/// Window MiniLM-L6 was actually trained with.
const TRAINED_WINDOW: usize = 256;
/// Probe kinds in report order.
const KINDS: [&str; 5] = ["drop50", "rare", "ref", "sem", "tail"];
/// Hits materialised per query so that rank and "no signal" are both observable.
const DEEP_LIMIT_FLOOR: usize = 100;

/// Parsed command line.
struct Args {
    dir: PathBuf,
    key: String,
    sample: usize,
    seed: u64,
    query_tokens: usize,
    embed: bool,
    rebuild_stale: bool,
    show_misses: usize,
    df_scope: DfScope,
    df_cutoff: Option<f32>,
    tail: bool,
    dilution: bool,
}

impl Args {
    /// Parse `std::env::args`, exiting with a usage line on error.
    fn parse() -> Self {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let usage = "usage: recall_audit <chain-dir> <chain-key> [--sample N] [--seed S] \
                     [--query-tokens N] [--rebuild-stale] [--show-misses N] [--df-scope global|field] [--df-cutoff X] [--tail] [--skip-dilution] [--no-embed]";
        if argv.len() < 2 {
            eprintln!("{usage}");
            std::process::exit(2);
        }
        let flag = |name: &str| -> Option<String> {
            argv.iter()
                .position(|a| a == name)
                .and_then(|i| argv.get(i + 1).cloned())
        };
        let num = |name: &str, default: u64| -> u64 {
            flag(name).and_then(|v| v.parse().ok()).unwrap_or(default)
        };
        Self {
            dir: PathBuf::from(&argv[0]),
            key: argv[1].clone(),
            sample: num("--sample", 300) as usize,
            seed: num("--seed", 7),
            query_tokens: num("--query-tokens", 32) as usize,
            embed: !argv.iter().any(|a| a == "--no-embed"),
            rebuild_stale: argv.iter().any(|a| a == "--rebuild-stale"),
            show_misses: num("--show-misses", 0) as usize,
            df_scope: flag("--df-scope")
                .map(|v| v.parse().expect("--df-scope must be global or field"))
                .unwrap_or_default(),
            df_cutoff: flag("--df-cutoff").and_then(|v| v.parse().ok()),
            tail: argv.iter().any(|a| a == "--tail"),
            dilution: !argv.iter().any(|a| a == "--skip-dilution"),
        }
    }
}

/// Deterministic xorshift64* generator; no external RNG so reports reproduce
/// across dependency upgrades.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// One synthetic recall query and the thought it should retrieve.
struct Probe {
    kind: &'static str,
    target: usize,
    /// Thought that authored the query text; never counted as a hit.
    exclude: Option<usize>,
    text: String,
}

/// Result of running one probe against one backend.
#[derive(Clone, Default)]
struct Outcome {
    /// 1-based rank of the target, `None` when it is absent from every hit.
    rank: Option<usize>,
    /// Target is absent because it has no lexical, vector or graph signal.
    no_signal: bool,
    /// Index of the top-1 hit.
    top: Option<usize>,
    target_lex: f32,
    target_vec: f32,
    target_total: f32,
    top_total: f32,
    top_importance: f32,
    top_recency: f32,
}

fn main() {
    let args = Args::parse();
    let chain = open_lexical(&args);
    println!("# recall_audit: {} ({})", args.key, args.dir.display());
    section_overview(&chain);
    let hybrid_chain = args.embed.then(|| open_hybrid(&args)).flatten();
    section_coverage(hybrid_chain.as_ref().unwrap_or(&chain));
    let tokenizer = load_tokenizer();
    section_window(&chain, tokenizer.as_ref());
    let vectors = hybrid_chain.as_ref().map(sidecar_vectors);
    let probes = build_probes(&chain, &args, vectors.as_ref(), tokenizer.as_ref());
    section_gating(&chain, &probes);
    let lexical = run_probes(&chain, &probes, false, &args);
    let hybrid = hybrid_chain
        .as_ref()
        .map(|handle| run_probes(handle, &probes, false, &args));
    let hybrid_rrf = hybrid_chain
        .as_ref()
        .map(|handle| run_probes(handle, &probes, true, &args));
    let mut modes = vec![Mode {
        name: "lexical-only",
        outcomes: &lexical,
    }];
    if let Some(outcomes) = hybrid.as_deref() {
        modes.push(Mode {
            name: "hybrid",
            outcomes,
        });
    }
    if let Some(outcomes) = hybrid_rrf.as_deref() {
        modes.push(Mode {
            name: "hybrid+rrf",
            outcomes,
        });
    }
    section_recall(&chain, &probes, &modes);
    show_misses(&chain, &probes, &lexical, args.show_misses);
    if args.embed && args.dilution {
        section_dilution(&chain, &probes);
    }
}

/// Thought index to stored fastembed vector, from the chain's sidecar.
fn sidecar_vectors(chain: &MentisDb) -> HashMap<usize, Vec<f32>> {
    chain
        .load_vector_sidecar(&fastembed_metadata())
        .ok()
        .flatten()
        .map(|sidecar| {
            sidecar
                .entries
                .into_iter()
                .map(|e| (e.thought_index as usize, e.vector))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- opening

/// Open the chain with managed vector sidecars disabled: lexical-only.
fn open_lexical(args: &Args) -> MentisDb {
    let mut chain = MentisDb::open_with_key(&args.dir, &args.key).expect("open chain copy");
    chain
        .apply_persisted_managed_vector_sidecars_with_mode(EmbeddingsMode::Off)
        .expect("disable sidecars");
    chain
}

/// Open the chain with its fastembed sidecar loaded, or `None` when the
/// sidecar is missing or stale (a stale sidecar would be rebuilt in the
/// background and silently degrade the numbers).
fn open_hybrid(args: &Args) -> Option<MentisDb> {
    let metadata = fastembed_metadata();
    let is_fresh = |chain: &MentisDb| {
        chain
            .load_vector_sidecar(&metadata)
            .ok()
            .flatten()
            .is_some_and(|sidecar| {
                chain
                    .vector_sidecar_freshness(&sidecar, &metadata)
                    .is_ok_and(|f| matches!(f, mentisdb::search::VectorSidecarFreshness::Fresh))
            })
    };
    let mut chain = MentisDb::open_with_key(&args.dir, &args.key).expect("open chain copy");
    if !is_fresh(&chain) && args.rebuild_stale {
        let provider = FastEmbedProvider::try_new().expect("load fastembed model");
        chain
            .rebuild_vector_sidecar(&provider)
            .expect("rebuild fastembed sidecar");
        println!(
            "\n(rebuilt the fastembed sidecar in {})",
            args.dir.display()
        );
        chain = MentisDb::open_with_key(&args.dir, &args.key).expect("reopen chain copy");
    }
    if !is_fresh(&chain) {
        println!("\n(hybrid skipped: fastembed sidecar missing or stale; pass --rebuild-stale on a copy)");
        return None;
    }
    chain
        .apply_persisted_managed_vector_sidecars_with_mode(EmbeddingsMode::FastEmbed)
        .expect("enable fastembed sidecar");
    Some(chain)
}

fn fastembed_metadata() -> EmbeddingMetadata {
    EmbeddingMetadata::new(
        mentisdb::search::fastembed_provider::FASTEMBED_MINILM_MODEL_ID,
        mentisdb::search::fastembed_provider::FASTEMBED_MINILM_DIMENSION,
        mentisdb::search::fastembed_provider::FASTEMBED_MINILM_VERSION,
    )
}

// ------------------------------------------------------------ helpers

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        100.0 * part as f64 / whole as f64
    }
}

fn token_set(text: &str) -> HashSet<String> {
    normalize_lexical_tokens(text, true).into_iter().collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        0.0
    } else {
        a.intersection(b).count() as f64 / union as f64
    }
}

fn snippet(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    flat.chars().take(max).collect()
}

fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Coarse value ranking used only to flag suspicious supersessions:
/// 2 = durable decision/preference/fact, 0 = transient or derived, 1 = rest.
fn value_rank(kind: ThoughtType) -> u8 {
    use ThoughtType::*;
    match kind {
        PreferenceUpdate | UserTrait | Decision | Constraint | LessonLearned | Correction
        | Mistake | FactLearned | Goal | RelationshipUpdate => 2,
        Question | Wonder | Summary | Checkpoint | StateSnapshot | Handoff | ActionTaken
        | TaskComplete | Hypothesis | LLMExtracted => 0,
        _ => 1,
    }
}

// ------------------------------------------------------------------ a

fn section_overview(chain: &MentisDb) {
    let thoughts = chain.thoughts();
    let n = thoughts.len();
    println!("\n## a. overview");
    let Some(first) = thoughts.first() else {
        println!("empty chain");
        return;
    };
    let last = thoughts.last().expect("non-empty");
    println!(
        "thoughts: {n}   span: {} .. {}   agents: {}",
        first.timestamp.format("%Y-%m-%d"),
        last.timestamp.format("%Y-%m-%d"),
        chain.agent_registry().agents.len()
    );
    let invalid = chain.invalidated_thought_ids();
    println!(
        "invalidated: {} ({:.1}%)",
        invalid.len(),
        pct(invalid.len(), n)
    );

    println!("\nage deciles (by append index):");
    for d in 0..10 {
        let lo = n * d / 10;
        let hi = (n * (d + 1) / 10).max(lo + 1).min(n);
        let slice = &thoughts[lo..hi];
        let inv = slice.iter().filter(|t| invalid.contains(&t.id)).count();
        println!(
            "  d{d}: idx {lo:>4}..{hi:<4} {} .. {}  n={:<4} invalidated={inv}",
            slice[0].timestamp.format("%Y-%m-%d"),
            slice.last().expect("slice").timestamp.format("%Y-%m-%d"),
            slice.len()
        );
    }

    let mut grid: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for t in thoughts {
        *grid
            .entry(format!("{:?}", t.thought_type))
            .or_default()
            .entry(format!("{:?}", t.role))
            .or_default() += 1;
    }
    println!("\ntype x role:");
    for (kind, roles) in &grid {
        let cells: Vec<String> = roles.iter().map(|(r, c)| format!("{r}={c}")).collect();
        let inv = thoughts
            .iter()
            .filter(|t| format!("{:?}", t.thought_type) == *kind && invalid.contains(&t.id))
            .count();
        println!(
            "  {kind:<20} total={:<4} invalidated={inv:<4} {}",
            roles.values().sum::<usize>(),
            cells.join(" ")
        );
    }

    supersession_report(chain);
    duplicate_report(chain);
}

fn supersession_report(chain: &MentisDb) {
    let thoughts = chain.thoughts();
    let by_id: HashMap<Uuid, usize> = thoughts.iter().map(|t| (t.id, t.index as usize)).collect();
    let mut edges = 0usize;
    let mut auto_like = 0usize;
    let mut imp_down = 0usize;
    let mut type_down = 0usize;
    let mut role_down = 0usize;
    let mut cross_type = 0usize;
    let mut pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut worst: Vec<(usize, usize, f64)> = Vec::new();
    for t in thoughts {
        for rel in &t.relations {
            if rel.kind != ThoughtRelationKind::Supersedes || rel.chain_key.is_some() {
                continue;
            }
            let Some(&ti) = by_id.get(&rel.target_id) else {
                continue;
            };
            let target = &thoughts[ti];
            edges += 1;
            let sim = jaccard(&token_set(&t.content), &token_set(&target.content));
            if sim >= 0.85 {
                auto_like += 1;
            }
            *pairs
                .entry((
                    format!("{:?}", t.thought_type),
                    format!("{:?}", target.thought_type),
                ))
                .or_default() += 1;
            let mut bad = false;
            if t.thought_type != target.thought_type {
                cross_type += 1;
            }
            if target.importance > t.importance + 1e-6 {
                imp_down += 1;
                bad = true;
            }
            if value_rank(t.thought_type) < value_rank(target.thought_type) {
                type_down += 1;
                bad = true;
            }
            if format!("{:?}", target.role) == "Memory" && format!("{:?}", t.role) != "Memory" {
                role_down += 1;
                bad = true;
            }
            if bad {
                worst.push((t.index as usize, ti, sim));
            }
        }
    }
    println!("\nSupersedes edges: {edges} (jaccard>=0.85, i.e. auto-dedup-like: {auto_like})");
    println!(
        "  cross-type: {cross_type}   target more important: {imp_down}   \
         type value downgrade: {type_down}   Memory role superseded by non-Memory: {role_down}"
    );
    for ((from, to), count) in pairs.iter().filter(|((f, t), _)| f != t) {
        println!("  {from} supersedes {to}: {count}");
    }
    worst.sort_by(|a, b| b.2.total_cmp(&a.2));
    for (from, to, sim) in worst.iter().take(8) {
        let (f, t) = (&thoughts[*from], &thoughts[*to]);
        println!(
            "  e.g. #{from} {:?}/{:?} imp={:.2} supersedes #{to} {:?}/{:?} imp={:.2} (jaccard {sim:.2})\n       new: {}\n       old: {}",
            f.thought_type,
            f.role,
            f.importance,
            t.thought_type,
            t.role,
            t.importance,
            snippet(&f.content, 90),
            snippet(&t.content, 90)
        );
    }
}

/// Near-duplicate clusters among live thoughts (token Jaccard >= 0.85), and how
/// far apart their members sit relative to the auto-dedup scan window.
fn duplicate_report(chain: &MentisDb) {
    let thoughts = chain.thoughts();
    let invalid = chain.invalidated_thought_ids();
    let live: Vec<usize> = (0..thoughts.len())
        .filter(|&i| !invalid.contains(&thoughts[i].id))
        .collect();
    let sets: Vec<HashSet<String>> = live
        .iter()
        .map(|&i| token_set(&thoughts[i].content))
        .collect();
    let mut parent: Vec<usize> = (0..live.len()).collect();
    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while parent[root] != root {
            root = parent[root];
        }
        let mut cur = x;
        while parent[cur] != root {
            let next = parent[cur];
            parent[cur] = root;
            cur = next;
        }
        root
    }
    let (mut close_pairs, mut far_pairs) = (0usize, 0usize);
    for a in 0..live.len() {
        for b in (a + 1)..live.len() {
            let (la, lb) = (sets[a].len(), sets[b].len());
            if la.min(lb) * 100 < la.max(lb) * 85 || la == 0 {
                continue;
            }
            if jaccard(&sets[a], &sets[b]) >= 0.85 {
                let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                parent[ra] = rb;
                if live[b] - live[a] <= 64 {
                    close_pairs += 1;
                } else {
                    far_pairs += 1;
                }
            }
        }
    }
    let mut clusters: HashMap<usize, Vec<usize>> = HashMap::new();
    for (a, &index) in live.iter().enumerate() {
        let root = find(&mut parent, a);
        clusters.entry(root).or_default().push(index);
    }
    let mut multi: Vec<Vec<usize>> = clusters.into_values().filter(|c| c.len() > 1).collect();
    multi.sort_by_key(|c| std::cmp::Reverse(c.len()));
    let in_clusters: usize = multi.iter().map(Vec::len).sum();
    println!(
        "\nnear-duplicate clusters (jaccard>=0.85, live thoughts): {} clusters holding {in_clusters} of {} live thoughts; \
         duplicate pairs within the 64-thought dedup window: {close_pairs}, beyond it: {far_pairs}",
        multi.len(),
        live.len()
    );
    for cluster in multi.iter().take(5) {
        let first = &thoughts[cluster[0]];
        println!(
            "  size {:<3} {:?} idx {}..{}  {}",
            cluster.len(),
            first.thought_type,
            cluster[0],
            cluster.last().expect("non-empty"),
            snippet(&first.content, 80)
        );
    }
}

// ------------------------------------------------------------------ b

fn section_coverage(chain: &MentisDb) {
    println!("\n## b. vector coverage");
    let n = chain.thoughts().len();
    let metadata = fastembed_metadata();
    let sidecar = chain.load_vector_sidecar(&metadata).ok().flatten();
    let Some(sidecar) = sidecar else {
        println!("fastembed sidecar: none");
        return;
    };
    let have: HashSet<Uuid> = sidecar.entries.iter().map(|e| e.thought_id).collect();
    let missing: Vec<&Thought> = chain
        .thoughts()
        .iter()
        .filter(|t| !have.contains(&t.id))
        .collect();
    let stale = sidecar
        .entries
        .iter()
        .filter(|e| {
            chain
                .thoughts()
                .get(e.thought_index as usize)
                .is_none_or(|t| t.hash != e.thought_hash)
        })
        .count();
    let freshness = chain.vector_sidecar_freshness(&sidecar, &metadata);
    println!(
        "sidecar rows {} / thoughts {n}  missing {} ({:.1}%)  hash-mismatched rows {stale}  freshness {:?}  generated {}",
        sidecar.entries.len(),
        missing.len(),
        pct(missing.len(), n),
        freshness.ok(),
        sidecar.generated_at.format("%Y-%m-%d")
    );
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    for t in &missing {
        *by_type.entry(format!("{:?}", t.thought_type)).or_default() += 1;
    }
    if !by_type.is_empty() {
        println!("  lexical-only thoughts by type: {by_type:?}");
    }
}

// ------------------------------------------------------------------ c

fn load_tokenizer() -> Option<tokenizers::Tokenizer> {
    let root = cache_dir_from_env();
    let found = find_file(&root, "tokenizer.json")?;
    let mut tokenizer = tokenizers::Tokenizer::from_file(found).ok()?;
    tokenizer.with_truncation(None).ok()?;
    tokenizer.with_padding(None);
    Some(tokenizer)
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
    }
    None
}

fn token_len(tokenizer: &tokenizers::Tokenizer, text: &str) -> usize {
    tokenizer
        .encode(text, true)
        .map(|e| e.get_ids().len())
        .unwrap_or(0)
}

/// Embedding text without the agent-registry lines (content, concepts, tags).
fn bare_embedding_text(thought: &Thought) -> String {
    let mut sections = Vec::new();
    let content = thought.content.trim();
    if !content.is_empty() {
        sections.push(content.to_string());
    }
    if !thought.concepts.is_empty() {
        sections.push(format!("Concepts: {}", thought.concepts.join(", ")));
    }
    if !thought.tags.is_empty() {
        sections.push(format!("Tags: {}", thought.tags.join(", ")));
    }
    sections.join("\n")
}

fn section_window(chain: &MentisDb, tokenizer: Option<&tokenizers::Tokenizer>) {
    println!(
        "\n## c. embedding window ({EMBED_WINDOW} token cap, MiniLM trained at {TRAINED_WINDOW})"
    );
    let Some(tokenizer) = tokenizer else {
        println!("tokenizer.json not found under the fastembed cache; skipped");
        return;
    };
    struct Acc {
        n: usize,
        over256: usize,
        over512: usize,
        total: usize,
        lost: usize,
        meta_lost: usize,
    }
    let mut by_type: BTreeMap<String, Acc> = BTreeMap::new();
    let mut suffix_total = 0usize;
    let mut all_total = 0usize;
    let mut lens = Vec::new();
    for t in chain.thoughts() {
        let full = token_len(tokenizer, &chain.embedding_text(t));
        let bare = token_len(tokenizer, &bare_embedding_text(t));
        let content_only = token_len(tokenizer, t.content.trim());
        suffix_total += full.saturating_sub(bare);
        all_total += full;
        lens.push(full);
        let acc = by_type
            .entry(format!("{:?}", t.thought_type))
            .or_insert(Acc {
                n: 0,
                over256: 0,
                over512: 0,
                total: 0,
                lost: 0,
                meta_lost: 0,
            });
        acc.n += 1;
        acc.over256 += usize::from(full > TRAINED_WINDOW);
        acc.over512 += usize::from(full > EMBED_WINDOW);
        acc.total += full;
        acc.lost += full.saturating_sub(EMBED_WINDOW);
        // Concepts/Tags sit after the content: invisible when content alone fills the window.
        acc.meta_lost += usize::from(
            content_only >= EMBED_WINDOW && (!t.tags.is_empty() || !t.concepts.is_empty()),
        );
    }
    lens.sort_unstable();
    let q = |p: f64| lens[((lens.len() - 1) as f64 * p) as usize];
    println!(
        "embedded-text tokens: median {} p90 {} p99 {} max {}   agent-registry suffix = {:.1}% of all embedded tokens",
        q(0.5),
        q(0.9),
        q(0.99),
        lens.last().copied().unwrap_or(0),
        pct(suffix_total, all_total)
    );
    println!(
        "  type                  n  >256  >512  invisible-tokens  (% of type)  tags/concepts cut"
    );
    for (kind, a) in &by_type {
        println!(
            "  {kind:<20} {:>4} {:>5} {:>5} {:>10} {:>10.1}% {:>12}",
            a.n,
            a.over256,
            a.over512,
            a.lost,
            pct(a.lost, a.total),
            a.meta_lost
        );
    }
    let long: Vec<&str> = ["Summary", "Checkpoint", "Handoff"].to_vec();
    let (mut n, mut cut, mut lost, mut tot) = (0, 0, 0, 0);
    for kind in long {
        if let Some(a) = by_type.get(kind) {
            n += a.n;
            cut += a.over512;
            lost += a.lost;
            tot += a.total;
        }
    }
    println!(
        "  Summary+Checkpoint+Handoff: {n} thoughts, {cut} truncated at 512, {lost} of {tot} tokens ({:.1}%) semantically invisible",
        pct(lost, tot)
    );
}

// ------------------------------------------------------------------ probes

fn build_probes(
    chain: &MentisDb,
    args: &Args,
    vectors: Option<&HashMap<usize, Vec<f32>>>,
    tokenizer: Option<&tokenizers::Tokenizer>,
) -> Vec<Probe> {
    let thoughts = chain.thoughts();
    let n = thoughts.len();
    let index = LexicalIndex::build_with_registry(thoughts, chain.agent_registry());
    let invalid = chain.invalidated_thought_ids();
    let mut rng = Rng::new(args.seed);
    let mut order: Vec<usize> = (0..n)
        .filter(|&i| !invalid.contains(&thoughts[i].id))
        .filter(|&i| token_set(&thoughts[i].content).len() >= 4)
        .collect();
    rng.shuffle(&mut order);
    let targets: Vec<usize> = order.into_iter().take(args.sample).collect();
    let mut probes = Vec::new();

    for &t in &targets {
        let content = &thoughts[t].content;
        let all = words(content);
        let kept: Vec<&str> = all.iter().copied().filter(|_| !rng.chance(0.5)).collect();
        if !kept.is_empty() {
            probes.push(Probe {
                kind: "drop50",
                target: t,
                exclude: None,
                text: spread(&kept, args.query_tokens),
            });
        }
        let mut scored: Vec<(f64, usize, &str)> = all
            .iter()
            .enumerate()
            .filter_map(|(pos, w)| {
                let term = normalize_lexical_tokens(w, false).into_iter().next()?;
                let df = index.document_frequency(&term);
                (df > 0 && w.chars().count() > 2).then_some((df as f64, pos, *w))
            })
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.dedup_by(|a, b| a.2.eq_ignore_ascii_case(b.2));
        let mut rare: Vec<(usize, &str)> =
            scored.iter().take(6).map(|(_, p, w)| (*p, *w)).collect();
        rare.sort();
        if !rare.is_empty() {
            probes.push(Probe {
                kind: "rare",
                target: t,
                exclude: None,
                text: rare.iter().map(|(_, w)| *w).collect::<Vec<_>>().join(" "),
            });
        }
    }

    // `ref`: the opening of a later thought that refs the target.
    let mut seen = HashSet::new();
    let mut ref_probes = 0usize;
    for t in thoughts {
        for &r in &t.refs {
            let r = r as usize;
            if r < n
                && (t.index as usize) > r
                && ref_probes < args.sample
                && seen.insert(r)
                && !invalid.contains(&thoughts[r].id)
            {
                let w = words(&t.content);
                if w.len() >= 4 {
                    ref_probes += 1;
                    probes.push(Probe {
                        kind: "ref",
                        target: r,
                        exclude: Some(t.index as usize),
                        text: w
                            .into_iter()
                            .take(args.query_tokens)
                            .collect::<Vec<_>>()
                            .join(" "),
                    });
                }
            }
        }
    }
    if let Some(vectors) = vectors.filter(|v| !v.is_empty()) {
        add_semantic_probes(chain, &targets, vectors, args, &mut probes);
    }
    if let (true, Some(tokenizer)) = (args.tail, tokenizer) {
        add_tail_probes(chain, tokenizer, args, &mut probes);
    }
    let count = |k: &str| probes.iter().filter(|p| p.kind == k).count();
    println!(
        "\n(probes: drop50={} rare={} ref={} sem={} tail={}; targets exclude invalidated and thoughts with <4 distinct terms)",
        count("drop50"),
        count("rare"),
        count("ref"),
        count("sem"),
        count("tail")
    );
    probes
}

/// `tail` probes: for every live thought whose embedded text overflows the
/// 512-token window, the query is a handful of words taken from the part the
/// embedding model never sees. Only the lexical channel can answer these.
fn add_tail_probes(
    chain: &MentisDb,
    tokenizer: &tokenizers::Tokenizer,
    args: &Args,
    probes: &mut Vec<Probe>,
) {
    let invalid = chain.invalidated_thought_ids();
    let mut rng = Rng::new(args.seed ^ 0x7A11);
    let mut added = 0usize;
    for t in chain.thoughts() {
        if added >= args.sample || invalid.contains(&t.id) {
            continue;
        }
        let text = chain.embedding_text(t);
        let Ok(encoding) = tokenizer.encode(text.as_str(), true) else {
            continue;
        };
        let Some(&(start, _)) = encoding.get_offsets().get(EMBED_WINDOW - 1) else {
            continue;
        };
        let Some(tail) = text.get(start..) else {
            continue;
        };
        let kept: Vec<&str> = words(tail)
            .into_iter()
            .filter(|_| !rng.chance(0.5))
            .collect();
        if kept.len() >= 4 {
            added += 1;
            probes.push(Probe {
                kind: "tail",
                target: t.index as usize,
                exclude: None,
                text: spread(&kept, args.query_tokens),
            });
        }
    }
}

/// `sem` probes: for each target, the query is the opening of its most similar
/// thought *by embedding* among those that share few words with it (Jaccard
/// below 0.2, cosine at least 0.5). That is "same topic, different words", the
/// situation where only the vector channel can help.
fn add_semantic_probes(
    chain: &MentisDb,
    targets: &[usize],
    vectors: &HashMap<usize, Vec<f32>>,
    args: &Args,
    probes: &mut Vec<Probe>,
) {
    let thoughts = chain.thoughts();
    let invalid = chain.invalidated_thought_ids();
    let sets: HashMap<usize, HashSet<String>> = vectors
        .keys()
        .map(|&i| (i, token_set(&thoughts[i].content)))
        .collect();
    for &t in targets {
        let (Some(tv), Some(ts)) = (vectors.get(&t), sets.get(&t)) else {
            continue;
        };
        let best = vectors
            .iter()
            .filter(|(&b, _)| b != t && !invalid.contains(&thoughts[b].id))
            .filter(|(&b, _)| jaccard(ts, &sets[&b]) < 0.2)
            .map(|(&b, bv)| (b, cosine(tv, bv)))
            .filter(|(_, c)| *c >= 0.5)
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
        if let Some((b, _)) = best {
            let w = words(&thoughts[b].content);
            if w.len() >= 4 {
                probes.push(Probe {
                    kind: "sem",
                    target: t,
                    exclude: Some(b),
                    text: spread(&w, args.query_tokens),
                });
            }
        }
    }
}

/// Evenly pick at most `cap` words, preserving order.
fn spread(items: &[&str], cap: usize) -> String {
    if items.len() <= cap {
        return items.join(" ");
    }
    (0..cap)
        .map(|i| items[i * items.len() / cap])
        .collect::<Vec<_>>()
        .join(" ")
}

// ------------------------------------------------------------------ d

fn section_gating(chain: &MentisDb, probes: &[Probe]) {
    println!("\n## d. document-frequency gating (global DF, content/tags/concepts cutoff)");
    let thoughts = chain.thoughts();
    let n = thoughts.len();
    let cutoffs = Bm25DfCutoffs::default();
    let index = LexicalIndex::build_with_registry(thoughts, chain.agent_registry());
    println!(
        "doc_count {n}; gating {}; cutoff content={:.2} tags={:.2} concepts={:.2} agent_id={:.2} registry={:.2}",
        if n < 20 { "OFF (<20 docs)" } else { "on" },
        cutoffs.content,
        cutoffs.tags,
        cutoffs.concepts,
        cutoffs.agent_id,
        cutoffs.agent_registry
    );
    let gated = |term: &str| -> bool {
        n >= 20 && index.document_frequency(term) as f32 / n as f32 > cutoffs.content
    };

    let content_gated = |term: &str| -> bool {
        n >= 20
            && index.postings(term).is_some_and(|p| {
                p.iter()
                    .filter(|x| x.term_frequency(LexicalField::Content) > 0)
                    .count() as f32
                    / n as f32
                    > cutoffs.content
            })
    };

    // Vocabulary of content/tags/concepts, and its total term frequency.
    let mut tf: HashMap<String, usize> = HashMap::new();
    for t in thoughts {
        let mut all = normalize_lexical_tokens(&t.content, false);
        all.extend(
            t.tags
                .iter()
                .flat_map(|s| normalize_lexical_tokens(s, false)),
        );
        all.extend(
            t.concepts
                .iter()
                .flat_map(|s| normalize_lexical_tokens(s, false)),
        );
        for term in all {
            *tf.entry(term).or_default() += 1;
        }
    }
    let vocab = tf.len();
    let gated_vocab = tf.keys().filter(|t| gated(t)).count();
    let mut by_tf: Vec<(&String, &usize)> = tf.iter().collect();
    by_tf.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let gated_vocab_field = tf.keys().filter(|t| content_gated(t)).count();
    let top: Vec<&String> = by_tf.iter().take(200).map(|(t, _)| *t).collect();
    let top_gated = top.iter().filter(|t| gated(t)).count();
    let tokens_total: usize = tf.values().sum();
    let tokens_gated: usize = tf.iter().filter(|(t, _)| gated(t)).map(|(_, c)| *c).sum();
    println!(
        "vocabulary {vocab} terms; gated to zero: {gated_vocab} ({:.2}%); top-200 terms by tf gated: {top_gated} ({:.1}%); gated terms carry {:.1}% of all tokens",
        pct(gated_vocab, vocab),
        pct(top_gated, 200.min(top.len())),
        pct(tokens_gated, tokens_total)
    );
    println!(
        "  same gate with per-field DF (DfScope::PerField, content field): {gated_vocab_field} terms gated ({:.2}%)",
        pct(gated_vocab_field, vocab)
    );
    let mut gated_list: Vec<(String, usize)> = tf
        .keys()
        .filter(|t| gated(t))
        .map(|t| (t.clone(), index.document_frequency(t)))
        .collect();
    gated_list.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let non_trivial: Vec<String> = gated_list
        .iter()
        .filter(|(t, _)| t.chars().count() >= 4)
        .take(30)
        .map(|(t, df)| format!("{t}:{:.0}%", pct(*df, n)))
        .collect();
    println!(
        "  gated terms with >=4 chars (df%): {}",
        non_trivial.join(" ")
    );
    let near: Vec<String> = {
        let mut v: Vec<(String, usize)> = tf
            .keys()
            .map(|t| (t.clone(), index.document_frequency(t)))
            .filter(|(_, df)| {
                let r = *df as f32 / n as f32;
                n >= 20 && r > 0.15 && r <= cutoffs.content
            })
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter()
            .filter(|(t, _)| t.chars().count() >= 4)
            .take(20)
            .map(|(t, df)| format!("{t}:{:.0}%", pct(df, n)))
            .collect()
    };
    println!(
        "  just under the cliff (15-30% df, >=4 chars): {}",
        near.join(" ")
    );

    // Registry-induced DF: terms whose DF is inflated by agent registry/id text.
    let mut inflated = Vec::new();
    for (term, df) in &gated_list {
        let content_df = thoughts
            .iter()
            .filter(|t| {
                normalize_lexical_tokens(&t.content, false).contains(term)
                    || t.tags
                        .iter()
                        .any(|s| normalize_lexical_tokens(s, false).contains(term))
                    || t.concepts
                        .iter()
                        .any(|s| normalize_lexical_tokens(s, false).contains(term))
            })
            .count();
        if content_df as f32 / n as f32 <= cutoffs.content && term.chars().count() >= 3 {
            inflated.push(format!(
                "{term}: content-df {:.0}% -> global-df {:.0}%",
                pct(content_df, n),
                pct(*df, n)
            ));
        }
    }
    println!(
        "  terms gated ONLY because agent id/registry text inflates the global DF ({}): {}",
        inflated.len(),
        inflated
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ")
    );

    // Query-like exposure: terms of the synthetic queries.
    for kind in KINDS {
        let qs: Vec<&Probe> = probes.iter().filter(|p| p.kind == kind).collect();
        if qs.is_empty() {
            continue;
        }
        let (mut terms, mut gated_terms, mut all_gated, mut none_known) = (0, 0, 0, 0);
        for p in &qs {
            let uniq: HashSet<String> = normalize_lexical_tokens(&p.text, true)
                .into_iter()
                .collect();
            let live = uniq
                .iter()
                .filter(|t| index.document_frequency(t) > 0)
                .count();
            let g = uniq
                .iter()
                .filter(|t| index.document_frequency(t) > 0 && gated(t))
                .count();
            terms += uniq.len();
            gated_terms += g;
            all_gated += usize::from(live > 0 && g == live);
            none_known += usize::from(live == 0);
        }
        println!(
            "  {kind:<7} queries {:<4} terms gated {:.1}%   queries whose every known term is gated (lexically blind): {} ({:.1}%)   no known term: {none_known}",
            qs.len(),
            pct(gated_terms, terms),
            all_gated,
            pct(all_gated, qs.len())
        );
    }
}

// ------------------------------------------------------------------ e

fn run_probes(chain: &MentisDb, probes: &[Probe], rrf: bool, args: &Args) -> Vec<Outcome> {
    let depth = chain.thoughts().len().max(DEEP_LIMIT_FLOOR);
    probes
        .iter()
        .map(|probe| {
            let mut query = RankedSearchQuery::new()
                .with_text(probe.text.clone())
                .with_limit(depth);
            query.enable_reranking = rrf;
            query.df_scope = args.df_scope;
            if let Some(cutoff) = args.df_cutoff {
                query.df_cutoffs = Some(Bm25DfCutoffs {
                    content: cutoff,
                    tags: cutoff,
                    concepts: cutoff,
                    ..Bm25DfCutoffs::default()
                });
            }
            query.rerank_k = depth;
            let syns = thesaurus::expand_text(&probe.text);
            if !syns.is_empty() {
                query = query.with_synonyms(syns, thesaurus::AUTO_SYNONYM_WEIGHT);
            }
            let result = chain.query_ranked(&query);
            let hits: Vec<_> = result
                .hits
                .iter()
                .filter(|h| Some(h.thought.index as usize) != probe.exclude)
                .collect();
            let mut out = Outcome::default();
            if let Some(top) = hits.first() {
                out.top = Some(top.thought.index as usize);
                out.top_total = top.score.total;
                out.top_importance = top.score.importance;
                out.top_recency = top.score.recency;
            }
            match hits
                .iter()
                .position(|h| h.thought.index as usize == probe.target)
            {
                Some(pos) => {
                    let hit = hits[pos];
                    out.rank = Some(pos + 1);
                    out.target_lex = hit.score.lexical;
                    out.target_vec = hit.score.vector;
                    out.target_total = hit.score.total;
                }
                None => out.no_signal = true,
            }
            out
        })
        .collect()
}

#[derive(Default, Clone, Copy)]
struct Tally {
    n: usize,
    r1: usize,
    r10: usize,
    none: usize,
}

impl Tally {
    fn add(&mut self, o: &Outcome) {
        self.n += 1;
        self.r1 += usize::from(o.rank == Some(1));
        self.r10 += usize::from(o.rank.is_some_and(|r| r <= 10));
        self.none += usize::from(o.no_signal);
    }
    fn cell(&self) -> String {
        if self.n == 0 {
            return "      -      ".into();
        }
        format!(
            "{:>3.0}/{:>3.0} ({:>3})",
            pct(self.r1, self.n),
            pct(self.r10, self.n),
            self.n
        )
    }
}

/// A named retrieval configuration and its per-probe outcomes.
struct Mode<'a> {
    name: &'static str,
    outcomes: &'a [Outcome],
}

fn section_recall(chain: &MentisDb, probes: &[Probe], modes: &[Mode<'_>]) {
    println!("\n## e. self-recall by age decile (R@1/R@10 %, n); default query, limit 10");
    let n = chain.thoughts().len();
    for kind in KINDS {
        let idx: Vec<usize> = (0..probes.len())
            .filter(|&i| probes[i].kind == kind)
            .collect();
        if idx.is_empty() {
            continue;
        }
        println!("\n{kind}:");
        print!("  decile ");
        for m in modes {
            print!("  {:<18}", m.name);
        }
        println!();
        let mut tallies = vec![[Tally::default(); 10]; modes.len()];
        for &i in &idx {
            let d = (probes[i].target * 10 / n).min(9);
            for (m, t) in modes.iter().zip(tallies.iter_mut()) {
                t[d].add(&m.outcomes[i]);
            }
        }
        for d in 0..10 {
            print!("  d{d}     ");
            for t in &tallies {
                print!("  {:<18}", t[d].cell());
            }
            println!();
        }
        let fold = |t: &[Tally; 10], range: std::ops::Range<usize>| {
            t[range].iter().fold(Tally::default(), |mut a, b| {
                a.n += b.n;
                a.r1 += b.r1;
                a.r10 += b.r10;
                a.none += b.none;
                a
            })
        };
        for (label, range) in [("ALL ", 0..10), ("old ", 0..5), ("new ", 5..10)] {
            print!("  {label}   ");
            for t in &tallies {
                print!("  {:<18}", fold(t, range.clone()).cell());
            }
            println!();
        }
    }
    println!("\nmiss anatomy (all probe kinds; R@1/R@10 misses) and near-duplicate displacement:");
    let thoughts = chain.thoughts();
    let tokens: Vec<HashSet<String>> = thoughts.iter().map(|t| token_set(&t.content)).collect();
    for mode in modes {
        let outcomes = mode.outcomes;
        let name = mode.name;
        let (mut miss1, mut miss10, mut nosig, mut displaced, mut newer_top) = (0, 0, 0, 0, 0);
        let (mut tgt_total, mut top_total, mut top_imp, mut top_rec) = (0.0, 0.0, 0.0, 0.0);
        let mut ranked_miss = 0;
        let mut displacers: HashMap<usize, usize> = HashMap::new();
        for (probe, o) in probes.iter().zip(outcomes) {
            if o.rank == Some(1) {
                continue;
            }
            miss1 += 1;
            if let Some(top) = o.top {
                *displacers.entry(top).or_default() += 1;
                if top > probe.target {
                    newer_top += 1;
                    if jaccard(&tokens[top], &tokens[probe.target]) >= 0.5 {
                        displaced += 1;
                    }
                }
            }
            if !o.rank.is_some_and(|r| r <= 10) {
                miss10 += 1;
                if o.no_signal {
                    nosig += 1;
                } else {
                    ranked_miss += 1;
                    tgt_total += o.target_total;
                    top_total += o.top_total;
                    top_imp += o.top_importance;
                    top_rec += o.top_recency;
                }
            }
        }
        let avg = |x: f32| {
            if ranked_miss == 0 {
                0.0
            } else {
                x / ranked_miss as f32
            }
        };
        println!(
            "  {name:<14} probes {}  miss@1 {miss1}  miss@10 {miss10} (no signal {nosig}, outranked {ranked_miss})  \
             miss@1 where top hit is NEWER {newer_top}, of which near-duplicate (jaccard>=0.5) {displaced}",
            probes.len()
        );
        println!(
            "                 outranked misses: mean total target {:.2} vs top1 {:.2}; top1 importance term {:.2}, recency term {:.3}",
            avg(tgt_total),
            avg(top_total),
            avg(top_imp),
            avg(top_rec)
        );
        let mut displacers: Vec<(usize, usize)> = displacers.into_iter().collect();
        displacers.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        println!("                 thoughts that most often take rank 1 from the target:");
        for (idx, count) in displacers.iter().take(4) {
            let t = &thoughts[*idx];
            println!(
                "                   #{idx} x{count}  {:?}/{:?} imp={:.2} words={}  {}",
                t.thought_type,
                t.role,
                t.importance,
                t.content.split_whitespace().count(),
                snippet(&t.content, 60)
            );
        }
    }
}

/// Print concrete lexical-only rank-1 misses so a human can judge whether the
/// thought that won is a duplicate, a superset or genuinely unrelated.
fn show_misses(chain: &MentisDb, probes: &[Probe], lexical: &[Outcome], per_kind: usize) {
    if per_kind == 0 {
        return;
    }
    let thoughts = chain.thoughts();
    println!("\n## sample lexical-only rank-1 misses");
    for kind in KINDS {
        let mut shown = 0;
        for (probe, o) in probes.iter().zip(lexical) {
            if probe.kind != kind || o.rank == Some(1) || shown >= per_kind {
                continue;
            }
            shown += 1;
            let target = &thoughts[probe.target];
            println!(
                "  [{kind}] rank {:?}  query: {}\n     target #{} {:?}/{:?} imp={:.2} words={}: {}",
                o.rank,
                snippet(&probe.text, 90),
                probe.target,
                target.thought_type,
                target.role,
                target.importance,
                target.content.split_whitespace().count(),
                snippet(&target.content, 90)
            );
            if let Some(top) = o.top {
                let t = &thoughts[top];
                println!(
                    "     beat-by #{top} {:?}/{:?} imp={:.2} words={} total {:.1} vs {:.1}: {}",
                    t.thought_type,
                    t.role,
                    t.importance,
                    t.content.split_whitespace().count(),
                    o.top_total,
                    o.target_total,
                    snippet(&t.content, 90)
                );
            }
        }
    }
}

// ------------------------------------------------------------------ f

fn section_dilution(chain: &MentisDb, probes: &[Probe]) {
    println!("\n## f. embedding dilution (agent-registry suffix)");
    let provider = match FastEmbedProvider::try_new() {
        Ok(p) => p,
        Err(error) => {
            println!("fastembed unavailable: {error}");
            return;
        }
    };
    let thoughts = chain.thoughts();
    let embed = |texts: Vec<String>| -> Vec<Vec<f32>> {
        let inputs: Vec<EmbeddingInput> = texts
            .into_iter()
            .enumerate()
            .map(|(i, t)| EmbeddingInput::new(format!("d{i}"), t))
            .collect();
        provider
            .embed_batch(&inputs)
            .expect("embed")
            .into_iter()
            .map(|v| v.values)
            .collect()
    };
    let with: Vec<Vec<f32>> = embed(thoughts.iter().map(|t| chain.embedding_text(t)).collect());
    let without: Vec<Vec<f32>> = embed(thoughts.iter().map(bare_embedding_text).collect());
    let suffixed = thoughts
        .iter()
        .filter(|t| chain.embedding_text(t) != bare_embedding_text(t))
        .count();
    println!(
        "thoughts whose embedded text carries a registry suffix: {suffixed}/{}",
        thoughts.len()
    );

    // Mean cosine among unrelated same-agent pairs (content jaccard < 0.05).
    let tokens: Vec<HashSet<String>> = thoughts.iter().map(|t| token_set(&t.content)).collect();
    let mut by_agent: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in thoughts.iter().enumerate() {
        by_agent.entry(t.agent_id.as_str()).or_default().push(i);
    }
    let mut agents: Vec<(&&str, &Vec<usize>)> = by_agent.iter().collect();
    agents.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let (mut sum_with, mut sum_without, mut pairs) = (0.0f64, 0.0f64, 0usize);
    println!("  agent                          thoughts  unrelated-pair mean cosine: with suffix / bare / delta");
    for (rank, (agent, members)) in agents.iter().enumerate() {
        let (mut sw, mut so, mut np) = (0.0f64, 0.0f64, 0usize);
        let cap = members.len().min(120);
        for a in 0..cap {
            for b in (a + 1)..cap {
                let (i, j) = (members[a], members[b]);
                if jaccard(&tokens[i], &tokens[j]) < 0.05 {
                    sw += f64::from(cosine(&with[i], &with[j]));
                    so += f64::from(cosine(&without[i], &without[j]));
                    np += 1;
                }
            }
        }
        sum_with += sw;
        sum_without += so;
        pairs += np;
        if rank < 6 && np > 0 {
            println!(
                "  {:<30} {:>8}  {:.4} / {:.4} / {:+.4}  (pairs {np})",
                agent,
                members.len(),
                sw / np as f64,
                so / np as f64,
                (sw - so) / np as f64
            );
        }
    }
    if pairs > 0 {
        println!(
            "  ALL same-agent unrelated pairs ({pairs}): with {:.4}  bare {:.4}  delta {:+.4}",
            sum_with / pairs as f64,
            sum_without / pairs as f64,
            (sum_with - sum_without) / pairs as f64
        );
    }

    // Query -> target cosine and vector-only recall, both variants.
    for kind in KINDS {
        let qs: Vec<&Probe> = probes.iter().filter(|p| p.kind == kind).collect();
        if qs.is_empty() {
            continue;
        }
        let qv = embed(qs.iter().map(|p| p.text.clone()).collect());
        let mut line = format!("  {kind:<7} n={:<4}", qs.len());
        for (label, corpus) in [("with", &with), ("bare", &without)] {
            let (mut cos, mut r1, mut r10, mut r256, mut mrr) = (0.0f64, 0, 0, 0, 0.0f64);
            for (p, q) in qs.iter().zip(&qv) {
                let target = cosine(q, &corpus[p.target]);
                cos += f64::from(target);
                let rank = 1 + corpus
                    .iter()
                    .enumerate()
                    .filter(|(i, v)| {
                        *i != p.target && Some(*i) != p.exclude && cosine(q, v) > target
                    })
                    .count();
                r1 += usize::from(rank == 1);
                r10 += usize::from(rank <= 10);
                r256 += usize::from(rank <= 256);
                mrr += 1.0 / rank as f64;
            }
            let m = qs.len();
            line.push_str(&format!(
                "  [{label}] cos {:.3} R@1 {:.0}% R@10 {:.0}% R@256 {:.0}% MRR {:.3}",
                cos / m as f64,
                pct(r1, m),
                pct(r10, m),
                pct(r256, m),
                mrr / m as f64
            ));
        }
        println!("{line}");
    }
}
