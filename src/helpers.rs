//! Ergonomic helpers for constructing common [`ThoughtInput`] configurations.
//!
//! These functions are thin builders that pre-configure the most frequently
//! used [`ThoughtType`] and [`ThoughtRole`] combinations so callers do not
//! have to repeat the same setup for routine memory writes.
//!
//! Every helper returns a [`ThoughtInput`] whose remaining fields (agent
//! identity, tags, concepts, refs, importance, …) can be further customised
//! with the builder methods on [`ThoughtInput`] before passing the value to
//! [`MentisDb::append`](crate::MentisDb::append).
//!
//! # Example
//!
//! ```rust
//! use mentisdb::helpers::{thought_decision, thought_retrospective};
//!
//! let decision = thought_decision("Use binary storage for all new chains.")
//!     .with_agent_name("architect-agent")
//!     .with_tags(["storage", "architecture"]);
//!
//! let retro = thought_retrospective("Session complete. Migrated 3 chains. Remaining: none.")
//!     .with_agent_name("architect-agent");
//! ```

use crate::{ThoughtInput, ThoughtRole, ThoughtType};

/// Create a [`ThoughtInput`] pre-configured for recording a concrete design or
/// implementation decision.
///
/// The returned input has `thought_type = Decision` and the default
/// [`ThoughtRole::Memory`] role.  Pair with [`thought_lesson_learned`] when
/// the decision was hard-won and you want future agents to understand why.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_decision;
///
/// let input = thought_decision("Adopt binary storage for all new chains.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Decision);
/// ```
pub fn thought_decision(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Decision, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a non-obvious
/// technical insight or useful realisation.
///
/// The returned input has `thought_type = Insight` and the default
/// [`ThoughtRole::Memory`] role.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_insight;
///
/// let input = thought_insight("Hash-chaining prevents silent corruption even with concurrent writers.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Insight);
/// ```
pub fn thought_insight(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Insight, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a wrong action or
/// misdirection (distinct from a factual error; use [`thought_correction`] for
/// those).
///
/// The returned input has `thought_type = Mistake` and the default
/// [`ThoughtRole::Memory`] role.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_mistake;
///
/// let input = thought_mistake("Ran migration on the wrong chain directory.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Mistake);
/// ```
pub fn thought_mistake(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Mistake, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a corrected version
/// of a prior false belief or factual error.
///
/// The returned input has `thought_type = Correction` and the default
/// [`ThoughtRole::Memory`] role.  Use [`ThoughtInput::with_relations`] to
/// point back at the thought being corrected with
/// [`ThoughtRelationKind::Corrects`](crate::ThoughtRelationKind::Corrects).
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_correction;
///
/// let input = thought_correction("The correct field name is `prev_hash`, not `previous_hash`.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Correction);
/// ```
pub fn thought_correction(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Correction, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a plan for future
/// work.
///
/// The returned input has `thought_type = Plan` and the default
/// [`ThoughtRole::Memory`] role.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_plan;
///
/// let input = thought_plan("Implement cross-chain search in three phases: …");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Plan);
/// ```
pub fn thought_plan(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Plan, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a retrospective
/// operating rule distilled from a failure or expensive fix.
///
/// The returned input has `thought_type = LessonLearned` and the default
/// [`ThoughtRole::Memory`] role.  Pair with [`thought_mistake`] and add a
/// `CausedBy` relation when the lesson came directly from a recorded mistake.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_lesson_learned;
///
/// let input = thought_lesson_learned("Always verify chain_key casing before migration.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::LessonLearned);
/// ```
pub fn thought_lesson_learned(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::LessonLearned, content)
}

/// Create a [`ThoughtInput`] pre-configured for an end-of-session retrospective
/// summary.
///
/// The returned input has `thought_type = Summary` and
/// [`ThoughtRole::Retrospective`].  Write this at ~40 % context usage or
/// before any session termination or handoff so the next agent can resume
/// without loss of state.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_retrospective;
///
/// let input = thought_retrospective("Session complete. Migrated 3 chains successfully.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Summary);
/// assert_eq!(input.role, mentisdb::ThoughtRole::Retrospective);
/// ```
pub fn thought_retrospective(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Summary, content).with_role(ThoughtRole::Retrospective)
}

/// Create a [`ThoughtInput`] pre-configured for recording successful completion
/// of a concrete deliverable.
///
/// The returned input has `thought_type = TaskComplete` and the default
/// [`ThoughtRole::Memory`] role.
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_task_complete;
///
/// let input = thought_task_complete("PR #42 merged: add cross-chain search.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::TaskComplete);
/// ```
pub fn thought_task_complete(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::TaskComplete, content)
}

/// Create a [`ThoughtInput`] pre-configured for recording a resumption
/// checkpoint — a snapshot of current state that lets another agent (or a
/// future session) restart quickly.
///
/// The returned input has `thought_type = Summary` and
/// [`ThoughtRole::Checkpoint`].
///
/// # Example
///
/// ```rust
/// use mentisdb::helpers::thought_checkpoint;
///
/// let input = thought_checkpoint("Completed phases 1-2. Phase 3 (vector index) starts next.");
/// assert_eq!(input.thought_type, mentisdb::ThoughtType::Summary);
/// assert_eq!(input.role, mentisdb::ThoughtRole::Checkpoint);
/// ```
pub fn thought_checkpoint(content: impl Into<String>) -> ThoughtInput {
    ThoughtInput::new(ThoughtType::Summary, content).with_role(ThoughtRole::Checkpoint)
}


