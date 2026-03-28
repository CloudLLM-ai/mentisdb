use mentisdb::helpers::{
    thought_checkpoint, thought_correction, thought_decision, thought_insight,
    thought_lesson_learned, thought_mistake, thought_plan, thought_retrospective,
    thought_task_complete,
};
use mentisdb::{ThoughtRole, ThoughtType};

#[test]
fn thought_decision_has_correct_type() {
    let t = thought_decision("Use binary storage.");
    assert_eq!(t.thought_type, ThoughtType::Decision);
    assert_eq!(t.role, ThoughtRole::Memory);
    assert_eq!(t.content, "Use binary storage.");
}

#[test]
fn thought_insight_has_correct_type() {
    let t = thought_insight("Hash chains prevent silent corruption.");
    assert_eq!(t.thought_type, ThoughtType::Insight);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_mistake_has_correct_type() {
    let t = thought_mistake("Ran migration on wrong directory.");
    assert_eq!(t.thought_type, ThoughtType::Mistake);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_correction_has_correct_type() {
    let t = thought_correction("Correct field name is prev_hash.");
    assert_eq!(t.thought_type, ThoughtType::Correction);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_plan_has_correct_type() {
    let t = thought_plan("Implement cross-chain search in phases.");
    assert_eq!(t.thought_type, ThoughtType::Plan);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_lesson_learned_has_correct_type() {
    let t = thought_lesson_learned("Always verify chain_key casing before migration.");
    assert_eq!(t.thought_type, ThoughtType::LessonLearned);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_retrospective_has_correct_type_and_role() {
    let t = thought_retrospective("Session complete. Migrated 3 chains.");
    assert_eq!(t.thought_type, ThoughtType::Summary);
    assert_eq!(t.role, ThoughtRole::Retrospective);
}

#[test]
fn thought_task_complete_has_correct_type() {
    let t = thought_task_complete("PR #42 merged.");
    assert_eq!(t.thought_type, ThoughtType::TaskComplete);
    assert_eq!(t.role, ThoughtRole::Memory);
}

#[test]
fn thought_checkpoint_has_correct_type_and_role() {
    let t = thought_checkpoint("Completed phases 1-2.");
    assert_eq!(t.thought_type, ThoughtType::Summary);
    assert_eq!(t.role, ThoughtRole::Checkpoint);
}

#[test]
fn helpers_return_builder_compatible_thought_input() {
    let t = thought_decision("Use binary storage.")
        .with_agent_name("test-agent")
        .with_tags(["storage", "architecture"]);
    assert_eq!(t.thought_type, ThoughtType::Decision);
    assert_eq!(t.agent_name.as_deref(), Some("test-agent"));
    assert!(t.tags.contains(&"storage".to_string()));
    assert!(t.tags.contains(&"architecture".to_string()));
}

#[test]
fn helpers_accept_string_and_str() {
    let owned = "owned string".to_string();
    let t1 = thought_insight(owned);
    let t2 = thought_insight("borrowed str");
    assert_eq!(t1.content, "owned string");
    assert_eq!(t2.content, "borrowed str");
}
