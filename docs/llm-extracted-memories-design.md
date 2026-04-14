# LLM-Extracted Memories Pipeline — Design Document

**Version:** 0.9.0  
**Status:** Draft  
**Author:** MentisDB Team

---

## 1. Overview

This document describes the design for an **opt-in LLM-extracted memories pipeline** in MentisDB 0.9.0. The pipeline allows agents to pass free-form text to an LLM and store the structured memories (as `ThoughtInput` records) that the LLM produces.

### 1.1 Goals

- Allow agents to submit raw conversational text and receive structured, typed memory records
- Map LLM output to existing `ThoughtType` variants (e.g., `FactLearned`, `PreferenceUpdate`, `Decision`)
- Make LLM integration truly opt-in — no HTTP client is initialized unless this feature is used
- Provide REST API (`POST /v1/extract-memories`), MCP tool (`mentisdb_extract_memories`), and Rust API
- Preserve the existing security model: thoughts can be signed before storage

### 1.2 Non-Goals

- LLM is **not** required for core MentisDB operation (purely lexical + vector retrieval)
- LLM output is **not** trusted by default — returned thoughts are unsigned unless the caller signs them
- This feature does **not** modify the bincode storage schema for existing variants

---

## 2. API Surface

### 2.1 REST API

**Endpoint:** `POST /v1/extract-memories`

**Request:**
```json
{
  "text": "The user mentioned they prefer dark mode. Also they asked about pricing for enterprise plans.",
  "chain_key": "my-agent-brain",
  "agent_id": "agent-001",
  "prompt_template": null
}
```

**Response:**
```json
{
  "thoughts": [
    {
      "thought_type": "PreferenceUpdate",
      "role": "Memory",
      "content": "User prefers dark mode interface.",
      "importance": 0.8,
      "confidence": 0.9,
      "tags": ["ui", "preference"],
      "concepts": ["dark-mode"],
      "refs": [],
      "relations": []
    },
    {
      "thought_type": "Question",
      "role": "Memory",
      "content": "User inquired about enterprise pricing.",
      "importance": 0.6,
      "confidence": 0.95,
      "tags": ["pricing", "enterprise"],
      "concepts": ["pricing inquiry"],
      "refs": [],
      "relations": []
    }
  ],
  "model": "gpt-4",
  "usage": {
    "prompt_tokens": 487,
    "completion_tokens": 124,
    "total_tokens": 611
  }
}
```

### 2.2 MCP Tool

**Tool name:** `mentisdb_extract_memories`

**Parameters:**
| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `text` | string | Yes | Free-form text to extract memories from |
| `chain_key` | string | No | Target chain key. Defaults to server default |
| `agent_id` | string | No | Agent identity for returned thoughts |
| `prompt_template` | string | No | Custom prompt template (see Section 5) |

### 2.3 Rust API

```rust
/// Configuration for the LLM extraction pipeline.
pub struct LlmExtractionConfig {
    /// OpenAI-compatible API base URL.
    pub base_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Model identifier (e.g., "gpt-4", "claude-3-sonnet").
    pub model: String,
}

/// Result of extracting structured memories from free-form text.
pub struct ExtractionResult {
    /// Extracted thought inputs ready for append.
    pub thoughts: Vec<ThoughtInput>,
    /// Model that produced the extraction.
    pub model: String,
    /// Token usage statistics.
    pub usage: TokenUsage,
}

/// Token usage from the LLM API response.
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}
```

```rust
impl MentisDb {
    /// Extract structured memories from free-form text using an LLM.
    ///
    /// This method is truly opt-in: no HTTP client is initialized unless
    /// this method is called. The LLM is called with a prompt that asks
    /// it to extract typed memory records from the provided text.
    ///
    /// The returned `ThoughtInput` records are **not** automatically appended.
    /// Callers should review, sign, and append them manually.
    ///
    /// # Errors
    ///
    /// Returns `LlmExtractionError` if:
    /// - `OPENAI_API_KEY` is not set
    /// - LLM API call fails or returns invalid JSON
    /// - The response cannot be parsed into structured memories
    pub async fn extract_memories(
        &self,
        text: &str,
        config: &LlmExtractionConfig,
    ) -> Result<ExtractionResult, LlmExtractionError>;
}
```

---

## 3. ThoughtType Mapping

The LLM is asked to categorize extracted content into one of the following `ThoughtType` variants:

| ThoughtType | When to use |
|-------------|-------------|
| `PreferenceUpdate` | User explicitly states a preference |
| `UserTrait` | A durable characteristic about the user |
| `RelationshipUpdate` | Change in agent-user relationship |
| `Finding` | Concrete observation |
| `Insight` | Higher-level synthesis |
| `FactLearned` | Factual information learned |
| `PatternDetected` | Recurring pattern |
| `Hypothesis` | Tentative explanation |
| `Mistake` | Error in prior reasoning |
| `Correction` | Corrected version of a mistake |
| `LessonLearned` | Distilled heuristic |
| `AssumptionInvalidated` | Previously trusted assumption proven wrong |
| `Constraint` | Hard limit identified |
| `Plan` | Future work plan |
| `Subgoal` | Component of a plan |
| `Decision` | Concrete choice made |
| `StrategyShift` | Change in approach |
| `Wonder` | Open-ended curiosity |
| `Question` | Unresolved question |
| `Idea` | Design concept or direction |
| `Experiment` | Experiment proposed/executed |
| `ActionTaken` | Meaningful action performed |
| `TaskComplete` | Milestone completed |
| `Checkpoint` | Resumption checkpoint |
| `StateSnapshot` | Broader state capture |
| `Handoff` | Context handed to another actor |
| `Summary` | Summary of prior thoughts |
| `Surprise` | Unexpected outcome |
| `Reframe` | Reinterpretation without deletion |
| `Goal` | High-level objective |

The `LLMExtracted` variant is **not** assigned to extracted memories directly — instead, each memory is typed according to its semantic category. The `LLMExtracted` variant exists to allow querying for thoughts that were produced via this pipeline.

---

## 4. LLM Integration

### 4.1 Configuration

LLM configuration is read from environment variables:

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `OPENAI_API_KEY` | Yes | — | API key for the LLM provider |
| `LLM_BASE_URL` | No | `<https://api.openai.com/v1>` | OpenAI-compatible API base URL |
| `LLM_MODEL` | No | `gpt-4` | Model identifier |

### 4.2 OpenAI-Compatible API

The implementation uses the **OpenAI Chat Completions API** format:

```
POST /v1/chat/completions
```

```json
{
  "model": "gpt-4",
  "messages": [
    {
      "role": "system",
      "content": "<extraction prompt>"
    },
    {
      "role": "user",
      "content": "<input text>"
    }
  ],
  "temperature": 0.1,
  "response_format": { "type": "json_object" }
}
```

- `temperature` is set to `0.1` to reduce creativity and improve consistency
- `response_format` uses `json_object` to request structured JSON output

### 4.3 HTTP Client

- Uses `reqwest` (already in `Cargo.toml` as a conditional dependency)
- Client is initialized **lazily** — only when `extract_memories` is first called
- No connection pooling issues since each call is independent
- Timeout: 30 seconds per request

### 4.4 Error Handling

| Error | Cause | Handling |
|-------|-------|----------|
| `LlmNotConfigured` | `OPENAI_API_KEY` not set | Return error with instructions |
| `LlmApiError` | HTTP 4xx/5xx from LLM | Include error message in response |
| `LlmParseError` | LLM output is not valid JSON | Log raw output, return error |
| `LlmSchemaMismatch` | JSON parses but doesn't match expected schema | Return error with details |

---

## 5. Prompt Template

### 5.1 Default Extraction Prompt

```
You are a memory analyst. Your task is to extract structured memory records from the provided text.

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

Input text:
<user text here>
```

### 5.2 Custom Prompt Template

Callers can provide a custom prompt template via `prompt_template`. The template is a string where:

- `{{text}}` is replaced with the input text
- `{{types}}` is replaced with the comma-separated list of valid ThoughtType names

If `prompt_template` is `null`, the default prompt above is used.

### 5.3 Output Schema

The LLM **must** return a JSON object matching this schema:

```json
{
  "thoughts": [
    {
      "thought_type": "string (ThoughtType name)",
      "content": "string",
      "importance": "number (0.0-1.0)",
      "confidence": "number (0.0-1.0)",
      "tags": ["string"],
      "concepts": ["string"]
    }
  ]
}
```

---

## 6. Security Model

### 6.1 Trust Level: Untrusted

LLM output is **never** automatically trusted:

1. **No auto-append**: `extract_memories` returns `Vec<ThoughtInput>`, not `Vec<Thought>`. The caller must explicitly review and append each thought.
2. **Optional signing**: Callers can sign thoughts before appending using the existing Ed25519 signing workflow
3. **Schema validation**: All returned thoughts are validated against the `ThoughtInput` schema before being returned
4. **No privilege escalation**: The feature does not grant any additional permissions beyond what the caller already has

### 6.2 Verification Checklist

Before appending an LLM-extracted thought, callers should consider:

- [ ] Does the `content` accurately reflect the input text?
- [ ] Is the `thought_type` appropriate?
- [ ] Are `importance` and `confidence` scores reasonable?
- [ ] Are `tags` and `concepts` correct?

### 6.3 Signing Flow

```rust
// 1. Extract memories
let extraction = chain.extract_memories(text, &config).await?;

// 2. Review and sign each thought
for input in extraction.thoughts {
    // Verify content is acceptable
    if !verify_extracted_content(&input.content, &original_text) {
        continue; // skip suspicious extractions
    }
    
    // Sign the thought
    let signed = sign_thought_input(&input, &signing_key)?;
    chain.append_thought(agent_id, signed)?;
}
```

---

## 7. Implementation Plan

### 7.1 New Files

| File | Purpose |
|------|---------|
| `src/llm.rs` | LLM client, prompt templates, response parsing |
| `tests/llm_extracted_memories_tests.rs` | Unit and integration tests |

### 7.2 Modified Files

| File | Changes |
|------|---------|
| `src/lib.rs` | Add `ThoughtType::LLMExtracted` at END of enum; add `LlmExtractionConfig`, `ExtractionResult`, `TokenUsage`, `LlmExtractionError` types |
| `src/server.rs` | Add REST endpoint `POST /v1/extract-memories`; add MCP tool `mentisdb_extract_memories` |
| `Cargo.toml` | No changes needed — `reqwest` is already a conditional dependency |

### 7.3 Feature Flag

No new feature flag is required. The LLM integration is conditionally initialized only when `extract_memories` is called.

---

## 8. Example Usage

### 8.1 Rust API

```rust
use mentisdb::{MentisDb, LlmExtractionConfig};

let config = LlmExtractionConfig {
    base_url: std::env::var("LLM_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
    api_key: std::env::var("OPENAI_API_KEY")
        .expect("OPENAI_API_KEY must be set"),
    model: std::env::var("LLM_MODEL")
        .unwrap_or_else(|_| "gpt-4".to_string()),
};

let extraction = chain.extract_memories(
    "The user prefers dark mode and asked about enterprise pricing.",
    &config,
).await?;

for thought in extraction.thoughts {
    println!("Extracted: {:?}", thought.thought_type);
}
```

### 8.2 REST API

```bash
curl -X POST http://localhost:9472/v1/extract-memories \
  -H "Content-Type: application/json" \
  -d '{
    "text": "The user prefers dark mode and asked about enterprise pricing.",
    "agent_id": "agent-001"
  }'
```

### 8.3 MCP Tool

```json
{
  "tool": "mentisdb_extract_memories",
  "parameters": {
    "text": "The user prefers dark mode and asked about enterprise pricing.",
    "agent_id": "agent-001"
  }
}
```

---

## 9. Error Handling Details

### 9.1 LlmExtractionError Enum

```rust
pub enum LlmExtractionError {
    /// LLM configuration is missing or invalid.
    NotConfigured(String),
    /// LLM API returned an error.
    ApiError { status: u16, message: String },
    /// LLM output could not be parsed as JSON.
    ParseError(String),
    /// LLM output JSON doesn't match expected schema.
    SchemaMismatch(String),
    /// Network or I/O error.
    IoError(io::Error),
}
```

### 9.2 Error Messages

| Variant | Message | Example |
|--------|---------|---------|
| `NotConfigured` | "OPENAI_API_KEY is not set" | — |
| `ApiError` | "LLM API error: {status} {message}" | "LLM API error: 401 Unauthorized" |
| `ParseError` | "Failed to parse LLM response as JSON" | — |
| `SchemaMismatch` | "LLM response missing required field: {field}" | "LLM response missing required field: thought_type" |

---

## 10. Testing Strategy

### 10.1 Unit Tests

- Prompt template formatting
- Response parsing (valid and invalid JSON)
- Schema validation
- Error handling paths

### 10.2 Integration Tests

- Mock HTTP server responses
- End-to-end extraction with mocked LLM
- Signing and appending extracted thoughts

### 10.3 Test Data

```rust
const SAMPLE_TEXT: &str = "The user mentioned they prefer dark mode. \
They also said pricing for enterprise is a concern. \
When I suggested a demo, they seemed interested.";

const EXPECTED_EXTRACTION: &str = r#"{
  "thoughts": [
    {
      "thought_type": "PreferenceUpdate",
      "content": "User prefers dark mode interface.",
      "importance": 0.8,
      "confidence": 0.95,
      "tags": ["ui", "preference"],
      "concepts": ["dark-mode"]
    },
    {
      "thought_type": "Question",
      "content": "User inquired about enterprise pricing.",
      "importance": 0.7,
      "confidence": 0.9,
      "tags": ["pricing", "enterprise"],
      "concepts": ["pricing inquiry"]
    },
    {
      "thought_type": "Interest",
      "content": "User seemed interested in a demo.",
      "importance": 0.5,
      "confidence": 0.7,
      "tags": ["demo", "interest"],
      "concepts": ["demo interest"]
    }
  ]
}"#;
```

---

## 11. Performance Considerations

### 11.1 Latency

- LLM API call adds ~200-2000ms latency depending on model and text length
- No caching is implemented (each call hits the LLM)
- Concurrent calls are supported but should be rate-limited by the caller

### 11.2 Token Usage

- Prompt tokens: ~200-500 tokens (fixed prompt overhead)
- Completion tokens: varies based on number of extractions
- Estimated cost: $0.01-$0.10 per extraction (depending on model)

### 11.3 Rate Limiting

- Callers should implement their own rate limiting
- MentisDB does not enforce rate limits
- Consider caching frequent extractions if the same text is processed multiple times

---

## 12. Appendix: OpenAPI Schema

```yaml
ExtractMemoriesRequest:
  type: object
  required:
    - text
  properties:
    text:
      type: string
      description: Free-form text to extract memories from
    chain_key:
      type: string
      description: Target chain key (defaults to server default)
    agent_id:
      type: string
      description: Agent identity for returned thoughts
    prompt_template:
      type: string
      nullable: true
      description: Custom prompt template (see Section 5.2)

ExtractMemoriesResponse:
  type: object
  properties:
    thoughts:
      type: array
      items:
        $ref: '#/components/schemas/ThoughtInput'
    model:
      type: string
    usage:
      $ref: '#/components/schemas/TokenUsage'

TokenUsage:
  type: object
  properties:
    prompt_tokens:
      type: integer
    completion_tokens:
      type: integer
    total_tokens:
      type: integer
```
