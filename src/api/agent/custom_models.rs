//! User-owned model credentials shared only with their original workspace.
use crate::{biz::authentication::jwt::Authorization, state::AppState};
use actix_web::{error, web, HttpResponse, Scope};
use aes_gcm::{
  aead::{Aead, KeyInit, Payload},
  Aes256Gcm, Nonce,
};
use hmac::{Hmac, Mac};
use rand::RngCore;
use secrecy::ExposeSecret;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::Row;
use uuid::Uuid;

pub fn scope() -> Scope {
  web::scope("/custom-models/{workspace_id}")
    .app_data(web::JsonConfig::default().limit(32768))
    .route("", web::get().to(list))
    .route("", web::post().to(create))
    .route("/{id}", web::put().to(update))
    .route("/{id}", web::delete().to(delete))
}

fn unavailable(_: impl std::fmt::Display) -> actix_web::Error {
  error::ErrorServiceUnavailable("自定义模型配置暂不可用，请稍后重试")
}

async fn can_manage(
  auth: &Authorization,
  state: &AppState,
  workspace: Uuid,
) -> actix_web::Result<bool> {
  let uid = super::conversations::workspace_user(auth, workspace, state).await?;
  let owner: bool =
    sqlx::query_scalar("SELECT owner_uid=$2 OR EXISTS(SELECT 1 FROM af_workspace_member WHERE workspace_id=$1 AND uid=$2 AND role_id=1) FROM af_workspace WHERE workspace_id=$1")
      .bind(workspace)
      .bind(uid)
      .fetch_one(&state.pg_pool)
      .await
      .map_err(unavailable)?;
  Ok(owner || auth.claims.role == "supabase_admin")
}

fn allowed(creator: i64, user: i64, origin: Uuid, workspace: Uuid) -> bool {
  creator == user || origin == workspace
}

fn can_reuse_credential(previous: &Value, config: &Value) -> bool {
  ["base_url", "provider"]
    .iter()
    .all(|field| previous["parameters"][field] == config["parameters"][field])
}

fn cipher(secret: &str) -> Aes256Gcm {
  let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC key");
  mac.update(b"ponynotes.agent.custom-models.v1");
  Aes256Gcm::new(&mac.finalize().into_bytes())
}

fn encrypt(secret: &str, id: Uuid, key: &str) -> actix_web::Result<Vec<u8>> {
  let mut nonce = [0u8; 12];
  rand::rngs::OsRng.fill_bytes(&mut nonce);
  let mut encrypted = cipher(secret)
    .encrypt(
      Nonce::from_slice(&nonce),
      Payload {
        msg: key.as_bytes(),
        aad: id.as_bytes(),
      },
    )
    .map_err(unavailable)?;
  let mut result = nonce.to_vec();
  result.append(&mut encrypted);
  Ok(result)
}

fn decrypt(secret: &str, id: Uuid, value: &[u8]) -> actix_web::Result<String> {
  if value.len() < 28 {
    return Err(unavailable("invalid credential"));
  }
  let bytes = cipher(secret)
    .decrypt(
      Nonce::from_slice(&value[..12]),
      Payload {
        msg: &value[12..],
        aad: id.as_bytes(),
      },
    )
    .map_err(unavailable)?;
  String::from_utf8(bytes).map_err(unavailable)
}

fn validate(input: &Value) -> actix_web::Result<Value> {
  let name = input["name"].as_str().unwrap_or("").trim();
  let url = reqwest::Url::parse(input["parameters"]["base_url"].as_str().unwrap_or(""))
    .map_err(|_| error::ErrorBadRequest("请输入有效的模型地址"))?;
  if name.is_empty()
    || name.len() > 256
    || name.chars().count() > 200
    || name.chars().any(char::is_control)
    || url.scheme() != "https"
    || url.host_str().is_none()
    || !url.username().is_empty()
    || url.password().is_some()
    || url.query().is_some()
    || url.fragment().is_some()
    || input["type"] != "KnowledgeQA"
    || input["parameters"]["api_key"].as_str().unwrap_or("").len() > 16384
  {
    return Err(error::ErrorBadRequest(
      "问AI模型需要 HTTPS 地址、模型名称和 OpenAI 兼容接口",
    ));
  }
  let mut config = json!({"name":name,"display_name":input["display_name"].as_str().filter(|s| !s.is_empty()).unwrap_or(name),"type":"KnowledgeQA","source":"remote","status":"active","parameters":{}});
  if config["display_name"].as_str().unwrap().chars().count() > 200 {
    return Err(error::ErrorBadRequest("模型显示名称过长"));
  }
  for field in [
    "base_url",
    "provider",
    "interface_type",
    "context_window",
    "max_output_tokens",
    "supports_vision",
  ] {
    if let Some(value) = input["parameters"].get(field) {
      config["parameters"][field] = value.clone();
    }
  }
  if let Some(spec) = input["parameters"]["spec"].as_object() {
    config["parameters"]["spec"] = json!({});
    for field in ["api", "reasoning", "context_window", "max_output_tokens"] {
      if let Some(value) = spec.get(field) {
        config["parameters"]["spec"][field] = value.clone();
      }
    }
  }
  for field in ["context_window", "max_output_tokens"] {
    if config["parameters"]
      .get(field)
      .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0 || n > 2_000_000))
    {
      return Err(error::ErrorBadRequest("模型上下文和输出上限无效"));
    }
  }
  // This model follows the existing Agent custom endpoint contract.
  if config["parameters"]["interface_type"]
    .as_str()
    .is_some_and(|v| v != "openai")
    || config["parameters"]["spec"]["api"]
      .as_str()
      .is_some_and(|v| v != "openai-completions")
  {
    return Err(error::ErrorBadRequest(
      "问AI自定义模型请使用 OpenAI 兼容接口",
    ));
  }
  Ok(config)
}

#[derive(Deserialize)]
struct ListQuery {
  #[serde(default)]
  personal: bool,
}

async fn list(
  auth: Authorization,
  path: web::Path<Uuid>,
  state: web::Data<AppState>,
  query: web::Query<ListQuery>,
) -> actix_web::Result<HttpResponse> {
  let workspace = *path;
  let uid = super::conversations::workspace_user(&auth, workspace, &state).await?;
  let manage = can_manage(&auth, &state, workspace).await?;
  let rows = sqlx::query("SELECT m.id,m.workspace_id,m.creator_uid,m.config,u.name AS creator_name FROM af_agent_custom_models m JOIN af_user u ON u.uid=m.creator_uid WHERE m.workspace_id=$1 OR ($3 AND m.creator_uid=$2) ORDER BY m.created_at,m.id")
    .bind(workspace).bind(uid).bind(query.personal).fetch_all(&state.pg_pool).await.map_err(unavailable)?;
  let models: Vec<Value> = rows
    .iter()
    .map(|row| {
      let id: Uuid = row.get("id");
      let origin: Uuid = row.get("workspace_id");
      let creator: i64 = row.get("creator_uid");
      let mut model: Value = row.get("config");
      model["id"] = json!(format!("custom:{id}"));
      model["workspace_id"] = json!(origin);
      model["creator_name"] = json!(row.get::<String, _>("creator_name"));
      model["show_creator"] = json!(!manage && creator != uid);
      model["is_builtin"] = json!(false);
      model["can_edit"] = json!(manage && origin == workspace);
      model
    })
    .collect();
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"models":models,"can_manage":manage})),
  )
}

async fn create(
  auth: Authorization,
  path: web::Path<Uuid>,
  state: web::Data<AppState>,
  input: web::Json<Value>,
) -> actix_web::Result<HttpResponse> {
  let workspace = *path;
  if !can_manage(&auth, &state, workspace).await? {
    return Err(error::ErrorForbidden("仅管理员或工作区拥有者可以配置模型"));
  }
  let config = validate(&input)?;
  let key = input["parameters"]["api_key"].as_str().unwrap_or("").trim();
  if key.is_empty() {
    return Err(error::ErrorBadRequest("请输入 API Key"));
  }
  let uid = state
    .user_cache
    .get_user_uid(&auth.uuid()?)
    .await
    .map_err(unavailable)?;
  let id = Uuid::new_v4();
  let credential = encrypt(state.config.gotrue.jwt_secret.expose_secret(), id, key)?;
  sqlx::query("INSERT INTO af_agent_custom_models(id,workspace_id,creator_uid,config,credential) VALUES($1,$2,$3,$4,$5)")
    .bind(id).bind(workspace).bind(uid).bind(config).bind(credential).execute(&state.pg_pool).await.map_err(unavailable)?;
  Ok(HttpResponse::Created().json(json!({"id":format!("custom:{id}")})))
}

async fn update(
  auth: Authorization,
  path: web::Path<(Uuid, Uuid)>,
  state: web::Data<AppState>,
  input: web::Json<Value>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, id) = *path;
  if !can_manage(&auth, &state, workspace).await? {
    return Err(error::ErrorForbidden("仅管理员或工作区拥有者可以配置模型"));
  }
  let config = validate(&input)?;
  let key = input["parameters"]["api_key"].as_str().unwrap_or("").trim();
  let credential = if key.is_empty() {
    let previous: Value = sqlx::query_scalar(
      "SELECT config FROM af_agent_custom_models WHERE workspace_id=$1 AND id=$2",
    )
    .bind(workspace)
    .bind(id)
    .fetch_optional(&state.pg_pool)
    .await
    .map_err(unavailable)?
    .ok_or_else(|| error::ErrorNotFound("模型不存在"))?;
    if !can_reuse_credential(&previous, &config) {
      return Err(error::ErrorConflict(
        "修改接口地址或服务商时请重新填写 API Key",
      ));
    }
    None
  } else {
    Some(encrypt(
      state.config.gotrue.jwt_secret.expose_secret(),
      id,
      key,
    )?)
  };
  let result = sqlx::query("UPDATE af_agent_custom_models SET config=$3,credential=COALESCE($4,credential) WHERE workspace_id=$1 AND id=$2")
    .bind(workspace).bind(id).bind(config).bind(credential).execute(&state.pg_pool).await.map_err(unavailable)?;
  if result.rows_affected() == 0 {
    return Err(error::ErrorNotFound("模型不存在"));
  }
  Ok(HttpResponse::Ok().json(json!({"id":format!("custom:{id}")})))
}

async fn delete(
  auth: Authorization,
  path: web::Path<(Uuid, Uuid)>,
  state: web::Data<AppState>,
) -> actix_web::Result<HttpResponse> {
  let (workspace, id) = *path;
  if !can_manage(&auth, &state, workspace).await? {
    return Err(error::ErrorForbidden("仅管理员或工作区拥有者可以删除模型"));
  }
  let result = sqlx::query("DELETE FROM af_agent_custom_models WHERE workspace_id=$1 AND id=$2")
    .bind(workspace)
    .bind(id)
    .execute(&state.pg_pool)
    .await
    .map_err(unavailable)?;
  if result.rows_affected() == 0 {
    return Err(error::ErrorNotFound("模型不存在"));
  }
  Ok(HttpResponse::NoContent().finish())
}

pub(super) async fn resolve(
  state: &AppState,
  uid: i64,
  workspace: Uuid,
  id: Uuid,
) -> actix_web::Result<(Value, String)> {
  let row = sqlx::query(
    "SELECT creator_uid,workspace_id,config,credential FROM af_agent_custom_models WHERE id=$1",
  )
  .bind(id)
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(unavailable)?
  .ok_or_else(|| error::ErrorForbidden("模型已删除或无权使用"))?;
  if !allowed(
    row.get("creator_uid"),
    uid,
    row.get("workspace_id"),
    workspace,
  ) {
    return Err(error::ErrorForbidden(
      "此模型仅原工作区成员及配置者本人可使用",
    ));
  }
  let key = decrypt(
    state.config.gotrue.jwt_secret.expose_secret(),
    id,
    &row.get::<Vec<u8>, _>("credential"),
  )?;
  Ok((row.get("config"), key))
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn model_permissions_follow_creator_and_original_workspace() {
    let home = Uuid::new_v4();
    let other = Uuid::new_v4();
    assert!(allowed(1, 1, home, home));
    assert!(allowed(1, 2, home, home));
    assert!(allowed(1, 1, home, other));
    assert!(!allowed(1, 2, home, other));
  }
  #[test]
  fn credentials_are_bound_to_model_and_key() {
    let id = Uuid::new_v4();
    let secret = "server-secret";
    let encrypted = encrypt(secret, id, "user-api-key").unwrap();
    assert_eq!(decrypt(secret, id, &encrypted).unwrap(), "user-api-key");
    assert!(decrypt("other-secret", id, &encrypted).is_err());
    assert!(decrypt(secret, Uuid::new_v4(), &encrypted).is_err());
    assert!(decrypt(secret, id, &encrypted[..5]).is_err());
    assert_ne!(encrypted, encrypt(secret, id, "user-api-key").unwrap());
  }
  #[test]
  fn public_config_never_contains_credentials() {
    let input = json!({"name":"reasoner","type":"KnowledgeQA","parameters":{"base_url":"https://model.example/v1","api_key":"private","custom_headers":{"secret":"private"},"spec":{"api":"openai-completions","reasoning":true,"compat":{"headers":{"Authorization":"private"}}}}});
    let config = validate(&input).unwrap();
    assert!(!config.to_string().contains("private"));
    let mut invalid = input;
    invalid["parameters"]["base_url"] = json!("http://localhost:11434");
    assert!(validate(&invalid).is_err());
  }

  #[test]
  fn changing_endpoint_or_provider_requires_a_new_key() {
    let original =
      json!({"parameters":{"base_url":"https://model.example/v1","provider":"generic"}});
    assert!(can_reuse_credential(&original, &original));
    let mut changed = original.clone();
    changed["parameters"]["base_url"] = json!("https://other.example/v1");
    assert!(!can_reuse_credential(&original, &changed));
    changed = original.clone();
    changed["parameters"]["provider"] = json!("other");
    assert!(!can_reuse_credential(&original, &changed));
  }
}
