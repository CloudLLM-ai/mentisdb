//! Multi-probe recall: run several sub-questions and see where they converge.
//!
//! A multi-hop question ("which library did the agent who fixed the WAL bug
//! recommend for retries?") rarely matches any single memory. The memories
//! that answer it each match one *clue*. Decomposing the question into short
//! clue-probing sub-questions, searching each one, and looking for the
//! memories and entities that several probes reach is the grounding and
//! entity-identification half of the structured recall protocol measured in
//! *MeMo: Memory as a Model* (arXiv 2605.15156): there, a structured protocol
//! beat a single query by more than 20 points and beat 50 unstructured
//! follow-up turns, on multi-hop benchmarks.
//!
//! This module is the deterministic, storage-free part:
//!
//! - [`probe_labels`] extracts the labels (entity type, concepts, tags and
//!   identifier-like terms from the content) that can name an entity.
//! - [`converge_probes`] fuses the per-probe hit lists into
//!   [`ConvergentHit`]s (memories ordered by how many probes reached them) and
//!   [`EntityCandidate`]s (labels shared by memories from different probes).
//!
//! [`crate::MentisDb::query_probes`] runs the probes against a chain and calls
//! both. Nothing here calls an LLM or writes to a chain; the calling agent
//! writes the sub-questions and decides what to ask next.

use crate::search::query_intent::QueryIntent;
use crate::{MentisDb, RankedSearchQuery, RankedSearchResult};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;

/// Most probes one call accepts. A question that needs more is better split
/// into a second round once the first has identified an entity.
pub const MAX_PROBES: usize = 8;

/// Hits kept per probe when the caller does not say.
pub const DEFAULT_PROBE_LIMIT: usize = 5;

/// Most hits kept per probe.
pub const MAX_PROBE_LIMIT: usize = 25;

/// Longest probe text accepted, in bytes. A probe is one short clue, not a
/// document.
pub const MAX_PROBE_TEXT_BYTES: usize = 1_000;

/// Reciprocal-rank-fusion constant used to score convergence.
const RRF_K: f32 = 60.0;

/// Where an [`EntityCandidate`] label came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeLabelKind {
    /// The thought's `entity_type`.
    EntityType,
    /// One of the thought's `concepts`.
    Concept,
    /// One of the thought's `tags` (scope tags such as `scope:user` excluded).
    Tag,
    /// An identifier-like word in the content: a code identifier
    /// (`snake_case`, `CamelCase`, `path/to.rs`, `a::b`) or a capitalized
    /// name that does not start a sentence.
    Term,
}

/// One label that may name an entity, as extracted by [`probe_labels`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeLabel {
    /// Where the label came from.
    pub kind: ProbeLabelKind,
    /// The label as first written.
    pub label: String,
}

impl ProbeLabel {
    /// Case-insensitive identity of the label: two labels with the same key
    /// are one entity candidate whatever their kind or capitalization.
    fn key(&self) -> String {
        self.label.to_lowercase()
    }
}

/// One probe hit as [`converge_probes`] sees it: the thought id and the
/// labels extracted from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeHitInput {
    /// Id of the thought the probe reached.
    pub id: Uuid,
    /// Labels from [`probe_labels`].
    pub labels: Vec<ProbeLabel>,
}

/// Options for [`converge_probes`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeConvergenceOptions {
    /// Most entity candidates returned. Default 10.
    pub max_entities: usize,
    /// Most supporting thought ids listed per entity candidate. Default 5.
    pub max_support: usize,
    /// A label carried by at least this share of all distinct hits does not
    /// tell them apart (a project-wide tag, say) and is not a candidate.
    /// Default 0.8.
    pub common_label_ratio: f32,
    /// The common-label rule applies only from this many distinct hits on;
    /// below it every label is kept. Default 5.
    pub common_label_min_hits: usize,
}

impl Default for ProbeConvergenceOptions {
    fn default() -> Self {
        Self {
            max_entities: 10,
            max_support: 5,
            common_label_ratio: 0.8,
            common_label_min_hits: 5,
        }
    }
}

/// A thought reached by one or more probes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConvergentHit {
    /// Thought id.
    pub id: Uuid,
    /// Indexes (into the probe list) of the probes that reached it, ascending.
    pub probes: Vec<usize>,
    /// Best (lowest, zero-based) rank it had in any probe.
    pub best_rank: usize,
    /// Reciprocal-rank-fusion score over the probes that reached it.
    pub score: f32,
}

/// A label shared by thoughts from different probes: a candidate for the
/// entity the question is about.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntityCandidate {
    /// The label as first seen.
    pub label: String,
    /// Where it was first seen.
    pub kind: ProbeLabelKind,
    /// Indexes of the probes whose hits carry it, ascending.
    pub probes: Vec<usize>,
    /// Up to [`ProbeConvergenceOptions::max_support`] supporting thought
    /// ids, best fused rank first.
    pub thought_ids: Vec<Uuid>,
    /// Sum over its probes of the best reciprocal rank it reached there.
    pub score: f32,
}

/// What [`converge_probes`] found.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ProbeConvergence {
    /// Every distinct hit, most probes first, then by fused score.
    pub hits: Vec<ConvergentHit>,
    /// Entity candidates, most probes first, then by score.
    pub entities: Vec<EntityCandidate>,
}

/// Extract the labels of one thought that can name an entity.
///
/// Returns the `entity_type`, the `concepts`, the `tags` other than scope tags
/// (`scope:...`), and identifier-like [`ProbeLabelKind::Term`]s from `content`.
/// Labels are deduplicated case-insensitively, first occurrence wins, in that
/// order.
///
/// # Example
///
/// ```
/// use mentisdb::search::probe::{probe_labels, ProbeLabelKind};
///
/// let labels = probe_labels(
///     "Fixed the WAL rebase in sidecar.rs; Orion reviewed compact_to_path.",
///     &["scope:user".to_string(), "wal".to_string()],
///     &["vector sidecar".to_string()],
///     None,
/// );
/// let names: Vec<_> = labels.iter().map(|l| l.label.as_str()).collect();
/// assert_eq!(names, ["vector sidecar", "wal", "sidecar.rs", "Orion", "compact_to_path"]);
/// assert_eq!(labels[3].kind, ProbeLabelKind::Term);
/// ```
pub fn probe_labels(
    content: &str,
    tags: &[String],
    concepts: &[String],
    entity_type: Option<&str>,
) -> Vec<ProbeLabel> {
    let mut seen = BTreeSet::new();
    let mut labels = Vec::new();
    let mut push = |kind: ProbeLabelKind, label: &str| {
        let label = label.trim();
        if label.is_empty() || !seen.insert(label.to_lowercase()) {
            return;
        }
        labels.push(ProbeLabel {
            kind,
            label: label.to_string(),
        });
    };
    if let Some(entity_type) = entity_type {
        push(ProbeLabelKind::EntityType, entity_type);
    }
    for concept in concepts {
        push(ProbeLabelKind::Concept, concept);
    }
    for tag in tags {
        if !tag.trim_start().to_ascii_lowercase().starts_with("scope:") {
            push(ProbeLabelKind::Tag, tag);
        }
    }
    for term in content_terms(content) {
        push(ProbeLabelKind::Term, term);
    }
    labels
}

/// Identifier-like words of `content`, in order of appearance.
fn content_terms(content: &str) -> Vec<&str> {
    let mut terms = Vec::new();
    // The first word of the text, and every word after sentence or list
    // punctuation, is capitalized by grammar, not because it is a name.
    let mut sentence_start = true;
    for raw in content.split_whitespace() {
        let word = raw.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'));
        let starts_sentence = sentence_start;
        sentence_start =
            raw.ends_with(['.', '!', '?', ':']) || raw.chars().all(|c| !c.is_alphanumeric());
        if word.chars().count() < 3 || !word.chars().any(char::is_alphabetic) {
            continue;
        }
        if is_code_identifier(word) || (!starts_sentence && is_name(word)) {
            terms.push(word);
        }
    }
    terms
}

/// `snake_case`, `a::b`, `path/to`, `file.rs` or `CamelCase`.
fn is_code_identifier(word: &str) -> bool {
    if word.contains('_') || word.contains("::") || word.contains('/') {
        return true;
    }
    if let Some((stem, ext)) = word.rsplit_once('.') {
        // `file.rs`, `server.toml`: a short alphabetic extension after a stem.
        // A sentence-final period was trimmed, so a dot here is inside the word.
        return !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_lowercase());
    }
    // An uppercase letter after a lowercase one: `MentisDb`, `HippoRAG`.
    let chars: Vec<char> = word.chars().collect();
    chars
        .windows(2)
        .any(|pair| pair[0].is_lowercase() && pair[1].is_uppercase())
}

/// A capitalized word that is not a common sentence word.
fn is_name(word: &str) -> bool {
    const COMMON: [&str; 24] = [
        "the", "this", "that", "these", "those", "then", "there", "they", "when", "what", "which",
        "while", "with", "from", "after", "before", "also", "and", "but", "for", "not", "use",
        "see", "note",
    ];
    word.chars().next().is_some_and(char::is_uppercase)
        && !COMMON.contains(&word.to_lowercase().as_str())
}

/// Fuse per-probe hit lists into convergent hits and entity candidates.
///
/// `probes[i]` is probe `i`'s hits, best first. A thought's score is
/// `Σ 1 / (60 + rank + 1)` over the probes that reached it; hits are ordered by
/// how many probes reached them, then by that score. A label becomes an
/// [`EntityCandidate`] when thoughts from at least two probes carry it (with a
/// single probe: at least two thoughts), unless at least
/// [`ProbeConvergenceOptions::common_label_ratio`] of all distinct hits carry
/// it. Ties break on rank, then id or label, so the output is deterministic.
///
/// # Example
///
/// ```
/// use mentisdb::search::probe::{converge_probes, ProbeConvergenceOptions, ProbeHitInput, ProbeLabel, ProbeLabelKind};
/// use uuid::Uuid;
///
/// let label = |l: &str| ProbeLabel { kind: ProbeLabelKind::Term, label: l.to_string() };
/// let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
/// let probes = vec![
///     vec![ProbeHitInput { id: a, labels: vec![label("Orion")] },
///          ProbeHitInput { id: b, labels: vec![label("Astro")] }],
///     vec![ProbeHitInput { id: c, labels: vec![label("orion")] },
///          ProbeHitInput { id: b, labels: vec![label("Astro")] }],
/// ];
/// let found = converge_probes(&probes, ProbeConvergenceOptions::default());
/// assert_eq!(found.hits[0].id, b); // reached by both probes
/// assert_eq!(found.hits[0].probes, vec![0, 1]);
/// // Both labels reach both probes; "Orion" ranks first in each of them.
/// let names: Vec<_> = found.entities.iter().map(|e| e.label.as_str()).collect();
/// assert_eq!(names, ["Orion", "Astro"]);
/// ```
pub fn converge_probes(
    probes: &[Vec<ProbeHitInput>],
    options: ProbeConvergenceOptions,
) -> ProbeConvergence {
    struct HitAcc {
        probes: BTreeSet<usize>,
        best_rank: usize,
        score: f32,
        first_seen: usize,
    }
    struct LabelAcc {
        label: String,
        kind: ProbeLabelKind,
        // Best reciprocal rank per probe.
        per_probe: BTreeMap<usize, f32>,
        thoughts: BTreeSet<Uuid>,
        first_seen: usize,
    }

    let rrf = |rank: usize| 1.0 / (RRF_K + rank as f32 + 1.0);
    let mut hits: HashMap<Uuid, HitAcc> = HashMap::new();
    let mut labels: HashMap<String, LabelAcc> = HashMap::new();
    let mut order = 0usize;

    for (probe, probe_hits) in probes.iter().enumerate() {
        let mut seen_in_probe = BTreeSet::new();
        for (rank, hit) in probe_hits.iter().enumerate() {
            if !seen_in_probe.insert(hit.id) {
                continue;
            }
            order += 1;
            let acc = hits.entry(hit.id).or_insert(HitAcc {
                probes: BTreeSet::new(),
                best_rank: rank,
                score: 0.0,
                first_seen: order,
            });
            acc.probes.insert(probe);
            acc.best_rank = acc.best_rank.min(rank);
            acc.score += rrf(rank);

            for label in &hit.labels {
                let acc = labels.entry(label.key()).or_insert_with(|| LabelAcc {
                    label: label.label.clone(),
                    kind: label.kind,
                    per_probe: BTreeMap::new(),
                    thoughts: BTreeSet::new(),
                    first_seen: order,
                });
                let best = acc.per_probe.entry(probe).or_insert(0.0);
                *best = best.max(rrf(rank));
                acc.thoughts.insert(hit.id);
            }
        }
    }

    let mut ranked_hits: Vec<(Uuid, HitAcc)> = hits.into_iter().collect();
    ranked_hits.sort_by(|(a_id, a), (b_id, b)| {
        b.probes
            .len()
            .cmp(&a.probes.len())
            .then(b.score.total_cmp(&a.score))
            .then(a.best_rank.cmp(&b.best_rank))
            .then(a.first_seen.cmp(&b.first_seen))
            .then(a_id.cmp(b_id))
    });
    let hit_position: HashMap<Uuid, usize> = ranked_hits
        .iter()
        .enumerate()
        .map(|(position, (id, _))| (*id, position))
        .collect();

    let distinct_hits = ranked_hits.len();
    let multi_probe = probes.iter().filter(|hits| !hits.is_empty()).count() >= 2;
    let mut entities: Vec<(usize, EntityCandidate)> = labels
        .into_values()
        .filter(|acc| {
            let converges = if multi_probe {
                acc.per_probe.len() >= 2
            } else {
                acc.thoughts.len() >= 2
            };
            let common = distinct_hits >= options.common_label_min_hits
                && acc.thoughts.len() as f32 >= options.common_label_ratio * distinct_hits as f32;
            converges && !common
        })
        .map(|acc| {
            let mut thought_ids: Vec<Uuid> = acc.thoughts.into_iter().collect();
            thought_ids.sort_by_key(|id| hit_position.get(id).copied().unwrap_or(usize::MAX));
            thought_ids.truncate(options.max_support);
            (
                acc.first_seen,
                EntityCandidate {
                    label: acc.label,
                    kind: acc.kind,
                    probes: acc.per_probe.keys().copied().collect(),
                    score: acc.per_probe.values().sum(),
                    thought_ids,
                },
            )
        })
        .collect();
    entities.sort_by(|(a_seen, a), (b_seen, b)| {
        b.probes
            .len()
            .cmp(&a.probes.len())
            .then(b.score.total_cmp(&a.score))
            .then(a_seen.cmp(b_seen))
            .then(a.label.cmp(&b.label))
    });
    entities.truncate(options.max_entities);

    ProbeConvergence {
        hits: ranked_hits
            .into_iter()
            .map(|(id, acc)| ConvergentHit {
                id,
                probes: acc.probes.into_iter().collect(),
                best_rank: acc.best_rank,
                score: acc.score,
            })
            .collect(),
        entities: entities.into_iter().map(|(_, entity)| entity).collect(),
    }
}

/// One probe run by [`MentisDb::query_probes`].
#[derive(Debug, Clone)]
pub struct ProbeRun<'a> {
    /// The probe text.
    pub text: String,
    /// Its deterministic intent signals.
    pub intent: QueryIntent,
    /// Its ranked hits.
    pub result: RankedSearchResult<'a>,
}

/// What [`MentisDb::query_probes`] returns.
#[derive(Debug, Clone)]
pub struct ProbeSearchResult<'a> {
    /// Each probe and its hits, in request order.
    pub probes: Vec<ProbeRun<'a>>,
    /// Convergent hits and entity candidates across the probes.
    pub convergence: ProbeConvergence,
}

impl MentisDb {
    /// Run each of `probes` as a ranked search and fuse the results.
    ///
    /// Every probe uses `base` (filters, graph, knobs, limit) with its own
    /// text. Use this for a multi-hop question: split it into short
    /// clue-probing sub-questions, run them together, then follow up on the
    /// best [`EntityCandidate`] with a ranked search filtered to it (its
    /// concept, tag or name). Empty probes are skipped.
    ///
    /// # Example
    ///
    /// ```
    /// use mentisdb::{MentisDb, RankedSearchQuery, ThoughtType};
    /// use mentisdb::search::probe::ProbeConvergenceOptions;
    ///
    /// let dir = std::env::temp_dir().join(format!("probe-doc-{}", uuid::Uuid::new_v4()));
    /// let mut chain = MentisDb::open_with_key(&dir, "probe-doc").unwrap();
    /// chain.append("astro", ThoughtType::Finding, "The WAL rebase bug was fixed by Orion.").unwrap();
    /// chain.append("astro", ThoughtType::Decision, "Backoff retries were recommended by Orion.").unwrap();
    /// let found = chain.query_probes(
    ///     &RankedSearchQuery::new().with_limit(5),
    ///     &["WAL rebase bug".to_string(), "backoff retries".to_string()],
    ///     ProbeConvergenceOptions::default(),
    /// );
    /// assert_eq!(found.probes.len(), 2);
    /// assert_eq!(found.convergence.entities[0].label, "Orion");
    /// # let _ = std::fs::remove_dir_all(&dir);
    /// ```
    pub fn query_probes(
        &self,
        base: &RankedSearchQuery,
        probes: &[String],
        options: ProbeConvergenceOptions,
    ) -> ProbeSearchResult<'_> {
        let runs: Vec<ProbeRun<'_>> = probes
            .iter()
            .map(|text| text.trim())
            .filter(|text| !text.is_empty())
            .map(|text| ProbeRun {
                text: text.to_string(),
                intent: QueryIntent::classify(text, &[]),
                result: self.query_ranked(&base.clone().with_text(text)),
            })
            .collect();
        let inputs: Vec<Vec<ProbeHitInput>> = runs
            .iter()
            .map(|run| {
                run.result
                    .hits
                    .iter()
                    .map(|hit| ProbeHitInput {
                        id: hit.thought.id,
                        labels: probe_labels(
                            &hit.thought.content,
                            &hit.thought.tags,
                            &hit.thought.concepts,
                            hit.thought.entity_type.as_deref(),
                        ),
                    })
                    .collect()
            })
            .collect();
        ProbeSearchResult {
            probes: runs,
            convergence: converge_probes(&inputs, options),
        }
    }
}
