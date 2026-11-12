//! Private embedding endpoint; callers never select a provider or billing user.
use crate::{library::require, Client};
use anyhow::{anyhow, ensure, Result};
use hmac::{Hmac, Mac};
use reqwest::Method;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

/// Capture this before inference; ownership transfers apply to subsequent requests.
pub async fn owner(pool: &PgPool, workspace: Uuid) -> Result<i64> {
  sqlx::query_scalar(
    "SELECT w.owner_uid FROM af_workspace w JOIN af_user u ON u.uid=w.owner_uid JOIN auth.users a ON a.id=u.uuid \
     WHERE w.workspace_id=$1 AND w.deleted_at IS NULL AND coalesce(w.is_initialized,true) \
     AND a.deleted_at IS NULL AND (a.banned_until IS NULL OR a.banned_until<now())")
    .bind(workspace).fetch_optional(pool).await?
    .ok_or_else(|| anyhow!(crate::library::Rejected(403)))
}

pub fn prepare(proxy: &Value, authorization: &str, payload: &Value) -> Result<Value> {
  let token = proxy["token"]
    .as_str()
    .filter(|s| !s.is_empty())
    .unwrap_or("");
  // HMAC verifies fixed-size digests in constant time, including different input lengths.
  let mut expected = Hmac::<Sha256>::new_from_slice(b"PonyNotes embedding bearer").unwrap();
  expected.update(format!("Bearer {token}").as_bytes());
  let mut actual = Hmac::<Sha256>::new_from_slice(b"PonyNotes embedding bearer").unwrap();
  actual.update(authorization.as_bytes());
  require(
    !token.is_empty()
      && expected
        .verify_slice(&actual.finalize().into_bytes())
        .is_ok(),
    401,
  )?;
  let texts = match &payload["input"] {
    Value::String(s) => vec![json!(s)],
    Value::Array(values) => values.clone(),
    _ => return Err(anyhow!(crate::library::Rejected(400))),
  };
  require(
    payload.is_object()
      && payload["model"] == proxy["name"]
      && proxy["name"].is_string()
      && (1..=100).contains(&texts.len())
      && texts.iter().all(Value::is_string)
      && texts
        .iter()
        .filter_map(Value::as_str)
        .map(|s| s.chars().count())
        .sum::<usize>()
        <= 1000000,
    400,
  )?;
  let parameters = &proxy["parameters"];
  ensure!(
    parameters["dimension"].as_u64().is_some_and(|n| n > 0),
    "missing embedding dimensions"
  );
  let mut request = json!({"model":proxy["name"],"input":texts,"truncate":true,
    "options":{"num_ctx":parameters["truncate_prompt_tokens"].as_u64().filter(|n|*n>0).unwrap_or(511)}});
  if parameters["supports_dimension_override"] == true {
    request["dimensions"] = parameters["dimension"].clone();
  }
  Ok(request)
}

pub async fn infer(client: &Client, request: &Value) -> Result<Value> {
  client
    .request(
      &format!("{}/api/embed", client.ollama.trim_end_matches('/')),
      Method::POST,
      None,
      Some(request),
    )
    .await
}

pub fn tokens(response: &Value) -> Result<i64> {
  response["prompt_eval_count"]
    .as_i64()
    .filter(|n| *n >= 0)
    .ok_or_else(|| anyhow!("invalid provider token usage"))
}

// Called after recording provider usage: invalid vectors still consumed inference tokens.
pub fn response(proxy: &Value, request: &Value, response: &Value, tokens: i64) -> Result<Value> {
  let vectors = response["embeddings"]
    .as_array()
    .ok_or_else(|| anyhow!("missing embeddings"))?;
  let dimension = proxy["parameters"]["dimension"]
    .as_u64()
    .ok_or_else(|| anyhow!("missing dimensions"))? as usize;
  ensure!(
    vectors.len() == request["input"].as_array().map_or(0, Vec::len)
      && vectors.iter().all(|v| v.as_array().is_some_and(
        |v| v.len() == dimension && v.iter().all(|n| n.as_f64().is_some_and(f64::is_finite))
      )),
    "invalid provider embeddings"
  );
  let data = vectors
    .iter()
    .enumerate()
    .map(|(index, v)| json!({"object":"embedding","index":index,"embedding":v}))
    .collect::<Vec<_>>();
  Ok(
    json!({"object":"list","model":proxy["name"],"data":data,"usage":{"prompt_tokens":tokens,"total_tokens":tokens}}),
  )
}
