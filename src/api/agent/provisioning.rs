use actix_web::error;
use hmac::{Hmac, Mac};
use secrecy::{ExposeSecret, Secret};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use std::{
  sync::Arc,
  time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{upstream_error, Sub2ApiGateway};

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(super) enum ManagementAuth {
  Key(Secret<String>),
  Login {
    email: String,
    password: Secret<String>,
    cached: Arc<Mutex<Option<(Instant, Secret<String>)>>>,
  },
}

impl ManagementAuth {
  pub(super) fn from_env() -> anyhow::Result<Self> {
    let key = std::env::var("PONYNOTES_SUB2API_ADMIN_KEY").unwrap_or_default();
    if !key.trim().is_empty() {
      return Ok(Self::Key(key.into()));
    }
    let email = std::env::var("PONYNOTES_SUB2API_ADMIN_EMAIL")?;
    let password = std::env::var("PONYNOTES_SUB2API_ADMIN_PASSWORD")?;
    anyhow::ensure!(
      !email.trim().is_empty() && !password.is_empty(),
      "Missing sub2api management credentials"
    );
    Ok(Self::Login {
      email,
      password: password.into(),
      cached: Arc::new(Mutex::new(None)),
    })
  }
}

#[derive(Clone)]
pub(super) struct Provisioning {
  secret: Secret<String>,
  group_id: i64,
}

impl Provisioning {
  pub(super) fn from_env() -> anyhow::Result<Option<Self>> {
    let secret = std::env::var("PONYNOTES_SUB2API_PROVISION_SECRET").unwrap_or_default();
    let group = std::env::var("PONYNOTES_SUB2API_GROUP_ID").unwrap_or_default();
    if secret.is_empty() && group.is_empty() {
      return Ok(None);
    }
    anyhow::ensure!(
      secret.len() >= 32,
      "Sub2api provisioning secret must have at least 32 bytes"
    );
    let group_id = group.parse::<i64>()?;
    anyhow::ensure!(group_id > 0, "Sub2api group ID must be positive");
    Ok(Some(Self {
      secret: secret.into(),
      group_id,
    }))
  }

  fn credential(&self, provision_id: Uuid, purpose: &str) -> String {
    // Separate domains keep the login password and API key independent.
    let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.expose_secret().as_bytes())
      .expect("HMAC accepts arbitrary-length keys");
    mac.update(b"ponynotes-sub2api-v1\0");
    mac.update(purpose.as_bytes());
    mac.update(b"\0");
    mac.update(provision_id.as_bytes());
    let bytes = mac.finalize().into_bytes();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
  }
}

fn db_error(_: sqlx::Error) -> actix_web::Error {
  error::ErrorServiceUnavailable("内部 AI 账号开通记录暂不可用，请重试")
}

impl Sub2ApiGateway {
  pub(super) async fn management_request(
    &self,
    request: reqwest::RequestBuilder,
  ) -> actix_web::Result<reqwest::RequestBuilder> {
    match &self.management {
      ManagementAuth::Key(key) => Ok(request.header("x-api-key", key.expose_secret())),
      ManagementAuth::Login {
        email,
        password,
        cached,
      } => {
        let mut cached = cached.lock().await;
        if cached
          .as_ref()
          .map(|(expires, _)| *expires <= Instant::now())
          .unwrap_or(true)
        {
          let login = self
            .provision_json(
              self
                .client
                .post(format!("{}/api/v1/auth/login", self.base))
                .json(&json!({"email":email,"password":password.expose_secret()})),
              "sub2api 管理员认证失败，请检查服务端管理账号配置",
            )
            .await?;
          if login["user"]["role"] != "admin" || login["user"]["status"] != "active" {
            return Err(error::ErrorForbidden(
              "sub2api 管理账号无管理员权限或已停用",
            ));
          }
          let token = login["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| error::ErrorBadGateway("sub2api 未返回管理员会话"))?;
          let lifetime = login["expires_in"].as_u64().unwrap_or(600).min(600);
          if lifetime < 60 {
            return Err(error::ErrorBadGateway("sub2api 管理员会话有效期过短"));
          }
          *cached = Some((
            Instant::now() + Duration::from_secs(lifetime - 30),
            token.to_owned().into(),
          ));
        }
        Ok(
          request.bearer_auth(
            cached
              .as_ref()
              .expect("management session was initialized")
              .1
              .expose_secret(),
          ),
        )
      },
    }
  }
  pub(super) async fn account_for_user(
    &self,
    pool: &PgPool,
    uid: i64,
  ) -> actix_web::Result<(i64, i64)> {
    // Preserve existing bindings; new provisioning settings apply only to new accounts.
    let existing = sqlx::query_as::<_, (Option<i64>, Option<i64>)>(
      "SELECT a.sub2api_user_id, a.sub2api_key_id FROM af_agent_accounts a \
       JOIN af_user u ON u.uid = a.uid WHERE a.uid = $1 AND u.deleted_at IS NULL",
    )
    .bind(uid)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;
    if let Some((Some(user), Some(key))) = existing {
      return Ok((user, key));
    }
    let provisioning = self
      .provisioning
      .as_ref()
      .ok_or_else(|| error::ErrorServiceUnavailable("内部 AI 自动开通尚未配置，请联系管理员"))?;
    // Commit the recovery identity before any external side effect.
    sqlx::query(
      "INSERT INTO af_agent_accounts (uid) SELECT uid FROM af_user WHERE uid = $1 \
       AND deleted_at IS NULL ON CONFLICT (uid) DO NOTHING",
    )
    .bind(uid)
    .execute(pool)
    .await
    .map_err(db_error)?;
    let mut tx = pool.begin().await.map_err(db_error)?;
    // A row lock serializes this user's provisioning across all cloud instances.
    let (provision_id, user, key, username) = sqlx::query_as::<_, (Uuid, Option<i64>, Option<i64>, String)>(
      "SELECT a.provision_id, a.sub2api_user_id, a.sub2api_key_id, u.name FROM af_agent_accounts a \
       JOIN af_user u ON u.uid = a.uid WHERE a.uid = $1 AND u.deleted_at IS NULL FOR UPDATE OF a",
    )
    .bind(uid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| error::ErrorUnauthorized("账号已停用，请重新登录"))?;
    if let (Some(user), Some(key)) = (user, key) {
      tx.commit().await.map_err(db_error)?;
      return Ok((user, key));
    }
    let account = self
      .provision_account(provisioning, provision_id, uid, &username)
      .await?;
    sqlx::query(
      "UPDATE af_agent_accounts SET sub2api_user_id = $2, sub2api_key_id = $3 WHERE uid = $1",
    )
    .bind(uid)
    .bind(account.0)
    .bind(account.1)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(account)
  }

  async fn admin_json(&self, path: &str, body: Option<Value>) -> actix_web::Result<Value> {
    let url = format!("{}/api/v1/admin/{path}", self.base);
    let request = match body {
      Some(body) => self.client.post(url).json(&body),
      None => self.client.get(url),
    };
    let request = self.management_request(request).await?;
    self
      .provision_json(request, "sub2api 自动开通失败，请检查管理接口配置后重试")
      .await
  }

  async fn provision_json(
    &self,
    request: reqwest::RequestBuilder,
    message: &'static str,
  ) -> actix_web::Result<Value> {
    let response = request
      .timeout(std::time::Duration::from_secs(15))
      .send()
      .await
      .map_err(upstream_error)?;
    if !response.status().is_success() {
      return Err(error::ErrorBadGateway(message));
    }
    let body: Value = response.json().await.map_err(upstream_error)?;
    if body.get("code").and_then(Value::as_i64) != Some(0) || !body["data"].is_object() {
      return Err(error::ErrorBadGateway(message));
    }
    Ok(body["data"].clone())
  }

  async fn find_provisioned_user(&self, email: &str) -> actix_web::Result<Option<Value>> {
    let mut page = 1;
    loop {
      let query = format!(
        "users?search={}&page_size=100&page={page}",
        urlencoding::encode(email)
      );
      let data = self.admin_json(&query, None).await?;
      let items = data["items"]
        .as_array()
        .ok_or_else(|| error::ErrorBadGateway("sub2api 用户列表响应无效"))?;
      if let Some(user) = items
        .iter()
        .find(|user| user["email"].as_str() == Some(email))
      {
        return Ok(Some(user.clone()));
      }
      let pages = data["pages"]
        .as_u64()
        .ok_or_else(|| error::ErrorBadGateway("sub2api 用户分页响应无效"))?;
      if items.is_empty() || page >= pages {
        return Ok(None);
      }
      page += 1;
    }
  }

  async fn provision_account(
    &self,
    provisioning: &Provisioning,
    provision_id: Uuid,
    uid: i64,
    username: &str,
  ) -> actix_web::Result<(i64, i64)> {
    let email = format!("pony-{provision_id}@ponynotes.invalid");
    let password = Secret::new(provisioning.credential(provision_id, "password"));
    let expected_key = Secret::new(format!(
      "sk-pony-{}",
      provisioning.credential(provision_id, "api-key")
    ));
    let user = match self.find_provisioned_user(&email).await? {
      Some(user) => user,
      None => self.admin_json("users", Some(json!({
        "email": email, "password": password.expose_secret(), "username": username,
        "notes": format!("PonyNotes uid={uid} Agent {provision_id}"), "role": "user", "balance": 0,
        "concurrency": 1, "allowed_groups": [provisioning.group_id], "restrict_public_groups": true,
      }))).await?,
    };
    let user_id = validate_user(&user, &email)?;
    let keys = self.user_keys(user_id).await?;
    if let Some(key) = find_owned_key(&keys, user_id, expected_key.expose_secret())? {
      return Ok((user_id, key));
    }
    // Authenticate as this managed user; never reset an existing account's password.
    let login = self
      .provision_json(
        self
          .client
          .post(format!("{}/api/v1/auth/login", self.base))
          .json(&json!({"email": email, "password": password.expose_secret()})),
        "sub2api 自动开通登录失败，请检查用户登录接口设置后重试",
      )
      .await?;
    if login["user"]["id"].as_i64() != Some(user_id) {
      return Err(error::ErrorBadGateway("sub2api 登录用户与开通账号不一致"));
    }
    let token = login["access_token"]
      .as_str()
      .filter(|token| !token.is_empty())
      .ok_or_else(|| error::ErrorBadGateway("sub2api 未返回开通所需的用户登录凭证"))?;
    let created = self.provision_json(
      self.client.post(format!("{}/api/v1/keys", self.base))
        .bearer_auth(token).header("Idempotency-Key", format!("ponynotes-agent-{provision_id}"))
        .json(&json!({"name": "PonyNotes Agent", "group_id": provisioning.group_id, "custom_key": expected_key.expose_secret()})),
      "sub2api 专属 Key 创建失败，请重试",
    ).await?;
    let key_id = find_owned_key(&[created], user_id, expected_key.expose_secret())?
      .ok_or_else(|| error::ErrorBadGateway("sub2api 返回的 Key 与开通账号不一致"))?;
    Ok((user_id, key_id))
  }
}

fn validate_user(user: &Value, email: &str) -> actix_web::Result<i64> {
  if user["email"].as_str() != Some(email) || user["role"] != "user" || user["status"] != "active" {
    return Err(error::ErrorForbidden(
      "sub2api 开通账号状态或身份不匹配，请联系管理员",
    ));
  }
  user["id"]
    .as_i64()
    .filter(|id| *id > 0)
    .ok_or_else(|| error::ErrorBadGateway("sub2api 开通账号 ID 无效"))
}

fn find_owned_key(
  keys: &[Value],
  user_id: i64,
  expected_key: &str,
) -> actix_web::Result<Option<i64>> {
  let key = match keys
    .iter()
    .find(|key| key["key"].as_str() == Some(expected_key))
  {
    Some(key) => key,
    None => return Ok(None),
  };
  super::bound_key(key, user_id)?;
  key["id"]
    .as_i64()
    .filter(|id| *id > 0)
    .map(Some)
    .ok_or_else(|| error::ErrorBadGateway("sub2api 专属 Key ID 无效"))
}
