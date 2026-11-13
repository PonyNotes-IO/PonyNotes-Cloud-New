//! Authenticated model discovery and a scoped OpenAI proxy; sub2api is the billing authority.
use std::time::Duration;

use actix_web::{error, web, HttpRequest, HttpResponse, Scope};
use futures_util::StreamExt;
use redis::AsyncCommands;
use rust_decimal::{prelude::ToPrimitive, Decimal};
use secrecy::{ExposeSecret, Secret};
use serde_json::{json, Value};

use crate::{biz::authentication::jwt::UserUuid, state::AppState};

mod conversations;
pub(crate) use conversations::authorize_artifact_blob;
mod automations;
mod note_tools;
mod provisioning;

const SESSION_SECONDS: u64 = 86400;

#[derive(Clone)]
pub struct Sub2ApiGateway {
  client: reqwest::Client,
  base: String,
  management: provisioning::ManagementAuth,
  credits_per_usd: u32,
  provisioning: Option<provisioning::Provisioning>,
}

impl Sub2ApiGateway {
  pub fn from_env() -> anyhow::Result<Option<Self>> {
    let base = std::env::var("PONYNOTES_SUB2API_URL").unwrap_or_default();
    if base.is_empty() {
      return Ok(None);
    }
    let url = reqwest::Url::parse(&base)?;
    anyhow::ensure!(
      matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none(),
      "Invalid sub2api URL"
    );
    let credits_per_usd = std::env::var("PONYNOTES_CREDITS_PER_USD")
      .unwrap_or_else(|_| "100".into())
      .parse::<u32>()?;
    anyhow::ensure!(credits_per_usd > 0, "Credits rate must be positive");
    Ok(Some(Self {
      client: reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?,
      base: base
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_owned(),
      management: provisioning::ManagementAuth::from_env()?,
      credits_per_usd,
      provisioning: provisioning::Provisioning::from_env()?,
    }))
  }

  async fn key_for_user(&self, state: &AppState, uid: i64) -> actix_web::Result<Secret<String>> {
    let account = self.account_for_user(&state.pg_pool, uid).await?;
    self.user_key(account.0, account.1).await
  }

  async fn user_keys(&self, user_id: i64) -> actix_web::Result<Vec<Value>> {
    let mut keys = Vec::new();
    let mut page = 1;
    loop {
      let request = self
        .client
        .get(format!(
          "{}/api/v1/admin/users/{}/api-keys",
          self.base, user_id
        ))
        .query(&[("page", page), ("page_size", 100)]);
      let response = self
        .management_request(request)
        .await?
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(upstream_error)?;
      if !response.status().is_success() {
        return Err(error::ErrorServiceUnavailable(
          "内部模型凭据查询失败，请联系管理员",
        ));
      }
      let body: Value = response.json().await.map_err(upstream_error)?;
      let data = &body["data"];
      let items = data["items"]
        .as_array()
        .ok_or_else(|| error::ErrorBadGateway("内部模型凭据响应无效"))?;
      keys.extend(items.iter().cloned());
      let pages = data["pages"]
        .as_u64()
        .ok_or_else(|| error::ErrorBadGateway("内部模型凭据分页无效"))?;
      if items.is_empty() || page >= pages {
        return Ok(keys);
      }
      page += 1;
    }
  }

  async fn user_key(&self, user_id: i64, key_id: i64) -> actix_web::Result<Secret<String>> {
    let keys = self.user_keys(user_id).await?;
    let key = keys
      .iter()
      .find(|key| key["id"].as_i64() == Some(key_id))
      .ok_or_else(|| error::ErrorForbidden("内部模型凭据已移除，请联系管理员"))?;
    bound_key(key, user_id)
  }

  async fn get(&self, path: &str, key: &Secret<String>) -> actix_web::Result<Value> {
    let response = self
      .client
      .get(format!("{}/v1/{path}", self.base))
      .bearer_auth(key.expose_secret())
      .timeout(Duration::from_secs(15))
      .send()
      .await
      .map_err(upstream_error)?;
    let status = response.status();
    let body: Value = response.json().await.map_err(upstream_error)?;
    if !status.is_success() {
      if body["error"]["code"] == "INSUFFICIENT_BALANCE" || body["code"] == "INSUFFICIENT_BALANCE" {
        return Err(error::ErrorForbidden(
          "积分不足，请先为您的 sub2api 账户充值后重试",
        ));
      }
      return Err(error::ErrorBadGateway("内部模型服务暂不可用，请稍后重试"));
    }
    Ok(body)
  }
}

fn bound_key(key: &Value, user_id: i64) -> actix_web::Result<Secret<String>> {
  if key["user_id"].as_i64() != Some(user_id) {
    return Err(error::ErrorForbidden(
      "内部模型凭据不属于绑定账号，请联系管理员",
    ));
  }
  match key["status"].as_str() {
    Some("active") => {},
    Some("quota_exhausted") => {
      return Err(error::ErrorForbidden("积分不足，充值后可继续使用 Agent"))
    },
    Some("expired") => return Err(error::ErrorForbidden("内部模型凭据已过期，请联系管理员")),
    _ => return Err(error::ErrorForbidden("内部模型凭据已停用，请联系管理员")),
  }
  let value = key["key"]
    .as_str()
    .filter(|s| !s.trim().is_empty())
    .ok_or_else(|| error::ErrorBadGateway("内部模型凭据无效"))?;
  Ok(value.to_owned().into())
}

fn upstream_error(_: reqwest::Error) -> actix_web::Error {
  // Upstream errors can contain credentials or URLs; only return our own message.
  error::ErrorBadGateway("内部模型服务连接失败，请稍后重试")
}

fn gateway(state: &AppState) -> actix_web::Result<&Sub2ApiGateway> {
  state
    .sub2api
    .as_ref()
    .ok_or_else(|| error::ErrorServiceUnavailable("内部模型服务尚未配置，请联系管理员"))
}

impl Sub2ApiGateway {
  /// Unattended automation runs need a credential that outlives a 24h session.
  pub(crate) fn automation_token(&self, uid: i64) -> actix_web::Result<Secret<String>> {
    self
      .provisioning
      .as_ref()
      .map(|provisioning| Secret::new(provisioning.automation_token(uid)))
      .ok_or_else(|| error::ErrorServiceUnavailable("内部 AI 自动开通尚未配置，自动化任务暂不可用"))
  }

  fn automation_uid(&self, token: &str) -> Option<i64> {
    self.provisioning.as_ref()?.automation_uid(token)
  }
}

pub fn agent_scope() -> Scope {
  web::scope("/agent")
    .service(
      web::resource("/note-tools")
        .app_data(web::JsonConfig::default().limit(1024 * 1024))
        .route(web::post().to(note_tools::rpc)),
    )
    .route(
      "/conversations/{workspace_id}/references",
      web::get().to(conversations::reference_catalog),
    )
    .route(
      "/conversations/{workspace_id}/references/{knowledge_id}",
      web::get().to(conversations::reference_file),
    )
    .service(conversations::scope())
    .service(conversations::collection_scope())
    .service(automations::scope())
    .route("/models", web::get().to(models))
    .route("/usage", web::get().to(usage))
    .route("/session", web::post().to(create_session))
    .route("/session", web::delete().to(revoke_session))
    .service(
      web::resource("/chat/completions")
        .app_data(web::JsonConfig::default().limit(20 * 1024 * 1024))
        .route(web::post().to(completions)),
    )
}

async fn user_uid(user: &UserUuid, state: &AppState) -> actix_web::Result<i64> {
  state
    .user_cache
    .get_user_uid(user)
    .await
    .map_err(|_| error::ErrorUnauthorized("请重新登录后使用内部模型"))
}

async fn models(user: UserUuid, state: web::Data<AppState>) -> actix_web::Result<HttpResponse> {
  let gateway = gateway(&state)?;
  let key = gateway
    .key_for_user(&state, user_uid(&user, &state).await?)
    .await?;
  let body = gateway.get("models", &key).await?;
  let data = body["data"]
    .as_array()
    .ok_or_else(|| error::ErrorBadGateway("内部模型列表响应无效"))?;
  // Only publish model metadata, never the admin response or credentials.
  let models: Vec<Value> = data.iter().filter_map(|model| {
    let id = model["id"].as_str().filter(|id| !id.is_empty())?;
    Some(json!({"id": id, "supports_reasoning": model["supports_reasoning"].as_bool().unwrap_or(false)}))
  }).collect();
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"data": models})),
  )
}

fn credit_balance(body: &Value, wallet: Option<&Value>, rate: u32) -> actix_web::Result<Value> {
  if (body.get("unit").is_some() && body["unit"] != "USD")
    || (body.get("unit").is_none() && wallet.is_none())
    || body["isValid"] == false
  {
    return Err(error::ErrorBadGateway("内部积分余额响应无效"));
  }
  let quota = body
    .get("remaining")
    .or_else(|| body.get("balance"))
    .map(|v| {
      v.as_number()
        .and_then(|v| v.to_string().parse::<Decimal>().ok())
        .ok_or_else(|| error::ErrorBadGateway("内部积分余额响应无效"))
    })
    .transpose()?;
  let remaining = if let Some(wallet) = wallet {
    let balance = wallet
      .as_number()
      .and_then(|v| v.to_string().parse::<Decimal>().ok())
      .ok_or_else(|| error::ErrorBadGateway("内部积分余额响应无效"))?
      .max(Decimal::ZERO);
    match quota {
      Some(value) if value >= Decimal::ZERO => balance.min(value),
      Some(value) if value == Decimal::NEGATIVE_ONE => balance,
      None => balance,
      _ => return Err(error::ErrorBadGateway("内部积分余额无效")),
    }
  } else {
    quota.ok_or_else(|| error::ErrorBadGateway("内部积分余额响应无效"))?
  };
  let unlimited = remaining == Decimal::NEGATIVE_ONE;
  if remaining < Decimal::ZERO && !unlimited {
    return Err(error::ErrorBadGateway("内部积分余额无效"));
  }
  let credits = if unlimited {
    None
  } else {
    Some(
      remaining
        .checked_mul(Decimal::from(rate))
        .ok_or_else(|| error::ErrorBadGateway("内部积分余额超出范围"))?
        .floor()
        .to_i64()
        .ok_or_else(|| error::ErrorBadGateway("内部积分余额超出范围"))?,
    )
  };
  Ok(
    json!({"remaining": remaining.to_f64(), "unit": "USD", "credits": credits,
    "credits_per_usd": rate, "unlimited": unlimited}),
  )
}

async fn usage(user: UserUuid, state: web::Data<AppState>) -> actix_web::Result<HttpResponse> {
  let gateway = gateway(&state)?;
  let (user_id, key_id) = gateway
    .account_for_user(&state.pg_pool, user_uid(&user, &state).await?)
    .await?;
  let keys = gateway.user_keys(user_id).await?;
  let key_info = keys
    .iter()
    .find(|key| key["id"].as_i64() == Some(key_id))
    .ok_or_else(|| error::ErrorForbidden("内部模型凭据已移除，请联系管理员"))?;
  let key = bound_key(key_info, user_id)?;
  let body = gateway.get("usage", &key).await?;
  let wallet = if body["mode"] == "quota_limited" {
    let billing = if key_info["group_id"].is_null() {
      "standard"
    } else {
      key_info["group"]["subscription_type"]
        .as_str()
        .ok_or_else(|| error::ErrorBadGateway("内部模型计费模式响应无效"))?
    };
    match billing {
      "standard" => {
        let account = gateway
          .admin_json(&format!("users/{user_id}"), None)
          .await?;
        if account["id"].as_i64() != Some(user_id) {
          return Err(error::ErrorBadGateway("内部模型账户响应无效"));
        }
        Some(account["balance"].clone())
      },
      "subscription" => None,
      _ => return Err(error::ErrorBadGateway("内部模型计费模式响应无效")),
    }
  } else {
    None
  };
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(credit_balance(
        &body,
        wallet.as_ref(),
        gateway.credits_per_usd,
      )?),
  )
}

async fn create_session(
  user: UserUuid,
  state: web::Data<AppState>,
) -> actix_web::Result<HttpResponse> {
  let uid = user_uid(&user, &state).await?;
  gateway(&state)?.key_for_user(&state, uid).await?;
  let token = issue_session_token(&state, uid).await?;
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"token": token, "expires_in": SESSION_SECONDS})),
  )
}

async fn issue_session_token(state: &AppState, uid: i64) -> actix_web::Result<String> {
  let token = format!("pn-agent-{}", nanoid::nanoid!(48));
  let mut redis = state.redis_connection_manager.clone();
  let _: () = redis
    .set_ex(format!("agent:session:{token}"), uid, SESSION_SECONDS)
    .await
    .map_err(|_| error::ErrorServiceUnavailable("内部模型会话创建失败"))?;
  Ok(token)
}

fn conversation_model_key(uid: i64, workspace: uuid::Uuid, document: uuid::Uuid) -> String {
  format!("agent:conversation:model:{uid}:{workspace}:{document}")
}

async fn revoke_conversation_model(
  state: &AppState,
  uid: i64,
  workspace: uuid::Uuid,
  document: uuid::Uuid,
) -> actix_web::Result<()> {
  let mut redis = state.redis_connection_manager.clone();
  let token: Option<String> = redis::cmd("GETDEL")
    .arg(conversation_model_key(uid, workspace, document))
    .query_async(&mut redis)
    .await
    .map_err(|_| error::ErrorServiceUnavailable("模型会话撤销失败"))?;
  if let Some(token) = token {
    let _: usize = redis
      .del(format!("agent:session:{token}"))
      .await
      .map_err(|_| error::ErrorServiceUnavailable("模型会话撤销失败"))?;
  }
  Ok(())
}

fn session_token(req: &HttpRequest) -> actix_web::Result<&str> {
  let token = req
    .headers()
    .get("Authorization")
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.strip_prefix("Bearer "))
    .ok_or_else(|| error::ErrorUnauthorized("内部模型会话无效，请重新连接 Agent"))?;
  let valid = if let Some(rest) = token.strip_prefix(provisioning::AUTOMATION_TOKEN_PREFIX) {
    rest.split_once('-').is_some_and(|(uid, digest)| {
      !uid.is_empty()
        && uid.bytes().all(|c| c.is_ascii_digit())
        && digest.len() == 32
        && digest.bytes().all(|c| c.is_ascii_hexdigit())
    })
  } else {
    token.starts_with("pn-agent-")
      && token.len() == 57
      && token
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
  };
  if !valid {
    return Err(error::ErrorUnauthorized("内部模型会话无效，请重新连接 Agent"));
  }
  Ok(token)
}

async fn revoke_session(
  req: HttpRequest,
  state: web::Data<AppState>,
) -> actix_web::Result<HttpResponse> {
  let token = session_token(&req)?;
  let mut redis = state.redis_connection_manager.clone();
  let _: usize = redis
    .del(format!("agent:session:{token}"))
    .await
    .map_err(|_| error::ErrorServiceUnavailable("内部模型会话撤销失败"))?;
  Ok(HttpResponse::NoContent().finish())
}

async fn completions(
  req: HttpRequest,
  state: web::Data<AppState>,
  payload: web::Json<Value>,
) -> actix_web::Result<HttpResponse> {
  let token = session_token(&req)?;
  let gateway = gateway(&state)?;
  let uid = if token.starts_with(provisioning::AUTOMATION_TOKEN_PREFIX) {
    gateway
      .automation_uid(token)
      .ok_or_else(|| error::ErrorUnauthorized("自动化凭据无效，请重新保存自动化任务"))?
  } else {
    let mut redis = state.redis_connection_manager.clone();
    let uid: Option<i64> = redis
      .get(format!("agent:session:{token}"))
      .await
      .map_err(|_| error::ErrorServiceUnavailable("内部模型会话查询失败"))?;
    uid.ok_or_else(|| error::ErrorUnauthorized("内部模型会话已过期，请重新连接 Agent"))?
  };
  let key = gateway.key_for_user(&state, uid).await?;
  let mut body = payload.into_inner();
  if body["model"]
    .as_str()
    .filter(|v| !v.trim().is_empty())
    .is_none()
    || !body["messages"].is_array()
  {
    return Err(error::ErrorBadRequest("请选择内部模型并提供消息"));
  }
  body["user"] = json!(uid.to_string());
  let response = gateway
    .client
    .post(format!("{}/v1/chat/completions", gateway.base))
    .bearer_auth(key.expose_secret())
    .json(&body)
    .timeout(Duration::from_secs(600))
    .send()
    .await
    .map_err(upstream_error)?;
  if !response.status().is_success() {
    let status = actix_web::http::StatusCode::from_u16(response.status().as_u16())
      .unwrap_or(actix_web::http::StatusCode::BAD_GATEWAY);
    let body: Value = response.json().await.unwrap_or(Value::Null);
    let code = match body["error"]["code"]
      .as_str()
      .or_else(|| body["code"].as_str())
    {
      Some("INSUFFICIENT_BALANCE" | "insufficient_quota" | "API_KEY_QUOTA_EXHAUSTED") => {
        "INSUFFICIENT_BALANCE"
      },
      Some("USAGE_LIMIT_EXCEEDED") => "USAGE_LIMIT_EXCEEDED",
      _ => "INTERNAL_MODEL_ERROR",
    };
    return Ok(HttpResponse::build(status).json(
      json!({"error": {"code": code, "message": "内部模型请求失败，请检查积分或稍后重试"}}),
    ));
  }
  let content_type = response
    .headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("application/json")
    .to_owned();
  // Stream tools and usage unchanged. No client estimates or second debit: sub2api settles every round.
  Ok(
    HttpResponse::Ok()
      .content_type(content_type)
      .insert_header(("Cache-Control", "no-store"))
      .insert_header(("X-Accel-Buffering", "no"))
      .streaming(
        response
          .bytes_stream()
          .map(|chunk| chunk.map_err(upstream_error)),
      ),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn effective_quota_and_unlimited_are_not_account_balance() {
    let value = credit_balance(
      &json!({"unit":"USD", "balance":50, "remaining":1.239}),
      None,
      100,
    )
    .unwrap();
    assert_eq!(value["credits"], 123);
    assert_eq!(
      credit_balance(&json!({"unit":"USD", "remaining":-1}), None, 100).unwrap()["unlimited"],
      true
    );
    assert!(credit_balance(&json!({"unit":"USD", "remaining":-2}), None, 100).is_err());
    assert!(credit_balance(
      &json!({"unit":"USD", "isValid":false, "remaining":10}),
      None,
      100
    )
    .is_err());
    assert!(credit_balance(&json!({"unit":"tokens", "remaining":10}), None, 100).is_err());
    assert_eq!(
      credit_balance(
        &json!({"unit":"USD", "remaining":0.9622}),
        Some(&json!(0.01)),
        100
      )
      .unwrap()["credits"],
      1
    );
    assert_eq!(
      credit_balance(
        &json!({"unit":"USD", "remaining":0.01}),
        Some(&json!(1)),
        100
      )
      .unwrap()["credits"],
      1
    );
    assert_eq!(
      credit_balance(&json!({}), Some(&json!(0.01)), 100).unwrap()["credits"],
      1
    );
    assert!(credit_balance(&json!({}), Some(&json!(null)), 100).is_err());
  }

  #[test]
  fn keys_must_belong_to_bound_user_and_be_active() {
    let mut key = json!({"user_id":17,"status":"active","key":"test-secret"});
    assert!(bound_key(&key, 17).is_ok());
    assert!(bound_key(&key, 18).is_err());
    key["status"] = json!("inactive");
    assert!(bound_key(&key, 17).is_err());
    key["status"] = json!("quota_exhausted");
    assert!(bound_key(&key, 17)
      .unwrap_err()
      .to_string()
      .contains("积分不足"));
    key["status"] = json!("expired");
    assert!(bound_key(&key, 17)
      .unwrap_err()
      .to_string()
      .contains("已过期"));
  }

  #[test]
  fn login_tokens_cannot_be_used_as_agent_session_tokens() {
    let req = actix_web::test::TestRequest::default()
      .insert_header(("Authorization", "Bearer login-jwt"))
      .to_http_request();
    assert!(session_token(&req).is_err());
    let value = format!("pn-agent-{}", nanoid::nanoid!(48));
    let req = actix_web::test::TestRequest::default()
      .insert_header(("Authorization", format!("Bearer {value}")))
      .to_http_request();
    assert_eq!(session_token(&req).unwrap(), value);
  }
}
