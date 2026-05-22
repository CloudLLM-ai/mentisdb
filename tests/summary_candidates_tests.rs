use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mentisdb::search::SummaryBuildConfig;
use mentisdb::{
    MentisDb, ThoughtInput, ThoughtQuery, ThoughtRelation, ThoughtRelationKind, ThoughtType,
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_chain_dir() -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "mentisdb_summary_candidates_test_{}_{}",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn summary_candidates_maps_committed_thoughts() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "summary-candidates-basic").unwrap();
    for index in 0..3 {
        chain
            .append_thought(
                "agent-a",
                ThoughtInput::new(ThoughtType::Insight, format!("Memory {index}"))
                    .with_entity_type("ProjectNote"),
            )
            .unwrap();
    }

    let candidates = chain.summary_candidates(SummaryBuildConfig {
        window_size: 2,
        overlap: 0,
        by_session: false,
        by_agent: true,
        by_entity_type: true,
    });

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].source_indices, vec![0, 1]);
    assert_eq!(candidates[1].source_indices, vec![2]);
    assert_eq!(candidates[0].group.agent_id.as_deref(), Some("agent-a"));
    assert_eq!(
        candidates[0].group.entity_type.as_deref(),
        Some("ProjectNote")
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn summary_candidates_respect_query_filter_and_summarizes_coverage() {
    let dir = unique_chain_dir();
    let mut chain = MentisDb::open_with_key(&dir, "summary-candidates-filtered").unwrap();
    let covered = chain
        .append_thought(
            "agent-a",
            ThoughtInput::new(ThoughtType::Decision, "Project alpha rollout decision."),
        )
        .unwrap()
        .id;
    chain
        .append_thought(
            "agent-a",
            ThoughtInput::new(ThoughtType::Decision, "Project beta rollout decision."),
        )
        .unwrap();
    chain
        .append_thought(
            "agent-a",
            ThoughtInput::new(ThoughtType::Insight, "Unrelated accounting note."),
        )
        .unwrap();
    chain
        .append_thought(
            "agent-a",
            ThoughtInput::new(ThoughtType::Summary, "Existing summary for alpha.").with_relations(
                vec![ThoughtRelation::new(
                    ThoughtRelationKind::Summarizes,
                    covered,
                )],
            ),
        )
        .unwrap();

    let candidates = chain.summary_candidates_matching(
        &ThoughtQuery::new()
            .with_text("project")
            .with_types(vec![ThoughtType::Decision]),
        SummaryBuildConfig {
            window_size: 1,
            overlap: 0,
            by_session: false,
            by_agent: false,
            by_entity_type: false,
        },
    );

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].source_indices, vec![1]);

    let _ = std::fs::remove_dir_all(&dir);
}
