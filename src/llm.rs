//! LLM integration for the opt-in memory extraction pipeline.
//!
//! This module provides an OpenAI-compatible HTTP client that transforms
//! free-form agent text into structured [`ThoughtInput`] records. The client
//! is lazily initialized — no HTTP stacks are spun up unless
//! [`extract_memories`](crate::MentisDb::extract_memories) is called.
//!
//! # Security
//!
//! LLM output is **untrusted**. The returned [`ExtractionResult`] contains
//! [`ThoughtInput`] records that callers must review, validate, and
//! optionally sign before appending to a chain.

use crate::{
    ExtractionResult, LlmExtractionConfig, LlmExtractionError, ThoughtInput, ThoughtRole,
    ThoughtType,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default base URL for OpenAI-compatible APIs.
const DEFAULT_LLM_BASE_URL: &str = "https://api.openai.com/v1";
/// Default model when `LLM_MODEL` is not set.
const DEFAULT_LLM_MODEL: &str = "gpt-4";
/// Request timeout for LLM API calls.
const LLM_REQUEST_TIMEOUT_SECS: u64 = 30;
/// Temperature setting for extraction prompts (low = more consistent).
const EXTRACTION_TEMPERATURE: f32 = 0.1;

/// The extraction prompt sent to the LLM.
const EXTRACTION_PROMPT: &str = r#"You are a memory analyst. Your task is to extract structured memory records from the provided text.

For each distinct piece of information, emit a JSON object with these fields:
- thought_type: one of the valid ThoughtType names listed below
- content: a concise, factual memory statement (1-2 sentences max)
- importance: a float 0.0-1.0 indicating memory significance
- confidence: a float 0.0-1.0 indicating extraction certainty
- tags: array of lowercase string tags
- concepts: array of lowercase concept labels

Valid ThoughtType names:
PreferenceUpdate, UserTrait, RelationshipUpdate, Finding, Insight, FactLearned, PatternDetected,
Hypothesis, Mistake, Correction, LessonLearned, AssumptionInvalidated, Constraint, Plan,
Subgoal, Decision, StrategyShift, Wonder, Question, Idea, Experiment, ActionTaken,
TaskComplete, Checkpoint, StateSnapshot, Handoff, Summary, Surprise, Reframe, Goal

Rules:
1. Each memory must be independently meaningful — no cross-refs within the batch
2. Be conservative: only extract what is explicitly stated or strongly implied
3. Use the most specific ThoughtType that applies
4. Tags should be lowercase and concise (noun form preferred)
5. Return a JSON object with a "thoughts" array containing all extracted memories
6. If no memories can be extracted, return {"thoughts": []}

Input text:"#;

/// Token usage returned by the LLM API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTokenUsage {
    #[serde(rename = "prompt_tokens")]
    prompt_tokens: u32,
    #[serde(rename = "completion_tokens")]
    completion_tokens: u32,
    #[serde(rename = "total_tokens")]
    total_tokens: u32,
}

impl From<ApiTokenUsage> for crate::TokenUsage {
    fn from(api: ApiTokenUsage) -> Self {
        Self {
            prompt_tokens: api.prompt_tokens,
            completion_tokens: api.completion_tokens,
            total_tokens: api.total_tokens,
        }
    }
}

/// API response shape from OpenAI-compatible `/v1/chat/completions` endpoints.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
    usage: ApiTokenUsage,
    model: String,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatMessageResponse,
}

#[derive(Debug, Deserialize)]
struct ChatMessageResponse {
    content: String,
}

/// Request shape sent to OpenAI-compatible `/v1/chat/completions` endpoints.
#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: [ChatMessageRequest<'a>; 2],
    temperature: f32,
    #[serde(rename = "response_format")]
    response_format: ResponseFormat<'a>,
}

#[derive(Debug, Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    r#type: &'a str,
}

#[derive(Debug, Serialize)]
struct ChatMessageRequest<'a> {
    role: &'a str,
    content: &'a str,
}

/// JSON shape extracted from the LLM response.
#[derive(Debug, Deserialize)]
struct ExtractedThoughts {
    thoughts: Vec<RawExtractedThought>,
}

#[derive(Debug, Deserialize)]
struct RawExtractedThought {
    #[serde(rename = "thought_type")]
    thought_type: String,
    content: String,
    importance: Option<f32>,
    confidence: Option<f32>,
    tags: Option<Vec<String>>,
    concepts: Option<Vec<String>>,
}

/// Build the system+user message pair for a chat completion request.
fn build_messages(
    user_text: &str,
    custom_prompt: Option<&str>,
) -> [ChatMessageRequest<'static>; 2] {
    let user_content = if let Some(template) = custom_prompt {
        template
            .replace("{{types}}", "PreferenceUpdate, UserTrait, RelationshipUpdate, Finding, Insight, FactLearned, PatternDetected, Hypothesis, Mistake, Correction, LessonLearned, AssumptionInvalidated, Constraint, Plan, Subgoal, Decision, StrategyShift, Wonder, Question, Idea, Experiment, ActionTaken, TaskComplete, Checkpoint, StateSnapshot, Handoff, Summary, Surprise, Reframe, Goal")
            .replace("{{text}}", user_text)
    } else {
        format!("{}\n\n{}", EXTRACTION_PROMPT.trim(), user_text)
    };

    [
        ChatMessageRequest {
            role: "system",
            content: "You are a helpful memory analyst.",
        },
        ChatMessageRequest {
            role: "user",
            content: Box::leak(user_content.into_boxed_str()),
        },
    ]
}

/// Extract structured memories from free-form text using an LLM.
///
/// This is the underlying HTTP call used by [`crate::MentisDb::extract_memories`].
/// It is exposed here so that callers who want to use the LLM integration
/// without going through the `MentisDb` wrapper can do so.
pub async fn extract_memories_from_text(
    text: &str,
    config: &LlmExtractionConfig,
    prompt_template: Option<&str>,
) -> Result<ExtractionResult, LlmExtractionError> {
    if config.api_key.is_empty() {
        return Err(LlmExtractionError::NotConfigured(
            "OPENAI_API_KEY is not set".to_string(),
        ));
    }

    let base_url = if config.base_url.is_empty() {
        DEFAULT_LLM_BASE_URL.to_string()
    } else {
        config.base_url.trim_end_matches('/').to_string()
    };

    let model = if config.model.is_empty() {
        DEFAULT_LLM_MODEL.to_string()
    } else {
        config.model.clone()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(LLM_REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| LlmExtractionError::IoError(std::io::Error::other(e.to_string())))?;

    let messages = build_messages(text, prompt_template);

    let request_body = ChatCompletionRequest {
        model: &model,
        messages,
        temperature: EXTRACTION_TEMPERATURE,
        response_format: ResponseFormat {
            r#type: "json_object",
        },
    };

    let response = client
        .post(format!("{}/chat/completions", base_url))
        .header("Authorization", format!("Bearer {}", config.api_key))
        .header("Content-Type", "application/json")
        .json(&request_body)
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                LlmExtractionError::NotConfigured(format!(
                    "Failed to connect to LLM API at {}: {}",
                    base_url, e
                ))
            } else {
                LlmExtractionError::ApiError {
                    status: 0,
                    message: e.to_string(),
                }
            }
        })?;

    let status = response.status();

    if !status.is_success() {
        let message = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(LlmExtractionError::ApiError {
            status: status.as_u16(),
            message,
        });
    }

    let api_response: ChatCompletionResponse = response.json().await.map_err(|e| {
        LlmExtractionError::ParseError(format!("Failed to parse LLM response: {}", e))
    })?;

    let raw_content = api_response
        .choices
        .first()
        .ok_or_else(|| LlmExtractionError::ParseError("Empty response from LLM".to_string()))?
        .message
        .content
        .trim();

    let extracted: ExtractedThoughts = serde_json::from_str(raw_content).map_err(|e| {
        LlmExtractionError::ParseError(format!(
            "LLM output is not valid JSON: {}\nRaw output: {}",
            e, raw_content
        ))
    })?;

    let thoughts = validate_and_transform_thoughts(extracted.thoughts)?;

    Ok(ExtractionResult {
        thoughts,
        model: api_response.model,
        usage: api_response.usage.into(),
    })
}

/// Validate extracted thought raw JSON and transform into [`ThoughtInput`] records.
///
/// Returns an error if any thought has a missing required field or invalid values.
fn validate_and_transform_thoughts(
    raw_thoughts: Vec<RawExtractedThought>,
) -> Result<Vec<ThoughtInput>, LlmExtractionError> {

    let mut thoughts = Vec::with_capacity(raw_thoughts.len());

    for (i, raw) in raw_thoughts.into_iter().enumerate() {
        let thought_type = parse_thought_type(&raw.thought_type).map_err(|e| {
            LlmExtractionError::SchemaMismatch(format!(
                "Invalid thought_type '{}' at index {}: {}",
                raw.thought_type, i, e
            ))
        })?;

        let content = raw.content.trim();
        if content.is_empty() {
            return Err(LlmExtractionError::SchemaMismatch(format!(
                "Empty content at index {}",
                i
            )));
        }

        let importance = raw.importance.unwrap_or(0.5).clamp(0.0, 1.0);
        let confidence = raw.confidence.unwrap_or(0.5).clamp(0.0, 1.0);
        let tags = raw.tags.unwrap_or_default();
        let concepts = raw.concepts.unwrap_or_default();

        thoughts.push(ThoughtInput {
            session_id: None,
            agent_name: None,
            agent_owner: None,
            signing_key_id: None,
            thought_signature: None,
            thought_type,
            role: ThoughtRole::Memory,
            content: content.to_string(),
            confidence: Some(confidence),
            importance,
            tags,
            concepts,
            refs: Vec::new(),
            relations: Vec::new(),
            entity_type: None,
            source_episode: None,
        });
    }

    Ok(thoughts)
}

/// Parse a ThoughtType from its string name.
fn parse_thought_type(name: &str) -> Result<ThoughtType, String> {
    match name.trim() {
        "PreferenceUpdate" => Ok(ThoughtType::PreferenceUpdate),
        "UserTrait" => Ok(ThoughtType::UserTrait),
        "RelationshipUpdate" => Ok(ThoughtType::RelationshipUpdate),
        "Finding" => Ok(ThoughtType::Finding),
        "Insight" => Ok(ThoughtType::Insight),
        "FactLearned" => Ok(ThoughtType::FactLearned),
        "PatternDetected" => Ok(ThoughtType::PatternDetected),
        "Hypothesis" => Ok(ThoughtType::Hypothesis),
        "Mistake" => Ok(ThoughtType::Mistake),
        "Correction" => Ok(ThoughtType::Correction),
        "LessonLearned" => Ok(ThoughtType::LessonLearned),
        "AssumptionInvalidated" => Ok(ThoughtType::AssumptionInvalidated),
        "Constraint" => Ok(ThoughtType::Constraint),
        "Plan" => Ok(ThoughtType::Plan),
        "Subgoal" => Ok(ThoughtType::Subgoal),
        "Decision" => Ok(ThoughtType::Decision),
        "StrategyShift" => Ok(ThoughtType::StrategyShift),
        "Wonder" => Ok(ThoughtType::Wonder),
        "Question" => Ok(ThoughtType::Question),
        "Idea" => Ok(ThoughtType::Idea),
        "Experiment" => Ok(ThoughtType::Experiment),
        "ActionTaken" => Ok(ThoughtType::ActionTaken),
        "TaskComplete" => Ok(ThoughtType::TaskComplete),
        "Checkpoint" => Ok(ThoughtType::Checkpoint),
        "StateSnapshot" => Ok(ThoughtType::StateSnapshot),
        "Handoff" => Ok(ThoughtType::Handoff),
        "Summary" => Ok(ThoughtType::Summary),
        "Surprise" => Ok(ThoughtType::Surprise),
        "Reframe" => Ok(ThoughtType::Reframe),
        "Goal" => Ok(ThoughtType::Goal),
        "LLMExtracted" => Ok(ThoughtType::LLMExtracted),
        other => Err(format!("Unknown ThoughtType '{}'", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_messages_default_prompt() {
        let messages = build_messages("User likes coffee.", None);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content.contains("User likes coffee."));
    }

    #[test]
    fn test_build_messages_custom_prompt() {
        let template = "Analyze this text: {{text}}. Types: {{types}}";
        let messages = build_messages("User likes coffee.", Some(template));
        assert!(messages[1].content.contains("Analyze this text:"));
        assert!(messages[1].content.contains("User likes coffee."));
        assert!(messages[1].content.contains("Types:"));
    }

    #[test]
    fn test_parse_thought_type_valid() {
        assert!(parse_thought_type("Decision").is_ok());
        assert!(parse_thought_type(" PreferenceUpdate ").is_ok());
    }

    #[test]
    fn test_parse_thought_type_invalid() {
        assert!(parse_thought_type("InvalidType").is_err());
        assert!(parse_thought_type("").is_err());
    }

    #[test]
    fn test_validate_and_transform_empty() {
        let result = validate_and_transform_thoughts(vec![]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_validate_and_transform_valid() {
    
        let raw_thoughts = vec![RawExtractedThought {
            thought_type: "Decision".to_string(),
            content: "User prefers dark mode.".to_string(),
            importance: Some(0.8),
            confidence: Some(0.9),
            tags: Some(vec!["ui".to_string()]),
            concepts: Some(vec!["dark-mode".to_string()]),
        }];

        let result = validate_and_transform_thoughts(raw_thoughts).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].thought_type, ThoughtType::Decision);
        assert_eq!(result[0].importance, 0.8);
        assert_eq!(result[0].confidence, Some(0.9));
    }

    #[test]
    fn test_validate_and_transform_invalid_type() {
        let raw_thoughts = vec![RawExtractedThought {
            thought_type: "NotARealType".to_string(),
            content: "Something.".to_string(),
            importance: None,
            confidence: None,
            tags: None,
            concepts: None,
        }];

        assert!(validate_and_transform_thoughts(raw_thoughts).is_err());
    }

    #[test]
    fn test_validate_and_transform_empty_content() {
        let raw_thoughts = vec![RawExtractedThought {
            thought_type: "Decision".to_string(),
            content: "   ".to_string(),
            importance: None,
            confidence: None,
            tags: None,
            concepts: None,
        }];

        assert!(validate_and_transform_thoughts(raw_thoughts).is_err());
    }

    #[test]
    fn test_importance_clamping() {
        let raw_thoughts = vec![
            RawExtractedThought {
                thought_type: "Finding".to_string(),
                content: "Test 1.".to_string(),
                importance: Some(1.5), // Should be clamped to 1.0
                confidence: None,
                tags: None,
                concepts: None,
            },
            RawExtractedThought {
                thought_type: "Finding".to_string(),
                content: "Test 2.".to_string(),
                importance: Some(-0.5), // Should be clamped to 0.0
                confidence: None,
                tags: None,
                concepts: None,
            },
        ];

        let result = validate_and_transform_thoughts(raw_thoughts).unwrap();
        assert_eq!(result[0].importance, 1.0);
        assert_eq!(result[1].importance, 0.0);
    }
}
