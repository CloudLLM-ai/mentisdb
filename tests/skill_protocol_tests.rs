//! Pins the session protocol the embedded operating skill teaches: durable
//! memory is loaded at startup and searched before answering.

const SKILL: &str = include_str!("../MENTISDB_SKILL.md");

/// Startup loads standing context before recent context, and the skill
/// defines read-side triggers symmetrical to the write triggers.
#[test]
fn test_skill_loads_standing_context_before_recent_and_defines_recall_triggers() {
    let standing = SKILL
        .find("`mentisdb_standing_context` → who the user is")
        .unwrap();
    let recent = SKILL
        .find("`mentisdb_recent_context` → where you left off")
        .unwrap();
    assert!(standing < recent, "standing context loads first");
    assert!(SKILL.contains("## ✍️ WRITE TRIGGERS"));
    assert!(SKILL.contains("## 🧠 RECALL TRIGGERS (READ BEFORE YOU ANSWER)"));
    assert!(SKILL.contains("include_invalidated=true"));
}

/// The skill is loaded into every session, so keep it small.
#[test]
fn test_skill_stays_under_its_size_budget() {
    assert!(SKILL.len() < 18_000, "skill grew to {} bytes", SKILL.len());
}
