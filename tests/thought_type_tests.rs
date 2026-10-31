//! Parsing and formatting tests for [`ThoughtType`].
//!
//! These lock the single canonical string mapping that replaced three
//! hand-maintained copies in `server.rs`, `llm.rs`, and `lib.rs` (GitHub issue
//! #17), and cover the `Goal`/`LLMExtracted` variants the server previously
//! rejected.

use std::str::FromStr;

use mentisdb::ThoughtType;

/// Every variant must round-trip through its canonical name in both directions,
/// and the display form must equal the canonical name.
#[test]
fn every_variant_round_trips() {
    assert_eq!(ThoughtType::ALL.len(), 32);
    for thought_type in ThoughtType::ALL {
        let name = thought_type.as_str();
        assert_eq!(
            ThoughtType::from_str(name).unwrap(),
            thought_type,
            "from_str round trip failed for {name}"
        );
        assert_eq!(name.parse::<ThoughtType>().unwrap(), thought_type);
        assert_eq!(thought_type.to_string(), name);
    }
}

/// Parsing ignores case and separators so shell-friendly spellings work.
#[test]
fn from_str_is_case_and_separator_insensitive() {
    for raw in [
        "FactLearned",
        "factlearned",
        "fact-learned",
        "fact_learned",
        "fact learned",
        "  FACT-LEARNED  ",
    ] {
        assert_eq!(
            raw.parse::<ThoughtType>().unwrap(),
            ThoughtType::FactLearned,
            "unexpected parse for {raw:?}"
        );
    }
}

/// The error carries the original input and lists the valid types so callers
/// can show an actionable message.
#[test]
fn from_str_error_reports_input_and_valid_types() {
    let error = "note".parse::<ThoughtType>().unwrap_err();
    assert_eq!(error.input(), "note");
    let message = error.to_string();
    assert!(message.contains("Unknown ThoughtType 'note'"));
    assert!(message.contains("FactLearned"));
    assert!(message.contains("LLMExtracted"));
}

/// Regression: the server's old table silently omitted `Goal` and
/// `LLMExtracted` even though the enum defines them.
#[test]
fn goal_and_llm_extracted_are_parseable() {
    assert!("Goal".parse::<ThoughtType>().unwrap() == ThoughtType::Goal);
    assert!("llm-extracted".parse::<ThoughtType>().unwrap() == ThoughtType::LLMExtracted);
    assert!(ThoughtType::ALL.contains(&ThoughtType::Goal));
    assert!(ThoughtType::ALL.contains(&ThoughtType::LLMExtracted));
}
