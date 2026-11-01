//! Version lineage: which memory is the *current* version of a fact.
//!
//! MentisDB is append-only, so a changed policy is a **new thought** that points
//! back at the old one. Retrieval must therefore answer one question reliably:
//! "is the thought I am about to hand the agent still the current version?"
//! This module answers it in three layers.
//!
//! # 1. Strong links hide the old version and redirect to the new one
//!
//! A relation of kind [`Supersedes`](crate::ThoughtRelationKind::Supersedes),
//! [`Corrects`](crate::ThoughtRelationKind::Corrects) or
//! [`Invalidates`](crate::ThoughtRelationKind::Invalidates) marks its target as
//! superseded. Default ranked search no longer silently drops such a target.
//! When the superseded thought would have ranked, its **live successor** (the
//! newest unsuperseded end of the chain of supersessions) takes its place and
//! carries [`VersionInfo::successor_of`], so a successor whose wording does not
//! match the query still reaches the agent.
//!
//! # 2. Weak links contest without hiding
//!
//! Measured on the real chains of one installation, **none** of the 14
//! `Correction`/`AssumptionInvalidated` thoughts and none of the ~90 `Decision`
//! thoughts carried a typed invalidating relation; 7 of the 14 corrections
//! carried no link at all and the others used plain `refs` (stored as
//! `References`). Agents link, they just do not type the link. But a plain
//! reference is also used for background ("consolidated identities A, B, C into
//! D" references six memories that stay valid), so treating every reference
//! from a correction as an invalidation would hide good memories.
//!
//! The decision is therefore asymmetric. A weak link from a thought that says
//! it replaces something (see [`contest_kind`]) records a
//! [`SuccessorKind::Contests`] edge. The target stays visible, is reported with
//! [`VersionStatus::Contested`], names the contesting thought, and that
//! thought is promoted right above it in ranked results. The agent sees both
//! and the warning, and nothing is lost if the link was only background.
//! Edges are derived from the stored chain at open time, so existing chains
//! benefit retroactively with no migration.
//!
//! # 3. Append-time guidance
//!
//! [`MentisDb::append_advice`] inspects a freshly appended thought and returns
//! [`AppendAdvice`] when it looks like a replacement but carries no strong
//! relation, naming candidate target ids so the agent can append the typed
//! link next time.
//!
//! # Example
//!
//! ```
//! use mentisdb::{MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtType};
//! use mentisdb::{ThoughtRelation, ThoughtRelationKind, VersionStatus};
//!
//! # fn main() -> std::io::Result<()> {
//! let dir = tempfile::tempdir()?;
//! let mut chain = MentisDb::open_with_key_and_storage_kind(dir.path(), "demo", StorageAdapterKind::Binary)?;
//! let old = chain
//!     .append_thought("a", ThoughtInput::new(ThoughtType::Decision, "Release policy: deploy on Fridays."))?
//!     .clone();
//! // The new policy shares no words with the old one.
//! chain.append_thought(
//!     "a",
//!     ThoughtInput::new(ThoughtType::Decision, "Ship only early in the week, never before a weekend.")
//!         .with_relations(vec![ThoughtRelation {
//!             kind: ThoughtRelationKind::Supersedes,
//!             target_id: old.id,
//!             chain_key: None,
//!             valid_at: None,
//!             invalid_at: None,
//!         }]),
//! )?;
//! let hits = chain.query_ranked(&RankedSearchQuery::new().with_text("release policy Fridays"));
//! assert_eq!(hits.hits[0].thought.content, "Ship only early in the week, never before a weekend.");
//! assert_eq!(hits.hits[0].version.successor_of, vec![old.id]);
//! assert_eq!(chain.version_status(old.id), VersionStatus::Superseded);
//! # Ok(())
//! # }
//! ```

use crate::audience::Audience;
use crate::{
    is_invalidating_relation_kind, MentisDb, RankedSearchHit, RankedSearchQuery,
    RankedSearchResult, Thought, ThoughtRelationKind, ThoughtType,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Longest chain of supersessions followed when looking for the live successor.
const MAX_LINEAGE_DEPTH: usize = 64;
/// How many characters of the new thought are used as the append-time
/// similarity query.
const ADVICE_QUERY_CHARS: usize = 400;
/// How many suggested targets an [`AppendAdvice`] lists.
const ADVICE_MAX_TARGETS: usize = 3;
/// Score margin by which a promoted contesting thought outranks the thought it
/// contests, so re-sorting by score cannot put the older version first.
const CONTEST_SCORE_BUMP: f32 = 1e-3;
/// Minimum distinct matched query terms for a similarity-derived suggested
/// target. A term count is stable on small chains where absolute BM25 scores
/// are tiny.
const ADVICE_MIN_MATCHED_TERMS: usize = 2;

/// How a newer thought relates to an older one.
///
/// The first three kinds are **strong**: the older thought is hidden from
/// default retrieval (see [`SuccessorKind::is_strong`]). The last two are
/// **weak**: the older thought stays visible but is flagged as contested.
///
/// # Examples
///
/// ```
/// use mentisdb::SuccessorKind;
///
/// assert!(SuccessorKind::Supersedes.is_strong());
/// assert!(!SuccessorKind::Contests.is_strong());
/// assert_eq!(SuccessorKind::Contests.to_string(), "contests");
/// assert_eq!("corrects".parse::<SuccessorKind>().unwrap(), SuccessorKind::Corrects);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessorKind {
    /// A typed `Supersedes` relation: the newer thought replaces the older.
    Supersedes,
    /// A typed `Corrects` relation: the older thought was wrong.
    Corrects,
    /// A typed `Invalidates` relation: the older thought no longer holds.
    Invalidates,
    /// A `Contradicts` relation from any thought.
    Contradicts,
    /// An untyped reference from a thought that presents itself as a
    /// replacement (a `Correction`, an `AssumptionInvalidated`, or a
    /// `Decision` pointing at an earlier decision-like thought).
    Contests,
}

impl SuccessorKind {
    /// Whether this kind hides the older thought from default retrieval.
    pub const fn is_strong(self) -> bool {
        matches!(self, Self::Supersedes | Self::Corrects | Self::Invalidates)
    }

    /// Stable lower-case wire label (`supersedes`, `corrects`, ...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supersedes => "supersedes",
            Self::Corrects => "corrects",
            Self::Invalidates => "invalidates",
            Self::Contradicts => "contradicts",
            Self::Contests => "contests",
        }
    }

    const fn strength(self) -> u8 {
        match self {
            Self::Supersedes | Self::Corrects | Self::Invalidates => 3,
            Self::Contradicts => 2,
            Self::Contests => 1,
        }
    }
}

impl std::fmt::Display for SuccessorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when a string is not a [`SuccessorKind`] label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSuccessorKindError(pub String);

impl std::fmt::Display for ParseSuccessorKindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown successor kind `{}`", self.0)
    }
}

impl std::error::Error for ParseSuccessorKindError {}

impl std::str::FromStr for SuccessorKind {
    type Err = ParseSuccessorKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "supersedes" => Ok(Self::Supersedes),
            "corrects" => Ok(Self::Corrects),
            "invalidates" => Ok(Self::Invalidates),
            "contradicts" => Ok(Self::Contradicts),
            "contests" => Ok(Self::Contests),
            other => Err(ParseSuccessorKindError(other.to_owned())),
        }
    }
}

/// Whether a thought is the current version of what it says.
///
/// # Examples
///
/// ```
/// use mentisdb::VersionStatus;
///
/// assert_eq!(VersionStatus::default(), VersionStatus::Current);
/// assert_eq!(VersionStatus::Superseded.to_string(), "superseded");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// No newer thought replaces or contests this one.
    #[default]
    Current,
    /// A newer thought references it as a probable replacement or
    /// contradicts it, but no typed invalidating relation exists. Still
    /// visible; the agent should read the successor too.
    Contested,
    /// A typed `Supersedes`/`Corrects`/`Invalidates` relation replaced it.
    Superseded,
}

impl std::fmt::Display for VersionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Current => "current",
            Self::Contested => "contested",
            Self::Superseded => "superseded",
        })
    }
}

/// A compact pointer to a newer thought and how it relates to an older one.
///
/// # Examples
///
/// ```
/// use mentisdb::{SuccessorKind, SuccessorNote, ThoughtType};
///
/// let note = SuccessorNote {
///     thought_id: uuid::Uuid::nil(),
///     index: 7,
///     kind: SuccessorKind::Supersedes,
///     thought_type: ThoughtType::Decision,
///     timestamp: chrono::Utc::now(),
/// };
/// assert_eq!(serde_json::to_value(&note).unwrap()["kind"], "supersedes");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuccessorNote {
    /// Id of the newer thought.
    pub thought_id: Uuid,
    /// Append-order index of the newer thought.
    pub index: u64,
    /// How the newer thought relates to the older one.
    pub kind: SuccessorKind,
    /// Type of the newer thought.
    pub thought_type: ThoughtType,
    /// When the newer thought was appended.
    pub timestamp: DateTime<Utc>,
}

/// Version annotation attached to a ranked hit and to every thought JSON.
///
/// It is present on **every** hit, including hits returned with
/// `include_invalidated = true`, so an agent never has to guess whether the
/// text in front of it is current.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Current, contested or superseded.
    pub status: VersionStatus,
    /// Every newer thought that supersedes, corrects, invalidates,
    /// contradicts or contests this one, oldest first.
    pub successors: Vec<SuccessorNote>,
    /// The newest live thought at the end of the supersession chain. Only
    /// set when `status` is [`VersionStatus::Superseded`] and a live end exists.
    pub current_version: Option<SuccessorNote>,
    /// Ids of older thoughts this hit stands in for. Non-empty when the hit
    /// was surfaced because it replaces or contests them.
    pub successor_of: Vec<Uuid>,
}

impl VersionInfo {
    /// Whether the annotation carries nothing worth reporting.
    pub fn is_plain(&self) -> bool {
        self.status == VersionStatus::Current
            && self.successors.is_empty()
            && self.successor_of.is_empty()
    }
}

/// One edge from an older thought to a newer one that replaces or contests it.
#[derive(Debug, Clone)]
pub(crate) struct SuccessorEdge {
    source_id: Uuid,
    source_index: u64,
    source_type: ThoughtType,
    timestamp: DateTime<Utc>,
    kind: SuccessorKind,
    valid_at: Option<DateTime<Utc>>,
    invalid_at: Option<DateTime<Utc>>,
}

impl SuccessorEdge {
    fn in_force_at(&self, as_of: Option<DateTime<Utc>>) -> bool {
        let Some(as_of) = as_of else { return true };
        self.timestamp <= as_of
            && !self.valid_at.is_some_and(|v| v > as_of)
            && !self.invalid_at.is_some_and(|i| i <= as_of)
    }

    fn note(&self) -> SuccessorNote {
        SuccessorNote {
            thought_id: self.source_id,
            index: self.source_index,
            kind: self.kind,
            thought_type: self.source_type,
            timestamp: self.timestamp,
        }
    }
}

/// Reverse index from an older thought to the newer thoughts that replace or
/// contest it. Derived from the stored chain, never persisted.
#[derive(Debug, Default)]
pub(crate) struct LineageIndex {
    by_target: HashMap<Uuid, Vec<SuccessorEdge>>,
}

/// Classify a link from `source` to `target` through `relation` (`None` for
/// kind-less `refs`).
///
/// Returns `None` when the link does not suggest replacement. This is the
/// single place that encodes which weak links count as contesting.
///
/// # Examples
///
/// ```
/// use mentisdb::{contest_kind, SuccessorKind, ThoughtRelationKind, ThoughtType};
///
/// // A typed relation is always strong.
/// assert_eq!(
///     contest_kind(ThoughtType::Decision, Some(ThoughtRelationKind::Supersedes), ThoughtType::Decision),
///     Some(SuccessorKind::Supersedes)
/// );
/// // A Correction that merely references anything contests it.
/// assert_eq!(
///     contest_kind(ThoughtType::Correction, None, ThoughtType::Summary),
///     Some(SuccessorKind::Contests)
/// );
/// // A new Decision referencing an older Decision contests it.
/// assert_eq!(
///     contest_kind(ThoughtType::Decision, None, ThoughtType::Decision),
///     Some(SuccessorKind::Contests)
/// );
/// // A plain finding referencing a decision does not.
/// assert_eq!(contest_kind(ThoughtType::Finding, None, ThoughtType::Decision), None);
/// ```
pub fn contest_kind(
    source: ThoughtType,
    relation: Option<ThoughtRelationKind>,
    target: ThoughtType,
) -> Option<SuccessorKind> {
    use ThoughtRelationKind as R;
    match relation {
        Some(R::Supersedes) => Some(SuccessorKind::Supersedes),
        Some(R::Corrects) => Some(SuccessorKind::Corrects),
        Some(R::Invalidates) => Some(SuccessorKind::Invalidates),
        Some(R::Contradicts) => Some(SuccessorKind::Contradicts),
        Some(R::Supports | R::Summarizes | R::CausedBy | R::BranchesFrom) => None,
        weak => {
            let correcting = matches!(
                source,
                ThoughtType::Correction | ThoughtType::AssumptionInvalidated
            );
            let policy_change =
                matches!(source, ThoughtType::Decision | ThoughtType::StrategyShift)
                    && matches!(
                        target,
                        ThoughtType::Decision
                            | ThoughtType::Constraint
                            | ThoughtType::PreferenceUpdate
                            | ThoughtType::Plan
                            | ThoughtType::StrategyShift
                    )
                    && matches!(weak, None | Some(R::References | R::RelatedTo));
            (correcting || policy_change).then_some(SuccessorKind::Contests)
        }
    }
}

impl LineageIndex {
    /// Record every edge `thought` creates, deriving weak edges from `refs`
    /// and untyped relations. `thoughts` and `id_to_index` must already
    /// contain `thought`'s targets.
    pub(crate) fn observe(
        &mut self,
        thought: &Thought,
        thoughts: &[Thought],
        id_to_index: &HashMap<Uuid, usize>,
    ) {
        let mut candidates: Vec<(
            Uuid,
            ThoughtType,
            Option<ThoughtRelationKind>,
            &crate::ThoughtRelation,
        )> = Vec::new();
        for relation in &thought.relations {
            if relation.chain_key.is_some() {
                continue;
            }
            let Some(&pos) = id_to_index.get(&relation.target_id) else {
                continue;
            };
            candidates.push((
                relation.target_id,
                thoughts[pos].thought_type,
                Some(relation.kind),
                relation,
            ));
        }
        let mut seen: HashMap<Uuid, SuccessorKind> = HashMap::new();
        for (target_id, target_type, kind, relation) in candidates {
            if let Some(succ) = contest_kind(thought.thought_type, kind, target_type) {
                self.add(
                    target_id,
                    thought,
                    succ,
                    relation.valid_at,
                    relation.invalid_at,
                );
                seen.insert(target_id, succ);
            }
        }
        for &index in &thought.refs {
            let Some(target) = thoughts.get(index as usize) else {
                continue;
            };
            if target.id == thought.id || seen.contains_key(&target.id) {
                continue;
            }
            if let Some(succ) = contest_kind(thought.thought_type, None, target.thought_type) {
                self.add(target.id, thought, succ, None, None);
            }
        }
    }

    fn add(
        &mut self,
        target: Uuid,
        source: &Thought,
        kind: SuccessorKind,
        valid_at: Option<DateTime<Utc>>,
        invalid_at: Option<DateTime<Utc>>,
    ) {
        if target == source.id {
            return;
        }
        let edges = self.by_target.entry(target).or_default();
        if let Some(existing) = edges.iter_mut().find(|e| e.source_id == source.id) {
            if kind.strength() > existing.kind.strength() {
                existing.kind = kind;
                existing.valid_at = valid_at;
                existing.invalid_at = invalid_at;
            }
            return;
        }
        edges.push(SuccessorEdge {
            source_id: source.id,
            source_index: source.index,
            source_type: source.thought_type,
            timestamp: source.timestamp,
            kind,
            valid_at,
            invalid_at,
        });
    }

    fn edges(
        &self,
        target: Uuid,
        as_of: Option<DateTime<Utc>>,
    ) -> impl Iterator<Item = &SuccessorEdge> {
        self.by_target
            .get(&target)
            .into_iter()
            .flatten()
            .filter(move |edge| edge.in_force_at(as_of))
    }
}

impl MentisDb {
    /// Whether `id` is invalidated now, or at `as_of` for point-in-time reads,
    /// counting only invalidations `audience` may see.
    pub(crate) fn invalidated_at(
        &self,
        id: Uuid,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> bool {
        match as_of {
            Some(t) => self.is_invalidated_as_of_for(id, t, audience),
            None => self.is_invalidated_for(id, audience),
        }
    }

    /// Successor edges into `target` whose source `audience` may see.
    fn visible_edges(
        &self,
        target: Uuid,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> impl Iterator<Item = &SuccessorEdge> {
        self.lineage
            .edges(target, as_of)
            .filter(move |edge| self.is_visible_index(edge.source_index, audience))
    }

    /// Return whether `thought_id` is current, contested or superseded.
    ///
    /// # Examples
    ///
    /// See the [module example](crate::lineage).
    pub fn version_status(&self, thought_id: Uuid) -> VersionStatus {
        self.version_status_at(thought_id, None, Audience::Authorized)
    }

    fn version_status_at(
        &self,
        id: Uuid,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> VersionStatus {
        if self.invalidated_at(id, as_of, audience) {
            VersionStatus::Superseded
        } else if self.visible_edges(id, as_of, audience).next().is_some() {
            VersionStatus::Contested
        } else {
            VersionStatus::Current
        }
    }

    /// Follow supersessions from `thought_id` to the newest **live** thought
    /// that replaced it, or `None` when it is not superseded or every
    /// successor was itself withdrawn without replacement.
    ///
    /// The walk is cycle-safe and bounded.
    ///
    /// # Examples
    ///
    /// See the [module example](crate::lineage).
    pub fn current_version(&self, thought_id: Uuid) -> Option<&Thought> {
        self.live_successor(thought_id, None, Audience::Authorized)
    }

    pub(crate) fn live_successor(
        &self,
        id: Uuid,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> Option<&Thought> {
        let mut best: Option<&Thought> = None;
        let mut visited: HashSet<Uuid> = HashSet::new();
        let mut frontier = vec![id];
        let mut depth = 0;
        while let Some(current) = frontier.pop() {
            depth += 1;
            if depth > MAX_LINEAGE_DEPTH || !visited.insert(current) {
                continue;
            }
            for edge in self
                .visible_edges(current, as_of, audience)
                .filter(|e| e.kind.is_strong())
            {
                let Some(source) = self.get_thought_by_id(edge.source_id) else {
                    continue;
                };
                if self.invalidated_at(source.id, as_of, audience) {
                    frontier.push(source.id);
                } else if best
                    .is_none_or(|b| (source.timestamp, source.index) > (b.timestamp, b.index))
                {
                    best = Some(source);
                }
            }
        }
        best
    }

    /// Newest live thought contesting `id` through a weak link, if any.
    pub(crate) fn live_contester(
        &self,
        id: Uuid,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> Option<&Thought> {
        self.visible_edges(id, as_of, audience)
            .filter(|e| !e.kind.is_strong())
            .filter_map(|e| self.get_thought_by_id(e.source_id))
            .filter(|t| !self.invalidated_at(t.id, as_of, audience))
            .max_by_key(|t| (t.timestamp, t.index))
    }

    /// Build the [`VersionInfo`] for one thought.
    ///
    /// `as_of` restricts the view to links in force at that instant.
    pub fn version_info(&self, thought: &Thought, as_of: Option<DateTime<Utc>>) -> VersionInfo {
        self.version_info_for(thought, as_of, Audience::Authorized)
    }

    /// [`MentisDb::version_info`] as seen by `audience`: successors, the
    /// current version and the status are computed from thoughts the audience
    /// may see only.
    pub fn version_info_for(
        &self,
        thought: &Thought,
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) -> VersionInfo {
        let status = self.version_status_at(thought.id, as_of, audience);
        let mut successors: Vec<SuccessorNote> = self
            .visible_edges(thought.id, as_of, audience)
            .map(SuccessorEdge::note)
            .collect();
        successors.sort_by_key(|n| (n.timestamp, n.index));
        let current_version = (status == VersionStatus::Superseded)
            .then(|| self.live_successor(thought.id, as_of, audience))
            .flatten()
            .map(|t| SuccessorNote {
                thought_id: t.id,
                index: t.index,
                kind: successors
                    .iter()
                    .find(|n| n.thought_id == t.id)
                    .map_or(SuccessorKind::Supersedes, |n| n.kind),
                thought_type: t.thought_type,
                timestamp: t.timestamp,
            });
        VersionInfo {
            status,
            successors,
            current_version,
            successor_of: Vec::new(),
        }
    }

    /// Ranked search that never leaves the agent holding a stale version.
    ///
    /// See [`RankedSearchQuery::resolve_successors`]. With resolution on
    /// (the default) and `include_invalidated` off, a superseded thought that
    /// would have ranked is replaced by its live successor and a contested
    /// thought is preceded by the thought contesting it. Every returned hit
    /// carries a [`VersionInfo`].
    pub fn query_ranked(&self, request: &RankedSearchQuery) -> RankedSearchResult<'_> {
        let explicit = request.include_invalidated || request.filter.include_invalidated;
        // A chain with no supersession or contest edge has nothing to redirect,
        // so skip the wider ranking pass entirely.
        let has_lineage =
            !self.invalidated_thought_ids().is_empty() || !self.lineage.by_target.is_empty();
        let redirect = request.resolve_successors && !explicit && request.limit > 0 && has_lineage;
        // Collapsing is a relevance feature: a metadata-only listing (no text)
        // must return every thought that matches the filter.
        let has_text = request
            .text
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        let collapse = request
            .collapse_duplicates
            .filter(|_| request.limit > 0 && has_text);
        if !redirect && collapse.is_none() {
            let mut result = self.query_ranked_raw(request);
            self.annotate_hits(&mut result.hits, request.as_of, request.filter.audience);
            return result;
        }
        let mut wide = request.clone();
        if redirect {
            wide.include_invalidated = true;
            wide.filter.include_invalidated = true;
        }
        // Redirecting and collapsing both remove or replace hits, so rank a
        // wider page and trim it back to the caller's limit afterwards.
        // Caller-supplied: saturate so `limit = usize::MAX` cannot overflow.
        wide.limit = request
            .limit
            .saturating_mul(2)
            .max(request.limit.saturating_add(8));
        let mut result = self.query_ranked_raw(&wide);
        let raw = std::mem::take(&mut result.hits);
        let mut hits = if redirect {
            self.redirect_hits(raw, request)
        } else {
            let mut raw = raw;
            self.annotate_hits(&mut raw, request.as_of, request.filter.audience);
            raw
        };
        if let Some(threshold) = collapse {
            hits = collapse_duplicate_hits(hits, threshold);
        }
        hits.truncate(request.limit);
        result.hits = hits;
        result
    }

    fn annotate_hits(
        &self,
        hits: &mut [RankedSearchHit<'_>],
        as_of: Option<DateTime<Utc>>,
        audience: Audience,
    ) {
        for hit in hits {
            let mut info = self.version_info_for(hit.thought, as_of, audience);
            info.successor_of = std::mem::take(&mut hit.version.successor_of);
            hit.version = info;
        }
    }

    fn successor_allowed(&self, thought: &Thought, request: &RankedSearchQuery) -> bool {
        // `query_matches_registry` repeats the audience check, so a successor in
        // a segment the reader may not see is never offered.
        request.filter.matches(thought) && self.query_matches_registry(thought, &request.filter)
    }

    fn redirect_hits<'a>(
        &'a self,
        raw: Vec<RankedSearchHit<'a>>,
        request: &RankedSearchQuery,
    ) -> Vec<RankedSearchHit<'a>> {
        let as_of = request.as_of;
        let audience = request.filter.audience;
        let by_id: HashMap<Uuid, usize> = raw
            .iter()
            .enumerate()
            .map(|(i, h)| (h.thought.id, i))
            .collect();
        // Sized from the hits we actually have, never from the caller-supplied
        // limit, which may be `usize::MAX`.
        let mut out: Vec<RankedSearchHit<'a>> =
            Vec::with_capacity(request.limit.min(raw.len()).saturating_add(1));
        let mut placed: HashMap<Uuid, usize> = HashMap::new();
        let place = |out: &mut Vec<RankedSearchHit<'a>>,
                     placed: &mut HashMap<Uuid, usize>,
                     template: &RankedSearchHit<'a>,
                     thought: &'a Thought,
                     stands_in_for: Uuid,
                     bump: f32| {
            if let Some(&slot) = placed.get(&thought.id) {
                let ids = &mut out[slot].version.successor_of;
                if !ids.contains(&stands_in_for) {
                    ids.push(stands_in_for);
                }
                return;
            }
            let mut hit = by_id
                .get(&thought.id)
                .map_or_else(|| template.clone(), |&i| raw[i].clone());
            hit.thought = thought;
            // Inherit the stand-in's score so later score-based re-sorting
            // (cross-chain merges, REST paging) keeps the successor on top.
            hit.score.total = hit.score.total.max(template.score.total + bump);
            hit.version.successor_of = vec![stands_in_for];
            placed.insert(thought.id, out.len());
            out.push(hit);
        };

        for hit in &raw {
            let id = hit.thought.id;
            if placed.contains_key(&id) {
                continue;
            }
            if self.invalidated_at(id, as_of, audience) {
                if let Some(succ) = self
                    .live_successor(id, as_of, audience)
                    .filter(|t| self.successor_allowed(t, request))
                {
                    place(&mut out, &mut placed, hit, succ, id, 0.0);
                }
                continue;
            }
            if let Some(contester) = self
                .live_contester(id, as_of, audience)
                .filter(|t| self.successor_allowed(t, request))
            {
                place(
                    &mut out,
                    &mut placed,
                    hit,
                    contester,
                    id,
                    CONTEST_SCORE_BUMP,
                );
            }
            placed.insert(id, out.len());
            out.push(hit.clone());
        }
        self.annotate_hits(&mut out, as_of, audience);
        out
    }

    /// Inspect a freshly appended thought and return guidance when it looks
    /// like it replaces older memory without a typed invalidating relation.
    ///
    /// Returns an empty vector for well-linked thoughts. Intended for the
    /// MCP and REST append responses so the agent can fix its linking habits
    /// on the very next call.
    ///
    /// # Examples
    ///
    /// ```
    /// use mentisdb::{AdviceCode, MentisDb, StorageAdapterKind, ThoughtInput, ThoughtType};
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let dir = tempfile::tempdir()?;
    /// let mut chain = MentisDb::open_with_key_and_storage_kind(dir.path(), "adv", StorageAdapterKind::Binary)?;
    /// chain.append_thought("a", ThoughtInput::new(ThoughtType::Decision, "Deploy on Fridays."))?;
    /// let fix = chain
    ///     .append_thought("a", ThoughtInput::new(ThoughtType::Correction, "Deploying on Fridays was wrong."))?
    ///     .clone();
    /// let advice = chain.append_advice(&fix);
    /// assert_eq!(advice[0].code, AdviceCode::CorrectionWithoutInvalidatingRelation);
    /// assert_eq!(advice[0].suggested_targets.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn append_advice(&self, thought: &Thought) -> Vec<AppendAdvice> {
        self.append_advice_for(thought, Audience::Authorized)
    }

    /// [`MentisDb::append_advice`] for a writer who may see `audience`: suggested
    /// targets (and their excerpts) come only from thoughts that audience may
    /// read. The server passes the audience of the *writer*; an unsigned writer
    /// on a chain with private segments must not be handed excerpts of them.
    pub fn append_advice_for(&self, thought: &Thought, audience: Audience) -> Vec<AppendAdvice> {
        if thought
            .relations
            .iter()
            .any(|r| is_invalidating_relation_kind(r.kind))
        {
            return Vec::new();
        }
        let code = match thought.thought_type {
            ThoughtType::Correction | ThoughtType::AssumptionInvalidated => {
                AdviceCode::CorrectionWithoutInvalidatingRelation
            }
            ThoughtType::Decision | ThoughtType::StrategyShift => {
                AdviceCode::DecisionReferencesLiveDecision
            }
            _ => return Vec::new(),
        };
        let mut targets = self.linked_live_targets(thought, audience);
        if targets.is_empty() && code == AdviceCode::CorrectionWithoutInvalidatingRelation {
            targets = self.similar_live_targets(thought, audience);
        }
        if targets.is_empty() {
            return Vec::new();
        }
        let ids: Vec<String> = targets.iter().map(|t| t.thought_id.to_string()).collect();
        let message = match code {
            AdviceCode::CorrectionWithoutInvalidatingRelation => format!(
                "This {:?} has no Supersedes/Corrects/Invalidates relation, so the older memory stays \
                 live and may be returned instead of this one. If it replaces an earlier memory, append \
                 it with relations=[{{\"kind\":\"supersedes\",\"target_id\":\"<id>\"}}] using one of: {}.",
                thought.thought_type,
                ids.join(", ")
            ),
            AdviceCode::DecisionReferencesLiveDecision => format!(
                "This {:?} references earlier decision-like memory that is still live. If it replaces \
                 it, add relations=[{{\"kind\":\"supersedes\",\"target_id\":\"<id>\"}}] (candidates: {}); \
                 until then both versions are returned and the older one is flagged as contested.",
                thought.thought_type,
                ids.join(", ")
            ),
        };
        vec![AppendAdvice {
            code,
            message,
            suggested_targets: targets,
        }]
    }

    fn target_summary(&self, target: &Thought) -> SuggestedTarget {
        SuggestedTarget {
            thought_id: target.id,
            index: target.index,
            thought_type: target.thought_type,
            excerpt: target.content.chars().take(120).collect(),
        }
    }

    /// Live thoughts the new thought already links to through `refs` or
    /// untyped relations.
    fn linked_live_targets(&self, thought: &Thought, audience: Audience) -> Vec<SuggestedTarget> {
        let mut seen = HashSet::new();
        let by_relation = thought
            .relations
            .iter()
            .filter(|r| r.chain_key.is_none())
            .filter_map(|r| self.get_thought_by_id(r.target_id));
        let by_ref = thought
            .refs
            .iter()
            .filter_map(|&i| self.get_thought_by_index(i));
        by_relation
            .chain(by_ref)
            .filter(|t| {
                t.id != thought.id
                    && self.is_visible(t, audience)
                    && !self.is_invalidated_for(t.id, audience)
            })
            .filter(|t| {
                contest_kind(thought.thought_type, None, t.thought_type).is_some()
                    || thought.thought_type == ThoughtType::Correction
            })
            .filter(|t| seen.insert(t.id))
            .take(ADVICE_MAX_TARGETS)
            .map(|t| self.target_summary(t))
            .collect()
    }

    /// Live thoughts lexically closest to the new thought, for corrections
    /// that carry no link at all.
    fn similar_live_targets(&self, thought: &Thought, audience: Audience) -> Vec<SuggestedTarget> {
        let text: String = thought.content.chars().take(ADVICE_QUERY_CHARS).collect();
        let query = RankedSearchQuery::new()
            .with_filter(crate::ThoughtQuery::new().with_audience(audience))
            .with_text(text)
            .with_limit(ADVICE_MAX_TARGETS + 2)
            .with_resolve_successors(false);
        self.query_ranked(&query)
            .hits
            .into_iter()
            .filter(|h| {
                h.thought.id != thought.id && h.matched_terms.len() >= ADVICE_MIN_MATCHED_TERMS
            })
            .take(ADVICE_MAX_TARGETS)
            .map(|h| self.target_summary(h.thought))
            .collect()
    }
}

/// Machine-readable reason for an [`AppendAdvice`].
///
/// # Examples
///
/// ```
/// use mentisdb::AdviceCode;
///
/// assert_eq!(serde_json::to_value(AdviceCode::DecisionReferencesLiveDecision).unwrap(),
///            "decision_references_live_decision");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdviceCode {
    /// A `Correction` or `AssumptionInvalidated` carries no typed
    /// invalidating relation.
    CorrectionWithoutInvalidatingRelation,
    /// A `Decision` or `StrategyShift` references an earlier decision-like
    /// thought that is still live and carries no typed invalidating relation.
    DecisionReferencesLiveDecision,
}

/// A candidate target for the typed relation the agent should add.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuggestedTarget {
    /// Id to use as the relation `target_id`.
    pub thought_id: Uuid,
    /// Append-order index of the candidate.
    pub index: u64,
    /// Type of the candidate.
    pub thought_type: ThoughtType,
    /// First characters of the candidate, so the agent can recognise it.
    pub excerpt: String,
}

/// Guidance returned alongside a successful append.
///
/// The append itself always succeeds; this only tells the agent how to make
/// the next write retrievable as the current version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppendAdvice {
    /// Machine-readable reason.
    pub code: AdviceCode,
    /// Human- and agent-readable instruction naming candidate target ids.
    pub message: String,
    /// Candidate targets for the `Supersedes`/`Corrects`/`Invalidates` relation.
    pub suggested_targets: Vec<SuggestedTarget>,
}

impl MentisDb {
    /// Add `version_status`, `successors` and `current_version` to a thought's
    /// JSON when it is anything but plainly current. Plain thoughts get no
    /// extra keys, which keeps responses compact and backward compatible.
    pub(crate) fn annotate_thought_json(
        &self,
        thought: &Thought,
        json: &mut serde_json::Value,
        audience: Audience,
    ) {
        let info = self.version_info_for(thought, None, audience);
        if info.is_plain() {
            return;
        }
        if let serde_json::Value::Object(map) = json {
            map.insert("version_status".into(), serde_json::json!(info.status));
            map.insert("successors".into(), serde_json::json!(info.successors));
            if let Some(current) = info.current_version {
                map.insert("current_version".into(), serde_json::json!(current));
            }
        }
    }
}

impl MentisDb {
    /// JSON for a ranked hit: [`MentisDb::thought_json`] plus `successor_of`
    /// when the hit was surfaced in place of, or above, older thoughts and
    /// `collapsed_duplicates` when near-copies were folded into it.
    pub fn hit_json(&self, hit: &RankedSearchHit<'_>) -> serde_json::Value {
        self.hit_json_for(hit, Audience::Authorized)
    }

    /// [`MentisDb::hit_json`] as seen by `audience`: pointers into hidden
    /// segments are removed and lineage is computed from visible thoughts only.
    pub fn hit_json_for(&self, hit: &RankedSearchHit<'_>, audience: Audience) -> serde_json::Value {
        let mut json = self.thought_json_for(hit.thought, audience);
        if let serde_json::Value::Object(map) = &mut json {
            if !hit.version.successor_of.is_empty() {
                map.insert(
                    "successor_of".into(),
                    serde_json::json!(hit.version.successor_of),
                );
            }
            if !hit.collapsed.is_empty() {
                map.insert(
                    "collapsed_duplicates".into(),
                    serde_json::json!(hit.collapsed),
                );
            }
        }
        json
    }
}

impl MentisDb {
    /// Prompt suffix naming the thought that contests `thought`, or empty.
    pub(crate) fn contested_suffix(&self, thought: &Thought, audience: Audience) -> String {
        match self.live_contester(thought.id, None, audience) {
            Some(c) if !self.is_invalidated_for(thought.id, audience) => format!(
                "  [CONTESTED: newer {:?} #{} may replace this; read it]",
                c.thought_type, c.index
            ),
            _ => String::new(),
        }
    }
}

/// Name of the environment variable read by [`collapse_duplicates_from_env`].
pub const COLLAPSE_DUPLICATES_ENV: &str = "MENTISDB_COLLAPSE_DUPLICATES";

/// Default near-duplicate threshold of [`RankedSearchQuery::collapse_duplicates`].
///
/// Reads `MENTISDB_COLLAPSE_DUPLICATES`: a number in `0.0..=1.0` enables
/// collapsing at that Jaccard threshold, `off`/`false`/`0` disables it, and an
/// unset or unparsable value yields the built-in default
/// ([`DEFAULT_COLLAPSE_DUPLICATES`], `0.9`: LongMemEval R@5 rose from 68.0% to
/// 68.4% and LoCoMo R@10 was unchanged at 73.1%).
pub fn collapse_duplicates_from_env() -> Option<f32> {
    match std::env::var(COLLAPSE_DUPLICATES_ENV)
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("off" | "false" | "0" | "none") => None,
        Some(value) => match value.parse::<f32>() {
            Ok(t) if (0.0..=1.0).contains(&t) && t > 0.0 => Some(t),
            _ => DEFAULT_COLLAPSE_DUPLICATES,
        },
        None => DEFAULT_COLLAPSE_DUPLICATES,
    }
}

/// Built-in default of [`collapse_duplicates_from_env`].
pub const DEFAULT_COLLAPSE_DUPLICATES: Option<f32> = Some(0.9);

fn token_jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count();
    inter as f32 / (a.len() + b.len() - inter) as f32
}

/// Drop every hit that is a near-duplicate (Jaccard of normalized tokens at
/// least `threshold`) of a higher-ranked hit, recording the dropped ids on the
/// survivor. Keeps rank order.
///
/// # Examples
///
/// ```
/// use mentisdb::{MentisDb, RankedSearchQuery, StorageAdapterKind, ThoughtInput, ThoughtType};
///
/// # fn main() -> std::io::Result<()> {
/// let dir = tempfile::tempdir()?;
/// let mut chain = MentisDb::open_with_key_and_storage_kind(dir.path(), "dup", StorageAdapterKind::Binary)?;
/// for _ in 0..3 {
///     chain.append_thought("a", ThoughtInput::new(ThoughtType::Finding, "The build takes forty seconds on CI."))?;
/// }
/// let q = RankedSearchQuery::new().with_text("build seconds CI").with_collapse_duplicates(Some(0.9));
/// let hits = chain.query_ranked(&q).hits;
/// assert_eq!(hits.len(), 1);
/// assert_eq!(hits[0].collapsed.len(), 2);
/// # Ok(())
/// # }
/// ```
pub(crate) fn collapse_duplicate_hits<'a>(
    hits: Vec<RankedSearchHit<'a>>,
    threshold: f32,
) -> Vec<RankedSearchHit<'a>> {
    use crate::search::lexical::normalize_lexical_tokens;
    let mut kept: Vec<(HashSet<String>, RankedSearchHit<'a>)> = Vec::with_capacity(hits.len());
    for hit in hits {
        let tokens: HashSet<String> = normalize_lexical_tokens(&hit.thought.content, false)
            .into_iter()
            .collect();
        match kept
            .iter_mut()
            .find(|(t, _)| token_jaccard(t, &tokens) >= threshold)
        {
            Some((_, survivor)) => survivor.collapsed.push(hit.thought.id),
            None => kept.push((tokens, hit)),
        }
    }
    kept.into_iter().map(|(_, hit)| hit).collect()
}
