//! Standing context: what is durably true, regardless of how old it is.
//!
//! MentisDB memory has two halves. The *episodic* half is what happened, in
//! time order; [`MentisDb::to_catchup_prompt`] and the `mentisdb_recent_context`
//! tool serve it by returning the last N thoughts. That window forgets: a
//! preference the user stated three months ago has long since scrolled out of
//! it. The *semantic* half is what stays true about the user and the project
//! (preferences, constraints, decisions, goals, lessons). Nothing about it
//! depends on recency, so it must not be loaded by recency.
//!
//! [`MentisDb::standing_context`] serves that half. It selects the live
//! thoughts of the *standing types*, ranks them by importance (newest first on
//! ties), collapses restated near-duplicates, caps the result per type and in
//! total, and renders a compact prompt block an agent loads at session start
//! next to the recent-context block.
//!
//! # What counts as standing
//!
//! | Group | Selected thoughts | Cap |
//! |---|---|---|
//! | [`StandingGroup::Constraint`] | `Constraint` | [`StandingContextQuery::per_type_cap`] |
//! | [`StandingGroup::Preference`] | `PreferenceUpdate` | per-type cap |
//! | [`StandingGroup::UserTrait`] | `UserTrait` | per-type cap |
//! | [`StandingGroup::Relationship`] | `RelationshipUpdate` | per-type cap |
//! | [`StandingGroup::Goal`] | `Goal` | per-type cap |
//! | [`StandingGroup::Decision`] | `Decision` | per-type cap |
//! | [`StandingGroup::Lesson`] | `LessonLearned` | per-type cap |
//! | [`StandingGroup::Handoff`] | `Handoff` | the single newest |
//! | [`StandingGroup::Checkpoint`] | `Checkpoint` / `Summary` type or role | the single newest |
//!
//! Only thoughts in the `scope:user` memory scope or carrying no `scope:` tag
//! are standing; `scope:session` and `scope:agent` thoughts are private to a
//! session or an agent and are skipped (see [`MemoryScope`]).
//!
//! # Examples
//!
//! ```rust
//! use mentisdb::{MentisDb, StandingContextQuery, ThoughtType};
//! use std::path::PathBuf;
//!
//! # fn main() -> std::io::Result<()> {
//! let dir = tempfile::tempdir()?;
//! let mut chain = MentisDb::open(&PathBuf::from(dir.path()), "agent1", "Agent", None, None)?;
//! chain.append("agent1", ThoughtType::PreferenceUpdate, "User prefers tabs over spaces.")?;
//! chain.append("agent1", ThoughtType::Finding, "The build takes 40 seconds.")?;
//!
//! let standing = chain.standing_context(&StandingContextQuery::new());
//! assert_eq!(standing.entries.len(), 1);
//! assert!(standing.prompt.starts_with("=== STANDING CONTEXT ==="));
//! assert!(standing.prompt.contains("User prefers tabs over spaces."));
//! # Ok(())
//! # }
//! ```

use crate::search::lexical::normalize_lexical_tokens;
use crate::{MemoryScope, MentisDb, Thought, ThoughtQuery, ThoughtRole, ThoughtType};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use uuid::Uuid;

/// Default number of thoughts kept per [`StandingGroup`].
pub const DEFAULT_STANDING_PER_TYPE_CAP: usize = 8;

/// Default cap on the durable (non-slot) thoughts across all groups.
pub const DEFAULT_STANDING_TOTAL_CAP: usize = 40;

/// Normalized-token Jaccard similarity at or above which two standing
/// thoughts of the same group are considered restatements of one another.
pub const STANDING_DUPLICATE_THRESHOLD: f32 = 0.85;

/// A bucket of standing thoughts, rendered as one section of the prompt.
///
/// The variant order is the render order: hard constraints first, the single
/// latest handoff and checkpoint last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StandingGroup {
    /// Hard limits the agent must respect (`ThoughtType::Constraint`).
    Constraint,
    /// Stated user preferences (`ThoughtType::PreferenceUpdate`).
    Preference,
    /// Durable characteristics of the user (`ThoughtType::UserTrait`).
    UserTrait,
    /// The agent's model of its relationship with the user
    /// (`ThoughtType::RelationshipUpdate`).
    Relationship,
    /// What the user or project is trying to achieve (`ThoughtType::Goal`).
    Goal,
    /// Choices already made (`ThoughtType::Decision`).
    Decision,
    /// Lessons that should change future behavior (`ThoughtType::LessonLearned`).
    Lesson,
    /// The most recent `Handoff`, at most one thought.
    Handoff,
    /// The most recent `Checkpoint` or `Summary` (by type or role), at most
    /// one thought.
    Checkpoint,
}

impl StandingGroup {
    /// Every group in render order.
    pub const ALL: [StandingGroup; 9] = [
        StandingGroup::Constraint,
        StandingGroup::Preference,
        StandingGroup::UserTrait,
        StandingGroup::Relationship,
        StandingGroup::Goal,
        StandingGroup::Decision,
        StandingGroup::Lesson,
        StandingGroup::Handoff,
        StandingGroup::Checkpoint,
    ];

    /// Human-readable section title used in the rendered prompt.
    pub fn title(self) -> &'static str {
        match self {
            StandingGroup::Constraint => "Constraints",
            StandingGroup::Preference => "Preferences",
            StandingGroup::UserTrait => "About the user",
            StandingGroup::Relationship => "Relationship",
            StandingGroup::Goal => "Goals",
            StandingGroup::Decision => "Decisions",
            StandingGroup::Lesson => "Lessons",
            StandingGroup::Handoff => "Latest handoff",
            StandingGroup::Checkpoint => "Latest checkpoint",
        }
    }

    /// `true` for the groups that hold only the single newest thought and are
    /// exempt from the total cap.
    pub fn is_latest_slot(self) -> bool {
        matches!(self, StandingGroup::Handoff | StandingGroup::Checkpoint)
    }

    /// The group a thought type belongs to, or `None` when the type is not
    /// standing.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{StandingGroup, ThoughtType};
    ///
    /// assert_eq!(StandingGroup::for_type(ThoughtType::Decision), Some(StandingGroup::Decision));
    /// assert_eq!(StandingGroup::for_type(ThoughtType::Summary), Some(StandingGroup::Checkpoint));
    /// assert_eq!(StandingGroup::for_type(ThoughtType::Finding), None);
    /// ```
    pub fn for_type(thought_type: ThoughtType) -> Option<Self> {
        match thought_type {
            ThoughtType::Constraint => Some(StandingGroup::Constraint),
            ThoughtType::PreferenceUpdate => Some(StandingGroup::Preference),
            ThoughtType::UserTrait => Some(StandingGroup::UserTrait),
            ThoughtType::RelationshipUpdate => Some(StandingGroup::Relationship),
            ThoughtType::Goal => Some(StandingGroup::Goal),
            ThoughtType::Decision => Some(StandingGroup::Decision),
            ThoughtType::LessonLearned => Some(StandingGroup::Lesson),
            ThoughtType::Handoff => Some(StandingGroup::Handoff),
            ThoughtType::Checkpoint | ThoughtType::Summary => Some(StandingGroup::Checkpoint),
            _ => None,
        }
    }

    fn for_thought(thought: &Thought, types_filter: bool) -> Option<Self> {
        Self::for_type(thought.thought_type).or_else(|| {
            let snapshot_role =
                matches!(thought.role, ThoughtRole::Checkpoint | ThoughtRole::Summary);
            (!types_filter && snapshot_role).then_some(StandingGroup::Checkpoint)
        })
    }
}

impl fmt::Display for StandingGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// Selection parameters for [`MentisDb::standing_context`].
///
/// Every field is optional; [`StandingContextQuery::new`] gives the defaults
/// the MCP tool and REST route use.
#[derive(Debug, Clone, PartialEq)]
pub struct StandingContextQuery {
    /// Only thoughts written by this agent id.
    pub agent_id: Option<String>,
    /// Restrict to these thought types. `None` selects every standing type
    /// plus checkpoint-role and summary-role thoughts. Types that are not
    /// standing (see [`StandingGroup::for_type`]) select nothing.
    pub types: Option<Vec<ThoughtType>>,
    /// Only thoughts appended at or after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Point-in-time view: ignore thoughts appended after this instant and
    /// treat only invalidations already in force then as superseding.
    pub as_of: Option<DateTime<Utc>>,
    /// Keep superseded, corrected and invalidated thoughts. Default `false`.
    pub include_invalidated: bool,
    /// Maximum thoughts per [`StandingGroup`]. Default
    /// [`DEFAULT_STANDING_PER_TYPE_CAP`].
    pub per_type_cap: usize,
    /// Maximum durable thoughts across all groups; the latest-handoff and
    /// latest-checkpoint slots do not count. Default
    /// [`DEFAULT_STANDING_TOTAL_CAP`].
    pub total_cap: usize,
    /// Which segments of the chain the selection may draw from. Default
    /// [`crate::audience::Audience::Authorized`]; the server sets it from the
    /// caller's proof. A public reader gets standing context made only of
    /// public-segment thoughts.
    pub audience: crate::audience::Audience,
}

impl Default for StandingContextQuery {
    fn default() -> Self {
        Self {
            agent_id: None,
            types: None,
            since: None,
            as_of: None,
            include_invalidated: false,
            per_type_cap: DEFAULT_STANDING_PER_TYPE_CAP,
            total_cap: DEFAULT_STANDING_TOTAL_CAP,
            audience: crate::audience::Audience::default(),
        }
    }
}

impl StandingContextQuery {
    /// Default query: all standing types, all agents, live thoughts, caps of
    /// [`DEFAULT_STANDING_PER_TYPE_CAP`] and [`DEFAULT_STANDING_TOTAL_CAP`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Select as `audience` (see [`StandingContextQuery::audience`]).
    pub fn with_audience(mut self, audience: crate::audience::Audience) -> Self {
        self.audience = audience;
        self
    }

    /// Restrict to one agent id.
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    /// Restrict to the given thought types.
    pub fn with_types(mut self, types: Vec<ThoughtType>) -> Self {
        self.types = Some(types);
        self
    }

    /// Only thoughts appended at or after `since`.
    pub fn with_since(mut self, since: DateTime<Utc>) -> Self {
        self.since = Some(since);
        self
    }

    /// Evaluate the chain as it stood at `as_of`.
    pub fn with_as_of(mut self, as_of: DateTime<Utc>) -> Self {
        self.as_of = Some(as_of);
        self
    }

    /// Keep invalidated thoughts too.
    pub fn with_include_invalidated(mut self, include: bool) -> Self {
        self.include_invalidated = include;
        self
    }

    /// Set the per-group cap.
    pub fn with_per_type_cap(mut self, cap: usize) -> Self {
        self.per_type_cap = cap;
        self
    }

    /// Set the cap on durable thoughts across all groups.
    pub fn with_total_cap(mut self, cap: usize) -> Self {
        self.total_cap = cap;
        self
    }
}

/// One selected standing thought, detached from the chain so it can be
/// serialized and returned across a process boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingEntry {
    /// Group the thought was filed under.
    pub group: StandingGroup,
    /// Append-order index of the thought.
    pub index: u64,
    /// Stable id of the thought.
    pub id: Uuid,
    /// Semantic type of the thought.
    pub thought_type: ThoughtType,
    /// Role of the thought.
    pub role: ThoughtRole,
    /// Id of the agent that wrote it.
    pub agent_id: String,
    /// When it was appended.
    pub timestamp: DateTime<Utc>,
    /// Importance in `0.0..=1.0`, the primary ranking key.
    pub importance: f32,
    /// The thought text.
    pub content: String,
}

/// Result of [`MentisDb::standing_context`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StandingContext {
    /// Selected thoughts in render order: group order, then importance
    /// descending, newest first on ties.
    pub entries: Vec<StandingEntry>,
    /// Near-duplicate thoughts collapsed into a newer or higher-ranked one.
    pub duplicates_collapsed: usize,
    /// Prompt block starting with `=== STANDING CONTEXT ===`; empty when
    /// `entries` is empty.
    pub prompt: String,
}

/// A candidate with its precomputed token set for duplicate detection.
struct Candidate<'a> {
    thought: &'a Thought,
    tokens: HashSet<String>,
}

impl<'a> Candidate<'a> {
    fn new(thought: &'a Thought) -> Self {
        let tokens = normalize_lexical_tokens(&thought.content, false)
            .into_iter()
            .collect();
        Self { thought, tokens }
    }

    fn similar_to(&self, other: &Candidate<'_>) -> bool {
        jaccard(&self.tokens, &other.tokens) >= STANDING_DUPLICATE_THRESHOLD
    }
}

/// Jaccard similarity of two token sets; `0.0` when either set is empty.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    intersection as f32 / (a.len() + b.len() - intersection) as f32
}

/// Importance descending, then newest (highest index) first.
fn rank_order(a: &Thought, b: &Thought) -> std::cmp::Ordering {
    b.importance
        .total_cmp(&a.importance)
        .then(b.index.cmp(&a.index))
}

/// `true` when the thought is user-scoped or carries no scope tag.
fn is_standing_scope(thought: &Thought) -> bool {
    let mut scopes = thought
        .tags
        .iter()
        .filter_map(|tag| MemoryScope::from_tag(tag))
        .peekable();
    scopes.peek().is_none() || scopes.any(|scope| scope == MemoryScope::User)
}

/// Pick up to `cap` candidates of one group by rank, collapsing near-duplicates.
///
/// A candidate similar to an already selected thought replaces it only when it
/// is newer; otherwise it is dropped. Returns the selection and the number of
/// collapsed duplicates.
fn select_group<'a>(mut pool: Vec<Candidate<'a>>, cap: usize) -> (Vec<Candidate<'a>>, usize) {
    pool.sort_by(|a, b| rank_order(a.thought, b.thought));
    let mut selected: Vec<Candidate<'a>> = Vec::with_capacity(cap.min(pool.len()));
    let mut collapsed = 0;
    for candidate in pool {
        match selected.iter().position(|kept| candidate.similar_to(kept)) {
            Some(slot) => {
                collapsed += 1;
                if candidate.thought.index > selected[slot].thought.index {
                    selected[slot] = candidate;
                }
            }
            None if selected.len() < cap => selected.push(candidate),
            None => {}
        }
    }
    selected.sort_by(|a, b| rank_order(a.thought, b.thought));
    (selected, collapsed)
}

impl MentisDb {
    /// Select the durable, age-independent thoughts an agent should know at
    /// session start: preferences, constraints, decisions, goals, lessons,
    /// user traits, the relationship model, plus the newest handoff and
    /// checkpoint.
    ///
    /// Complements [`MentisDb::to_catchup_prompt`], which serves recency. See
    /// the [module documentation](crate::standing) for the selection rules,
    /// scoping and caps. Invalidated thoughts are excluded unless
    /// [`StandingContextQuery::include_invalidated`] is set; `as_of` applies
    /// the same point-in-time rules as ranked search.
    ///
    /// Runs in one pass over the chain plus, per group, at most
    /// `candidates x per_type_cap` token-set comparisons.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use mentisdb::{MentisDb, StandingContextQuery, ThoughtType};
    /// use std::path::PathBuf;
    ///
    /// # fn main() -> std::io::Result<()> {
    /// let dir = tempfile::tempdir()?;
    /// let mut chain = MentisDb::open(&PathBuf::from(dir.path()), "a", "A", None, None)?;
    /// chain.append("a", ThoughtType::Constraint, "Never deploy on Fridays.")?;
    /// chain.append("a", ThoughtType::Decision, "We chose Postgres over MySQL.")?;
    ///
    /// let query = StandingContextQuery::new().with_types(vec![ThoughtType::Constraint]);
    /// let standing = chain.standing_context(&query);
    /// assert_eq!(standing.entries.len(), 1);
    /// assert!(standing.prompt.contains("Never deploy on Fridays."));
    /// # Ok(())
    /// # }
    /// ```
    pub fn standing_context(&self, query: &StandingContextQuery) -> StandingContext {
        let mut filter = ThoughtQuery::new();
        filter.agent_ids = query.agent_id.clone().map(|id| vec![id]);
        filter.since = query.since;
        filter.audience = query.audience;
        let wanted = query.types.as_deref();
        // Superseded thoughts are fetched too so that each can be replaced by
        // its live successor: dropping them would leave the digest with no
        // constraint or decision at all whenever the replacement is not itself
        // a standing type (for example a `Correction`).
        let candidates = self.retrieval_candidates(&filter, query.as_of, true);
        let mut seen: HashSet<Uuid> = HashSet::new();

        let mut pools: Vec<(StandingGroup, Vec<Candidate<'_>>)> = StandingGroup::ALL
            .iter()
            .map(|group| (*group, Vec::new()))
            .collect();
        for thought in candidates {
            if wanted.is_some_and(|set| !set.contains(&thought.thought_type)) {
                continue;
            }
            if !is_standing_scope(thought) {
                continue;
            }
            let Some(group) = StandingGroup::for_thought(thought, wanted.is_some()) else {
                continue;
            };
            let superseded = !query.include_invalidated
                && self.invalidated_at(thought.id, query.as_of, query.audience);
            let mut subjects: Vec<&Thought> = Vec::new();
            if superseded {
                subjects.extend(self.live_successor(thought.id, query.as_of, query.audience));
            } else {
                subjects.push(thought);
                subjects.extend(self.live_contester(thought.id, query.as_of, query.audience));
            }
            for subject in subjects {
                if !seen.insert(subject.id) {
                    continue;
                }
                if let Some((_, pool)) = pools.iter_mut().find(|(g, _)| *g == group) {
                    pool.push(Candidate::new(subject));
                }
            }
        }

        let mut duplicates_collapsed = 0;
        let mut selected: Vec<(StandingGroup, Candidate<'_>)> = Vec::new();
        for (group, pool) in pools {
            let cap = if group.is_latest_slot() {
                1
            } else {
                query.per_type_cap
            };
            let (picked, collapsed) = select_group(pool, cap);
            duplicates_collapsed += collapsed;
            selected.extend(picked.into_iter().map(|c| (group, c)));
        }
        let selected = apply_total_cap(selected, query.total_cap);

        let entries: Vec<StandingEntry> = selected
            .iter()
            .map(|(group, c)| StandingEntry {
                group: *group,
                index: c.thought.index,
                id: c.thought.id,
                thought_type: c.thought.thought_type,
                role: c.thought.role,
                agent_id: c.thought.agent_id.clone(),
                timestamp: c.thought.timestamp,
                importance: c.thought.importance,
                content: c.thought.content.clone(),
            })
            .collect();
        let prompt = self.render_standing_prompt(&selected, query.audience);
        StandingContext {
            entries,
            duplicates_collapsed,
            prompt,
        }
    }

    /// Render the grouped `=== STANDING CONTEXT ===` block; empty when there
    /// is nothing selected.
    fn render_standing_prompt(
        &self,
        selected: &[(StandingGroup, Candidate<'_>)],
        audience: crate::audience::Audience,
    ) -> String {
        if selected.is_empty() {
            return String::new();
        }
        let mut prompt = String::from("=== STANDING CONTEXT ===\n");
        for group in StandingGroup::ALL {
            let mut lines = selected.iter().filter(|(g, _)| *g == group).peekable();
            if lines.peek().is_none() {
                continue;
            }
            prompt.push_str(&format!("\n-- {} --\n", group.title()));
            for (_, c) in lines {
                let t = c.thought;
                prompt.push_str(&format!(
                    "[#{}] {:?}/{:?} ({}, {}) {}{}\n",
                    t.index,
                    t.thought_type,
                    t.role,
                    self.agent_label_for(t),
                    t.timestamp.format("%Y-%m-%d"),
                    t.content,
                    self.contested_suffix(t, audience)
                ));
            }
        }
        prompt.push_str("\n=== END STANDING CONTEXT ===\n");
        prompt
    }
}

/// Keep the latest-slot thoughts and the best `total_cap` durable thoughts,
/// preserving group order.
fn apply_total_cap<'a>(
    selected: Vec<(StandingGroup, Candidate<'a>)>,
    total_cap: usize,
) -> Vec<(StandingGroup, Candidate<'a>)> {
    let durable = selected.iter().filter(|(g, _)| !g.is_latest_slot()).count();
    if durable <= total_cap {
        return selected;
    }
    let mut ranked: Vec<&Thought> = selected
        .iter()
        .filter(|(g, _)| !g.is_latest_slot())
        .map(|(_, c)| c.thought)
        .collect();
    ranked.sort_by(|a, b| rank_order(a, b));
    let keep: HashSet<u64> = ranked.iter().take(total_cap).map(|t| t.index).collect();
    selected
        .into_iter()
        .filter(|(g, c)| g.is_latest_slot() || keep.contains(&c.thought.index))
        .collect()
}
