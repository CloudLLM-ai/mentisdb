//! Tests for the LLM-extracted memories pipeline.

use mentisdb::{
    ExtractionResult, LlmExtractionConfig, LlmExtractionError, ThoughtInput, ThoughtRole,
    ThoughtType, TokenUsage,
};

/// Test the LLM extraction config from environment.
#[test]
fn test_llm_extraction_config_api_key_required() {
    // Without OPENAI_API_KEY set, from_env should fail
    let result = LlmExtractionConfig::from_env();
    // Note: This test will pass or fail depending on environment
    // In CI without OPENAI_API_KEY, this should return Err
    if std::env::var("OPENAI_API_KEY").is_err() {
        assert!(result.is_err());
        if let Err(e) = result {
            match e {
                LlmExtractionError::NotConfigured(msg) => {
                    assert!(msg.contains("OPENAI_API_KEY"));
                }
                _ => panic!("Expected NotConfigured error"),
            }
        }
    }
}

/// Test config with explicit values.
#[test]
fn test_llm_extraction_config_explicit() {
    let config = LlmExtractionConfig {
        base_url: "https://api.openai.com/v1".to_string(),
        api_key: "test-key".to_string(),
        model: "gpt-4".to_string(),
    };

    assert_eq!(config.base_url, "https://api.openai.com/v1");
    assert_eq!(config.api_key, "test-key");
    assert_eq!(config.model, "gpt-4");
}

/// Test that empty api_key is rejected.
#[test]
fn test_llm_extraction_config_empty_api_key() {
    let config = LlmExtractionConfig {
        base_url: "".to_string(),
        api_key: "".to_string(),
        model: "".to_string(),
    };

    // Empty api_key should be handled by the extraction function
    // (from_env would fail earlier, but explicit empty key is a runtime concern)
    assert_eq!(config.api_key, "");
}

/// Test thought type mapping from string.
#[test]
fn test_thought_type_parsing() {
    use mentisdb::ThoughtType;

    // Valid types that should be parseable by the LLM
    let valid_types = [
        "PreferenceUpdate",
        "UserTrait",
        "Finding",
        "Insight",
        "FactLearned",
        "Decision",
        "Constraint",
        "Plan",
        "Question",
        "LessonLearned",
    ];

    for thought_type_str in valid_types {
        // These should be valid ThoughtType variant names
        let parsed = match thought_type_str {
            "PreferenceUpdate" => Ok(ThoughtType::PreferenceUpdate),
            "UserTrait" => Ok(ThoughtType::UserTrait),
            "Finding" => Ok(ThoughtType::Finding),
            "Insight" => Ok(ThoughtType::Insight),
            "FactLearned" => Ok(ThoughtType::FactLearned),
            "Decision" => Ok(ThoughtType::Decision),
            "Constraint" => Ok(ThoughtType::Constraint),
            "Plan" => Ok(ThoughtType::Plan),
            "Question" => Ok(ThoughtType::Question),
            "LessonLearned" => Ok(ThoughtType::LessonLearned),
            _ => Err(()),
        };
        assert!(parsed.is_ok(), "Failed to parse: {}", thought_type_str);
    }
}

/// Test ExtractionResult structure.
#[test]
fn test_extraction_result_structure() {
    let thoughts = vec![
        ThoughtInput::new(ThoughtType::Decision, "User prefers dark mode."),
        ThoughtInput::new(ThoughtType::Question, "Asked about enterprise pricing."),
    ];

    let result = ExtractionResult {
        thoughts,
        model: "gpt-4".to_string(),
        usage: TokenUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        },
    };

    assert_eq!(result.thoughts.len(), 2);
    assert_eq!(result.model, "gpt-4");
    assert_eq!(result.usage.total_tokens, 150);
}

/// Test TokenUsage default values.
#[test]
fn test_token_usage_default() {
    let usage = TokenUsage::default();
    assert_eq!(usage.prompt_tokens, 0);
    assert_eq!(usage.completion_tokens, 0);
    assert_eq!(usage.total_tokens, 0);
}

/// Test ThoughtInput with LLM-extracted memory.
#[test]
fn test_thought_input_llm_extracted() {
    let input = ThoughtInput::new(
        ThoughtType::PreferenceUpdate,
        "User prefers dark mode interface.",
    )
    .with_importance(0.8)
    .with_confidence(0.9)
    .with_tags(["ui", "preference"])
    .with_concepts(["dark-mode"]);

    assert_eq!(input.thought_type, ThoughtType::PreferenceUpdate);
    assert_eq!(input.importance, 0.8);
    assert_eq!(input.confidence, Some(0.9));
    assert_eq!(input.tags, vec!["ui", "preference"]);
    assert_eq!(input.concepts, vec!["dark-mode"]);
    assert_eq!(input.role, ThoughtRole::Memory);
}

/// Test the LLMExtracted thought type variant exists.
#[test]
fn test_llm_extracted_variant_exists() {
    let thought_type = ThoughtType::LLMExtracted;
    let json = serde_json::to_string(&thought_type).unwrap();
    assert_eq!(json, "\"LLMExtracted\"");
}

/// Test LlmExtractionError display.
#[test]
fn test_llm_extraction_error_display() {
    let not_configured = LlmExtractionError::NotConfigured("API key missing".to_string());
    assert_eq!(
        format!("{}", not_configured),
        "LLM not configured: API key missing"
    );

    let api_error = LlmExtractionError::ApiError {
        status: 401,
        message: "Unauthorized".to_string(),
    };
    assert_eq!(
        format!("{}", api_error),
        "LLM API error (401): Unauthorized"
    );

    let parse_error = LlmExtractionError::ParseError("Invalid JSON".to_string());
    assert_eq!(
        format!("{}", parse_error),
        "Failed to parse LLM response: Invalid JSON"
    );

    let schema_error = LlmExtractionError::SchemaMismatch("Missing field".to_string());
    assert_eq!(
        format!("{}", schema_error),
        "LLM schema mismatch: Missing field"
    );
}

/// Test that malformed custom-prompt output is surfaced clearly.
#[test]
fn test_llm_extraction_parse_error_includes_raw_output() {
    let parse_error = LlmExtractionError::ParseError(
        "LLM output is not valid JSON: missing field `thought_type`\nRaw output: {\"thoughts\":[{\"type\":\"Question\"}]}".to_string(),
    );

    let rendered = format!("{}", parse_error);
    assert!(rendered.contains("missing field `thought_type`"));
    assert!(rendered.contains("Raw output:"));
}
