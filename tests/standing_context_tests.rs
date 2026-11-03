//! Behavior of `MentisDb::standing_context`: the durable, age-independent half
//! of agent memory that the recent-context window cannot serve.

use chrono::Duration;
use mentisdb::{
    MemoryScope, MentisDb, StandingContextQuery, StandingGroup, ThoughtInput, ThoughtRelation,
    ThoughtRelationKind, ThoughtRole, ThoughtType,
};
use tempfile::{tempdir, TempDir};

fn open() -> (MentisDb, TempDir) {
    let dir = tempdir().unwrap();
    let chain = MentisDb::open_with_key(dir.path(), "standing").unwrap();
    (chain, dir)
}

fn supersedes(target: uuid::Uuid, content: &str, thought_type: ThoughtType) -> ThoughtInput {
    ThoughtInput::new(thought_type, content).with_relations(vec![ThoughtRelation {
        kind: ThoughtRelationKind::Supersedes,
        target_id: target,
        chain_key: None,
        valid_at: None,
        invalid_at: None,
    }])
}

fn contents(chain: &MentisDb, query: &StandingContextQuery) -> Vec<String> {
    chain
        .standing_context(query)
        .entries
        .into_iter()
        .map(|e| e.content)
        .collect()
}

/// The reported bug: a preference stated long ago is outside the recent window
/// but must still be in the standing context.
#[test]
fn test_old_preference_survives_beyond_the_recent_window() {
    let (mut chain, _dir) = open();
    chain
        .append(
            "a",
            ThoughtType::PreferenceUpdate,
            "User wants answers in Spanish.",
        )
        .unwrap();
    for i in 0..40 {
        chain
            .append("a", ThoughtType::Finding, &format!("unrelated finding {i}"))
            .unwrap();
    }
    assert!(!chain.to_catchup_prompt(12).contains("Spanish"));
    let standing = chain.standing_context(&StandingContextQuery::new());
    assert!(standing.prompt.contains("User wants answers in Spanish."));
    assert_eq!(standing.entries.len(), 1);
    assert_eq!(standing.entries[0].group, StandingGroup::Preference);
}

/// Episodic types are never standing, and an empty selection renders nothing.
#[test]
fn test_non_standing_types_are_excluded_and_empty_renders_empty_prompt() {
    let (mut chain, _dir) = open();
    chain
        .append("a", ThoughtType::Finding, "a finding")
        .unwrap();
    chain
        .append("a", ThoughtType::TaskComplete, "done")
        .unwrap();
    let standing = chain.standing_context(&StandingContextQuery::new());
    assert!(standing.entries.is_empty());
    assert_eq!(standing.prompt, "");
}

/// Session- and agent-scoped thoughts are private; user-scoped and unscoped
/// thoughts are standing.
#[test]
fn test_scope_filter_keeps_user_and_unscoped_only() {
    let (mut chain, _dir) = open();
    let add = |chain: &mut MentisDb, text: &str, scope: Option<MemoryScope>| {
        let mut input = ThoughtInput::new(ThoughtType::Constraint, text);
        if let Some(scope) = scope {
            input = input.with_scope(scope);
        }
        chain.append_thought("a", input).unwrap();
    };
    add(&mut chain, "unscoped rule alpha", None);
    add(&mut chain, "user rule bravo", Some(MemoryScope::User));
    add(
        &mut chain,
        "session rule charlie",
        Some(MemoryScope::Session),
    );
    add(&mut chain, "agent rule delta", Some(MemoryScope::Agent));
    let found = contents(&chain, &StandingContextQuery::new());
    assert_eq!(found.len(), 2);
    assert!(found.contains(&"unscoped rule alpha".to_string()));
    assert!(found.contains(&"user rule bravo".to_string()));
}

/// A superseded preference is hidden by default and shown on request.
#[test]
fn test_invalidated_thoughts_are_hidden_unless_requested() {
    let (mut chain, _dir) = open();
    let old = chain
        .append(
            "a",
            ThoughtType::PreferenceUpdate,
            "User likes light theme.",
        )
        .unwrap()
        .id;
    chain
        .append_thought(
            "a",
            supersedes(
                old,
                "User now likes dark theme.",
                ThoughtType::PreferenceUpdate,
            ),
        )
        .unwrap();
    let live = contents(&chain, &StandingContextQuery::new());
    assert_eq!(live, vec!["User now likes dark theme."]);
    let all = contents(
        &chain,
        &StandingContextQuery::new().with_include_invalidated(true),
    );
    assert_eq!(all.len(), 2);
}

/// `as_of` shows the chain as it stood before the superseding thought.
#[test]
fn test_as_of_restores_the_preference_that_was_current_then() {
    let (mut chain, _dir) = open();
    let old = chain
        .append(
            "a",
            ThoughtType::PreferenceUpdate,
            "User likes light theme.",
        )
        .unwrap()
        .clone();
    let newer = chain
        .append_thought(
            "a",
            supersedes(
                old.id,
                "User now likes dark theme.",
                ThoughtType::PreferenceUpdate,
            ),
        )
        .unwrap()
        .clone();
    let before = newer.timestamp - Duration::nanoseconds(1);
    let found = contents(&chain, &StandingContextQuery::new().with_as_of(before));
    assert_eq!(found, vec!["User likes light theme."]);
}

/// Within a group: importance descending, newest first on ties.
#[test]
fn test_ordering_is_importance_then_newest() {
    let (mut chain, _dir) = open();
    for (text, importance) in [("rule one", 0.5), ("rule two", 0.9), ("rule three", 0.5)] {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(ThoughtType::Constraint, text).with_importance(importance),
            )
            .unwrap();
    }
    assert_eq!(
        contents(&chain, &StandingContextQuery::new()),
        vec!["rule two", "rule three", "rule one"]
    );
}

/// Restated preferences collapse into the newest; distinct ones stay.
#[test]
fn test_near_duplicates_collapse_keeping_the_newest() {
    let (mut chain, _dir) = open();
    for text in [
        "The user prefers concise answers without preamble",
        "The user prefers concise answers without preamble.",
        "The user prefers concise answers, without any preamble",
        "The user deploys on Kubernetes clusters",
    ] {
        chain
            .append("a", ThoughtType::PreferenceUpdate, text)
            .unwrap();
    }
    let standing = chain.standing_context(&StandingContextQuery::new());
    assert_eq!(standing.entries.len(), 2, "{:#?}", standing.entries);
    assert_eq!(standing.duplicates_collapsed, 2);
    let concise = standing
        .entries
        .iter()
        .find(|e| e.content.contains("concise"))
        .unwrap();
    assert_eq!(concise.index, 2, "the newest restatement wins");
}

/// Per-type and total caps bound the result; the latest slots are exempt.
#[test]
fn test_caps_apply_per_group_and_in_total() {
    let (mut chain, _dir) = open();
    for i in 0..10 {
        chain
            .append(
                "a",
                ThoughtType::Decision,
                &format!("decision number{i} alpha{i}"),
            )
            .unwrap();
        chain
            .append("a", ThoughtType::Goal, &format!("goal number{i} bravo{i}"))
            .unwrap();
    }
    chain
        .append("a", ThoughtType::Handoff, "handoff body")
        .unwrap();
    let query = StandingContextQuery::new().with_per_type_cap(3);
    let standing = chain.standing_context(&query);
    let count = |g| standing.entries.iter().filter(|e| e.group == g).count();
    assert_eq!(count(StandingGroup::Decision), 3);
    assert_eq!(count(StandingGroup::Goal), 3);
    assert_eq!(count(StandingGroup::Handoff), 1);

    let total = chain.standing_context(&query.clone().with_total_cap(4));
    let durable = total
        .entries
        .iter()
        .filter(|e| !e.group.is_latest_slot())
        .count();
    assert_eq!(durable, 4);
    assert!(total
        .entries
        .iter()
        .any(|e| e.group == StandingGroup::Handoff));
}

/// Only the newest handoff and the newest checkpoint/summary are returned,
/// including checkpoint-role thoughts of any type.
#[test]
fn test_only_the_latest_handoff_and_checkpoint_are_returned() {
    let (mut chain, _dir) = open();
    chain
        .append("a", ThoughtType::Handoff, "old handoff")
        .unwrap();
    chain
        .append("a", ThoughtType::Handoff, "new handoff")
        .unwrap();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Summary, "old checkpoint")
                .with_role(ThoughtRole::Checkpoint),
        )
        .unwrap();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(ThoughtType::Finding, "role-only checkpoint")
                .with_role(ThoughtRole::Checkpoint),
        )
        .unwrap();
    assert_eq!(
        contents(&chain, &StandingContextQuery::new()),
        vec!["new handoff", "role-only checkpoint"]
    );
}

/// `agent_id`, `types` and `since` narrow the selection; non-standing types
/// select nothing; an explicit `types` list ignores role-only checkpoints.
#[test]
fn test_filters_agent_types_and_since() {
    let (mut chain, _dir) = open();
    chain
        .append("alpha", ThoughtType::Constraint, "alpha rule")
        .unwrap();
    let boundary = chain
        .append("beta", ThoughtType::Constraint, "beta rule")
        .unwrap()
        .timestamp;
    chain
        .append("beta", ThoughtType::Goal, "beta goal")
        .unwrap();
    chain
        .append_thought(
            "beta",
            ThoughtInput::new(ThoughtType::Finding, "snapshot").with_role(ThoughtRole::Checkpoint),
        )
        .unwrap();

    let by_agent = contents(&chain, &StandingContextQuery::new().with_agent_id("alpha"));
    assert_eq!(by_agent, vec!["alpha rule"]);
    let by_type = contents(
        &chain,
        &StandingContextQuery::new().with_types(vec![ThoughtType::Goal]),
    );
    assert_eq!(by_type, vec!["beta goal"]);
    let since = contents(&chain, &StandingContextQuery::new().with_since(boundary));
    assert!(!since.contains(&"alpha rule".to_string()));
    assert!(since.contains(&"beta rule".to_string()));
    let none =
        chain.standing_context(&StandingContextQuery::new().with_types(vec![ThoughtType::Finding]));
    assert!(none.entries.is_empty() && none.prompt.is_empty());
}

/// The prompt groups by type and each line carries index, type/role, agent, date and trust.
#[test]
fn test_prompt_format_groups_and_shows_age() {
    let (mut chain, _dir) = open();
    chain
        .append("a", ThoughtType::PreferenceUpdate, "likes tabs")
        .unwrap();
    chain
        .append("a", ThoughtType::Constraint, "no Fridays")
        .unwrap();
    let standing = chain.standing_context(&StandingContextQuery::new());
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert!(standing.prompt.starts_with("=== STANDING CONTEXT ===\n"));
    assert!(standing.prompt.ends_with("=== END STANDING CONTEXT ===\n"));
    assert!(standing.prompt.contains(&format!(
        "[#0] PreferenceUpdate/Memory (a, {today}, trust=unsigned) likes tabs"
    )));
    let constraints = standing.prompt.find("-- Constraints --").unwrap();
    let preferences = standing.prompt.find("-- Preferences --").unwrap();
    assert!(constraints < preferences, "constraints render first");
}

/// Zero caps select nothing and do not panic.
#[test]
fn test_zero_caps_select_nothing() {
    let (mut chain, _dir) = open();
    chain
        .append("a", ThoughtType::Constraint, "a rule")
        .unwrap();
    let standing = chain.standing_context(&StandingContextQuery::new().with_per_type_cap(0));
    assert!(standing.entries.is_empty());
}

const ANIMALS: [&str; 10] = [
    "quokka", "marmot", "lemur", "ocelot", "pangolin", "tapir", "wombat", "gecko", "narwhal",
    "badger",
];

/// Entries a standing context returns for two decisions holding the first `a`
/// and the first `b` animals (the duplicate threshold is 0.85).
fn decision_survivors(a: usize, b: usize) -> usize {
    let (mut chain, _dir) = open();
    for n in [a, b] {
        chain
            .append("a", ThoughtType::Decision, &ANIMALS[..n].join(" "))
            .unwrap();
    }
    chain
        .standing_context(&StandingContextQuery::new())
        .entries
        .len()
}

/// The token-set size shortcut must not change which pairs collapse: sets of 10
/// and 9 tokens have Jaccard 0.9 and collapse, 10 and 8 have 0.8 and both stay.
#[test]
fn test_size_shortcut_does_not_change_which_decisions_collapse() {
    assert_eq!(decision_survivors(10, 9), 1);
    assert_eq!(decision_survivors(10, 8), 2);
    assert_eq!(decision_survivors(10, 10), 1);
}

/// A group only considers its best-ranked thoughts (see
/// `STANDING_SCAN_WINDOW_FACTOR`), so a low-importance restatement far below
/// that window is neither tokenized nor counted, while the ranked picks are
/// the same as ever.
#[test]
fn test_thoughts_ranked_beyond_the_scan_window_are_ignored() {
    let (mut chain, _dir) = open();
    chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Use Postgres for all durable storage.",
            )
            .with_importance(0.9),
        )
        .unwrap();
    for i in 0..1100 {
        chain
            .append_thought(
                "a",
                ThoughtInput::new(
                    ThoughtType::Decision,
                    format!("Filler decision number {i} about subsystem{i}."),
                )
                .with_importance(0.5),
            )
            .unwrap();
    }
    chain
        .append_thought(
            "a",
            ThoughtInput::new(
                ThoughtType::Decision,
                "Use Postgres for all durable storage!",
            )
            .with_importance(0.1),
        )
        .unwrap();
    let query = StandingContextQuery::new().with_per_type_cap(3);
    let standing = chain.standing_context(&query);
    assert_eq!(standing.entries.len(), 3);
    assert_eq!(
        standing.entries[0].content,
        "Use Postgres for all durable storage."
    );
    assert_eq!(
        standing.duplicates_collapsed, 0,
        "the restatement ranks past the scan window and is never compared"
    );
}
