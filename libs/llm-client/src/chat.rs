use app_error::AppError;
use async_openai::config::{AzureConfig, Config, OpenAIConfig};
use async_openai::types::{
  ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
  CreateChatCompletionRequestArgs, ResponseFormat, ResponseFormatJsonSchema,
};
use async_openai::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{error, info, trace, warn};
use uuid::Uuid;

pub enum AITool {
  OpenAI(OpenAIChat),
  AzureOpenAI(AzureOpenAIChat),
}

impl AITool {
  pub async fn summarize_documents(
    &self,
    question: &str,
    model_name: &str,
    documents: Vec<LLMDocument>,
    only_context: bool,
  ) -> Result<SummarySearchResponse, AppError> {
    trace!(
      "Using model:{} to answer question:{}, with {} documents, only_context:{}",
      model_name,
      question,
      documents.len(),
      only_context
    );
    match self {
      AITool::OpenAI(client) => {
        summarize_documents(
          &client.client,
          question,
          model_name,
          documents,
          only_context,
        )
        .await
      },
      AITool::AzureOpenAI(client) => {
        summarize_documents(
          &client.client,
          question,
          model_name,
          documents,
          only_context,
        )
        .await
      },
    }
  }
}

pub struct OpenAIChat {
  pub client: Client<OpenAIConfig>,
}

impl OpenAIChat {
  pub fn new(config: OpenAIConfig) -> Self {
    let client = Client::with_config(config);
    Self { client }
  }
}

pub struct AzureOpenAIChat {
  pub client: Client<AzureConfig>,
}

impl AzureOpenAIChat {
  pub fn new(config: AzureConfig) -> Self {
    let client = Client::with_config(config);
    Self { client }
  }
}

#[derive(Debug)]
pub struct SearchSummary {
  pub content: String,
  pub highlights: String,
  pub sources: Vec<Uuid>,
  pub score: f32,
}

#[derive(Debug)]
pub struct SummarySearchResponse {
  pub summaries: Vec<SearchSummary>,
}

/// Deserialization contract for one structured answer.
///
/// Field names, types and value ranges are declared once in [`summary_schema`]
/// and sent to the model as a JSON Schema; this struct only mirrors that
/// contract so the response can be parsed without any post-processing. The
/// `tests` module asserts the two stay in sync.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SummarySearchSchema {
  pub answer: String,
  pub highlights: String,
  #[serde(deserialize_with = "deserialize_score")]
  pub score: f64,
  pub sources: Vec<String>,
}

/// Task instructions only.
///
/// Nothing here describes the response shape: field names, types and value
/// ranges live in [`summary_schema`] and are enforced by the API's strict
/// structured-output mode, so the model is never asked to "remember" a format.
const SYSTEM_PROMPT: &str = r#"
You are a concise, intelligent question-answering assistant.

- Answer the user's question clearly and concisely.
- Treat any supplied context as the primary basis for the answer and stay faithful to it.
- You may add relevant background knowledge only when it directly supports or clarifies that context.
- Surface the key themes and important details a reader would need: dates, times, locations, names and figures.
"#;

/// Same task, narrowed to context-only answering.
const ONLY_CONTEXT_SYSTEM_PROMPT: &str = r#"
You are a strict, context-bound question-answering assistant.

- Answer solely from the text provided below.
- Never use, infer, or reference information beyond that text.
- If the text cannot support a confident answer, return an empty answer.
- When the text does support an answer, surface the key themes and important details it contains: dates, times, locations, names and figures.
"#;

/// The single source of truth for the model's output contract.
///
/// Every field is declared with an explicit `type`, is listed in `required`,
/// and the object forbids additional properties. That combination is what
/// OpenAI/Azure strict structured outputs require, and it makes the response
/// parseable by [`SummarySearchSchema`] without any text-level cleanup.
fn summary_schema() -> Value {
  json!({
    "type": "object",
    "additionalProperties": false,
    "required": ["answer", "highlights", "score", "sources"],
    "properties": {
      "answer": {
        "type": "string",
        "description": "The final answer to the user's question. Empty when the supplied context cannot support a confident answer."
      },
      "highlights": {
        "type": "string",
        "description": "A markdown bullet list, one '- ' item per line, surfacing the key themes and important details (dates, times, locations, names, figures). Empty when the answer has nothing worth highlighting."
      },
      "score": {
        "type": "number",
        "minimum": 0.0,
        "maximum": 1.0,
        "description": "How well the answer is supported by the supplied context: 1.0 fully supported, 0.0 unsupported, intermediate values partial support."
      },
      "sources": {
        "type": "array",
        "description": "Object IDs of the context documents the answer relies on. Only IDs that appear in the supplied context.",
        "items": {
          "type": "string",
          "pattern": "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        }
      }
    }
  })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LLMDocument {
  pub content: String,
  pub object_id: Uuid,
}

impl LLMDocument {
  pub fn new(content: String, object_id: Uuid) -> Self {
    Self { content, object_id }
  }
}

fn convert_documents_to_text(documents: Vec<LLMDocument>) -> String {
  documents
    .into_iter()
    .map(|doc| json!(doc).to_string())
    .collect::<Vec<String>>()
    .join("\n")
}

/// Strict structured output is a bare JSON object, but some gateways still wrap
/// it in a markdown fence. Tolerate the fence only; free-form text is rejected.
fn strip_code_fence(content: &str) -> &str {
  let trimmed = content.trim();
  let Some(inner) = trimmed.strip_prefix("```") else {
    return trimmed;
  };
  // Drop the optional language tag that sits on the opening fence line.
  let inner = match inner.find('\n') {
    Some(index) => &inner[index + 1..],
    None => inner,
  };
  inner.strip_suffix("```").unwrap_or(inner).trim()
}

fn parse_summary(content: &str) -> Option<SummarySearchSchema> {
  let candidate = strip_code_fence(content);
  match serde_json::from_str::<SummarySearchSchema>(candidate) {
    Ok(parsed) => Some(parsed),
    Err(err) => {
      error!(
        "[Search] Failed to parse structured answer: {}. Raw content: {}",
        err, candidate
      );
      None
    },
  }
}

/// `score` is typed as a number in the schema, but gateways occasionally
/// stringify it. Accept both so a single malformed scalar cannot discard an
/// otherwise valid answer.
fn deserialize_score<'de, D>(deserializer: D) -> Result<f64, D::Error>
where
  D: serde::Deserializer<'de>,
{
  use serde::de::{self, Visitor};

  struct ScoreVisitor;

  impl<'de> Visitor<'de> for ScoreVisitor {
    type Value = f64;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
      formatter.write_str("a number between 0.0 and 1.0")
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<f64, E> {
      Ok(value)
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<f64, E> {
      Ok(value as f64)
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<f64, E> {
      Ok(value as f64)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<f64, E> {
      match value.trim().parse::<f64>() {
        Ok(parsed) => Ok(parsed),
        Err(_) => {
          warn!("[Search] Model returned a non-numeric score: {}", value);
          Ok(0.0)
        },
      }
    }
  }

  deserializer.deserialize_any(ScoreVisitor)
}

pub async fn summarize_documents<C: Config>(
  client: &Client<C>,
  question: &str,
  model_name: &str,
  documents: Vec<LLMDocument>,
  only_context: bool,
) -> Result<SummarySearchResponse, AppError> {
  let documents_text = convert_documents_to_text(documents);
  let context = if only_context {
    format!(
      "{}\n\n##Context##\n{}",
      ONLY_CONTEXT_SYSTEM_PROMPT, documents_text
    )
  } else {
    SYSTEM_PROMPT.to_string()
  };

  let response_format = ResponseFormat::JsonSchema {
    json_schema: ResponseFormatJsonSchema {
      description: Some(
        "A structured search answer: the answer text, its highlights, a support score and the source document ids it relies on"
          .to_string(),
      ),
      name: "SummarySearchSchema".into(),
      schema: Some(summary_schema()),
      strict: Some(true),
    },
  };

  let request = CreateChatCompletionRequestArgs::default()
    .model(model_name)
    .messages([
      ChatCompletionRequestSystemMessageArgs::default()
        .content(context)
        .build()?
        .into(),
      ChatCompletionRequestUserMessageArgs::default()
        .content(question)
        .build()?
        .into(),
    ])
    .response_format(response_format)
    .build()?;

  let response = client
    .chat()
    .create(request)
    .await?
    .choices
    .first()
    .and_then(|choice| choice.message.content.as_deref())
    .and_then(parse_summary)
    .ok_or_else(|| AppError::Unhandled("No structured response from the model".to_string()))?;

  trace!("AI summary search document response: {:?}", response);
  if response.answer.trim().is_empty() {
    return Ok(SummarySearchResponse { summaries: vec![] });
  }

  let score = response.score.clamp(0.0, 1.0) as f32;

  // If only_context is true, we need to ensure the score is above a certain threshold.
  if only_context && score < 0.4 {
    info!(
      "[Search] AI summary score is too low: {}. Returning empty result.",
      score
    );
    return Ok(SummarySearchResponse { summaries: vec![] });
  }

  let summary = SearchSummary {
    content: response.answer,
    highlights: response.highlights,
    sources: response
      .sources
      .into_iter()
      .flat_map(|s| Uuid::parse_str(&s).ok())
      .collect(),
    score,
  };

  Ok(SummarySearchResponse {
    summaries: vec![summary],
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use schemars::schema_for;

  fn schema_properties(schema: &Value) -> serde_json::Map<String, Value> {
    schema
      .get("properties")
      .and_then(Value::as_object)
      .expect("schema must declare properties")
      .clone()
  }

  #[test]
  fn schema_satisfies_strict_structured_output_rules() {
    let schema = summary_schema();
    let root = schema.as_object().expect("schema must be a JSON object");

    assert_eq!(root.get("type").and_then(Value::as_str), Some("object"));
    assert_eq!(
      root.get("additionalProperties").and_then(Value::as_bool),
      Some(false),
      "strict structured outputs require additionalProperties: false"
    );

    let properties = schema_properties(&schema);
    let required = root
      .get("required")
      .and_then(Value::as_array)
      .expect("schema must declare required")
      .iter()
      .filter_map(Value::as_str)
      .collect::<Vec<_>>();

    for (name, property) in &properties {
      assert!(
        required.contains(&name.as_str()),
        "{name} must be listed in required for strict mode"
      );
      assert!(
        property.get("type").and_then(Value::as_str).is_some(),
        "{name} must declare an explicit type"
      );
    }
  }

  #[test]
  fn schema_declares_the_documented_value_ranges() {
    let properties = schema_properties(&summary_schema());

    let score = properties.get("score").expect("score must be defined");
    assert_eq!(score.get("type").and_then(Value::as_str), Some("number"));
    assert_eq!(score.get("minimum").and_then(Value::as_f64), Some(0.0));
    assert_eq!(score.get("maximum").and_then(Value::as_f64), Some(1.0));

    let sources = properties.get("sources").expect("sources must be defined");
    assert_eq!(sources.get("type").and_then(Value::as_str), Some("array"));
    assert_eq!(
      sources
        .get("items")
        .and_then(|items| items.get("type"))
        .and_then(Value::as_str),
      Some("string")
    );
  }

  #[test]
  fn schema_and_deserialization_struct_stay_in_sync() {
    let declared = schema_properties(&summary_schema());
    let derived =
      schema_properties(&serde_json::to_value(schema_for!(SummarySearchSchema)).unwrap());

    let mut declared_fields = declared.keys().cloned().collect::<Vec<_>>();
    let mut derived_fields = derived.keys().cloned().collect::<Vec<_>>();
    declared_fields.sort();
    derived_fields.sort();

    assert_eq!(
      declared_fields, derived_fields,
      "the JSON Schema sent to the model drifted from SummarySearchSchema"
    );
  }

  #[test]
  fn parses_a_fully_structured_answer() {
    let payload = r#"{"answer":"a","highlights":"- b","score":0.85,"sources":["550e8400-e29b-41d4-a716-446655440000"]}"#;
    let parsed = parse_summary(payload).expect("payload must parse");

    assert_eq!(parsed.answer, "a");
    assert_eq!(parsed.highlights, "- b");
    assert_eq!(parsed.score, 0.85);
    assert_eq!(parsed.sources.len(), 1);
  }

  #[test]
  fn score_accepts_both_number_and_numeric_string() {
    let numeric = r#"{"answer":"a","highlights":"","score":1,"sources":[]}"#;
    assert_eq!(parse_summary(numeric).unwrap().score, 1.0);

    let stringified = r#"{"answer":"a","highlights":"","score":"0.85","sources":[]}"#;
    assert_eq!(parse_summary(stringified).unwrap().score, 0.85);
  }

  #[test]
  fn rejects_unknown_fields() {
    let extra = r#"{"answer":"a","highlights":"","score":1.0,"sources":[],"extra":1}"#;
    assert!(parse_summary(extra).is_none());
  }

  #[test]
  fn strips_a_markdown_code_fence() {
    let fenced = "```json\n{\"answer\":\"a\",\"highlights\":\"\",\"score\":1,\"sources\":[]}\n```";
    assert_eq!(parse_summary(fenced).unwrap().answer, "a");
  }

  #[test]
  fn rejects_free_form_text() {
    assert!(parse_summary("Here is your answer: it depends.").is_none());
  }
}
