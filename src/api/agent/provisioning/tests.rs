use std::{
  collections::HashMap,
  net::TcpListener,
  sync::{Arc, Mutex},
};

use actix_web::{dev::ServerHandle, web, App, HttpRequest, HttpResponse, HttpServer};

use super::*;

#[derive(Default)]
struct GatewayState {
  users: Vec<Value>,
  passwords: HashMap<i64, String>,
  keys: Vec<Value>,
  logins: usize,
  admin_logins: usize,
  fail_after_user: bool,
  fail_after_key: bool,
}

struct Fixture {
  gateway: Sub2ApiGateway,
  state: Arc<Mutex<GatewayState>>,
  server: ServerHandle,
}

impl Drop for Fixture {
  fn drop(&mut self) {
    let server = self.server.clone();
    tokio::spawn(async move {
      server.stop(false).await;
    });
  }
}

async fn mock_gateway(
  req: HttpRequest,
  payload: web::Bytes,
  state: web::Data<Arc<Mutex<GatewayState>>>,
) -> HttpResponse {
  let mut state = state.lock().unwrap();
  let path = req.path();
  let body: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
  if path.starts_with("/api/v1/admin/") {
    assert!(
      req
        .headers()
        .get("x-api-key")
        .map(|v| v == "test-admin")
        .unwrap_or(false)
        || req
          .headers()
          .get("Authorization")
          .map(|v| v == "Bearer admin-token")
          .unwrap_or(false)
    );
  }
  if path == "/api/v1/admin/users" {
    if req.method() == "GET" {
      let query = web::Query::<HashMap<String, String>>::from_query(req.query_string()).unwrap();
      let users: Vec<_> = state
        .users
        .iter()
        .filter(|user| user["email"].as_str() == query.get("search").map(String::as_str))
        .cloned()
        .collect();
      return HttpResponse::Ok().json(json!({"code":0,"data":{"items":users,"pages":1}}));
    }
    assert_eq!(body["role"], "user");
    assert_eq!(body["balance"], 0);
    assert_eq!(body["allowed_groups"], json!([7]));
    assert_eq!(body["restrict_public_groups"], true);
    if state
      .users
      .iter()
      .any(|user| user["email"] == body["email"])
    {
      return HttpResponse::Conflict().finish();
    }
    let id = state.users.len() as i64 + 1;
    let user = json!({"id":id,"email":body["email"],"username":body["username"],"role":"user","status":"active"});
    state.users.push(user.clone());
    state
      .passwords
      .insert(id, body["password"].as_str().unwrap().to_owned());
    if std::mem::take(&mut state.fail_after_user) {
      return HttpResponse::InternalServerError().finish();
    }
    return HttpResponse::Ok().json(json!({"code":0,"data":user}));
  }
  if let Some(id) = path
    .strip_prefix("/api/v1/admin/users/")
    .and_then(|path| path.strip_suffix("/api-keys"))
  {
    let id: i64 = id.parse().unwrap();
    let keys: Vec<_> = state
      .keys
      .iter()
      .filter(|key| key["user_id"] == id)
      .cloned()
      .collect();
    return HttpResponse::Ok().json(json!({"code":0,"data":{"items":keys,"pages":1}}));
  }
  if path == "/api/v1/auth/login" {
    assert!(!req.headers().contains_key("x-api-key"));
    if body["email"] == "admin@test.invalid" {
      assert_eq!(body["password"], "admin-test-password");
      state.admin_logins += 1;
      return HttpResponse::Ok().json(json!({"code":0,"data":{"access_token":"admin-token","expires_in":3600,"user":{"id":999,"role":"admin","status":"active"}}}));
    }
    let user = state
      .users
      .iter()
      .find(|user| user["email"] == body["email"])
      .unwrap()
      .clone();
    let id = user["id"].as_i64().unwrap();
    assert_eq!(state.passwords[&id], body["password"].as_str().unwrap());
    state.logins += 1;
    return HttpResponse::Ok()
      .json(json!({"code":0,"data":{"user":user,"access_token":format!("user-token-{id}")}}));
  }
  if path == "/api/v1/keys" {
    assert_eq!(req.method(), "POST");
    assert!(!req.headers().contains_key("x-api-key"));
    let id: i64 = req
      .headers()
      .get("Authorization")
      .unwrap()
      .to_str()
      .unwrap()
      .strip_prefix("Bearer user-token-")
      .unwrap()
      .parse()
      .unwrap();
    assert!(state.users.iter().any(|user| user["id"] == id));
    assert_eq!(body["group_id"], 7);
    assert!(req.headers().contains_key("Idempotency-Key"));
    if state
      .keys
      .iter()
      .any(|key| key["key"] == body["custom_key"])
    {
      return HttpResponse::Conflict().finish();
    }
    let key = json!({"id":state.keys.len() as i64 + 1,"user_id":id,"key":body["custom_key"],"status":"active","group_id":7});
    state.keys.push(key.clone());
    if std::mem::take(&mut state.fail_after_key) {
      return HttpResponse::InternalServerError().finish();
    }
    return HttpResponse::Ok().json(json!({"code":0,"data":key}));
  }
  HttpResponse::NotFound().finish()
}

async fn fixture() -> Fixture {
  let listener = TcpListener::bind("127.0.0.1:0").unwrap();
  let base = format!("http://{}", listener.local_addr().unwrap());
  let state = Arc::new(Mutex::new(GatewayState::default()));
  let shared = state.clone();
  let server = HttpServer::new(move || {
    App::new()
      .app_data(web::Data::new(shared.clone()))
      .default_service(web::to(mock_gateway))
  })
  .workers(1)
  .disable_signals()
  .listen(listener)
  .unwrap()
  .run();
  let handle = server.handle();
  tokio::spawn(server);
  Fixture {
    gateway: Sub2ApiGateway {
      client: reqwest::Client::new(),
      base,
      management: ManagementAuth::Key("test-admin".to_owned().into()),
      credits_per_usd: 100,
      provisioning: Some(Provisioning {
        secret: "test-provision-secret-only-for-fixtures".to_owned().into(),
        group_id: 7,
      }),
    },
    state,
    server: handle,
  }
}

#[test]
fn account_credentials_are_stable_separated_and_user_specific() {
  let config = Provisioning {
    secret: "test-provision-secret-only-for-fixtures".to_owned().into(),
    group_id: 7,
  };
  let id = Uuid::new_v4();
  let password = config.credential(id, "password");
  assert_eq!(password, config.credential(id, "password"));
  assert_ne!(password, config.credential(id, "api-key"));
  assert_ne!(password, config.credential(Uuid::new_v4(), "password"));
  assert_eq!(password.len(), 64);
}

#[test]
fn recovered_user_and_key_must_match_the_provisioning_identity() {
  let mut user = json!({"id":1,"email":"owned@ponynotes.invalid","role":"user","status":"active"});
  assert_eq!(validate_user(&user, "owned@ponynotes.invalid").unwrap(), 1);
  assert!(validate_user(&user, "another@ponynotes.invalid").is_err());
  user["role"] = json!("admin");
  assert!(validate_user(&user, "owned@ponynotes.invalid").is_err());
  let key = json!({"id":1,"key":"expected","user_id":2,"status":"active"});
  assert!(find_owned_key(&[key], 1, "expected").is_err());
}

#[actix_rt::test]
async fn each_user_gets_their_own_account_and_key_and_retries_reuse_them() {
  let f = fixture().await;
  let config = f.gateway.provisioning.as_ref().unwrap();
  let first_id = Uuid::new_v4();
  let first = f
    .gateway
    .provision_account(config, first_id, 10, "小马用户同名")
    .await
    .unwrap();
  assert_eq!(
    first,
    f.gateway
      .provision_account(config, first_id, 10, "小马用户同名")
      .await
      .unwrap()
  );
  let second = f
    .gateway
    .provision_account(config, Uuid::new_v4(), 20, "小马用户同名")
    .await
    .unwrap();
  assert_ne!(first.0, second.0);
  assert_ne!(first.1, second.1);
  let state = f.state.lock().unwrap();
  assert_eq!(state.users.len(), 2);
  assert_eq!(state.users[0]["username"], "小马用户同名");
  assert_eq!(state.users[1]["username"], "小马用户同名");
  assert_eq!(state.keys.len(), 2);
  assert_eq!(state.logins, 2);
  assert_ne!(state.keys[0]["key"], state.keys[1]["key"]);
}

#[actix_rt::test]
async fn admin_login_mode_caches_and_renews_only_management_sessions() {
  let mut f = fixture().await;
  let cached = Arc::new(tokio::sync::Mutex::new(None));
  f.gateway.management = ManagementAuth::Login {
    email: "admin@test.invalid".into(),
    password: "admin-test-password".to_owned().into(),
    cached: cached.clone(),
  };
  let config = f.gateway.provisioning.as_ref().unwrap();
  f.gateway
    .provision_account(config, Uuid::new_v4(), 10, "小马用户一")
    .await
    .unwrap();
  f.gateway
    .provision_account(config, Uuid::new_v4(), 20, "小马用户二")
    .await
    .unwrap();
  assert_eq!(f.state.lock().unwrap().admin_logins, 1);
  *cached.lock().await = Some((
    Instant::now() - Duration::from_secs(1),
    "expired-token".to_owned().into(),
  ));
  f.gateway
    .provision_account(config, Uuid::new_v4(), 30, "小马用户三")
    .await
    .unwrap();
  let state = f.state.lock().unwrap();
  assert_eq!(state.admin_logins, 2);
  assert_eq!(state.logins, 3);
  assert_eq!(state.keys.len(), 3);
}

#[actix_rt::test]
async fn lost_user_or_key_responses_resume_without_creating_duplicates() {
  for fail_user in [true, false] {
    let f = fixture().await;
    {
      let mut state = f.state.lock().unwrap();
      state.fail_after_user = fail_user;
      state.fail_after_key = !fail_user;
    }
    let id = Uuid::new_v4();
    let config = f.gateway.provisioning.as_ref().unwrap();
    assert!(f
      .gateway
      .provision_account(config, id, 10, "小马用户")
      .await
      .is_err());
    assert_eq!(
      f.gateway
        .provision_account(config, id, 10, "小马用户")
        .await
        .unwrap(),
      (1, 1)
    );
    let state = f.state.lock().unwrap();
    assert_eq!(state.users.len(), 1);
    assert_eq!(state.keys.len(), 1);
  }
}

#[actix_rt::test]
async fn revoked_accounts_and_keys_are_not_replaced_automatically() {
  let f = fixture().await;
  let config = f.gateway.provisioning.as_ref().unwrap();
  let id = Uuid::new_v4();
  f.gateway
    .provision_account(config, id, 10, "小马用户")
    .await
    .unwrap();
  f.state.lock().unwrap().users[0]["status"] = json!("disabled");
  assert!(f
    .gateway
    .provision_account(config, id, 10, "小马用户")
    .await
    .is_err());
  f.state.lock().unwrap().users[0]["status"] = json!("active");
  f.state.lock().unwrap().keys[0]["status"] = json!("inactive");
  assert!(f
    .gateway
    .provision_account(config, id, 10, "小马用户")
    .await
    .is_err());
  let state = f.state.lock().unwrap();
  assert_eq!(state.users.len(), 1);
  assert_eq!(state.keys.len(), 1);
}

#[sqlx::test(migrations = false)]
async fn concurrent_cloud_instances_provision_once_and_keep_bindings_unique(pool: PgPool) {
  sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"; CREATE TABLE af_user (uid BIGINT PRIMARY KEY, name TEXT NOT NULL DEFAULT '测试小马用户', deleted_at TIMESTAMPTZ);")
    .execute(&pool).await.unwrap();
  sqlx::raw_sql(include_str!(
    "../../../../migrations/20261106000000_agent_sub2api_accounts.sql"
  ))
  .execute(&pool)
  .await
  .unwrap();
  sqlx::raw_sql(include_str!(
    "../../../../migrations/20261106010000_agent_auto_provision.sql"
  ))
  .execute(&pool)
  .await
  .unwrap();
  sqlx::query("INSERT INTO af_user(uid) VALUES (10), (20), (30)")
    .execute(&pool)
    .await
    .unwrap();
  let f = fixture().await;
  let requests = (0..6).map(|_| {
    let gateway = f.gateway.clone();
    let pool = pool.clone();
    async move { gateway.account_for_user(&pool, 10).await }
  });
  for result in futures_util::future::join_all(requests).await {
    assert_eq!(result.unwrap(), (1, 1));
  }
  {
    let state = f.state.lock().unwrap();
    assert_eq!(state.users.len(), 1);
    assert_eq!(state.keys.len(), 1);
    assert_eq!(state.users[0]["username"], "测试小马用户");
  }
  assert_eq!(f.gateway.account_for_user(&pool, 20).await.unwrap(), (2, 2));
  assert!(sqlx::query(
    "UPDATE af_agent_accounts SET sub2api_user_id = 1, sub2api_key_id = 1 WHERE uid = 20"
  )
  .execute(&pool)
  .await
  .is_err());
  // Existing bindings remain usable without new provisioning configuration.
  let mut legacy = f.gateway.clone();
  legacy.provisioning = None;
  assert_eq!(legacy.account_for_user(&pool, 10).await.unwrap(), (1, 1));
  // External creation succeeded but the IDs were never committed locally.
  sqlx::query("INSERT INTO af_agent_accounts(uid) VALUES (30)")
    .execute(&pool)
    .await
    .unwrap();
  let id: Uuid = sqlx::query_scalar("SELECT provision_id FROM af_agent_accounts WHERE uid = 30")
    .fetch_one(&pool)
    .await
    .unwrap();
  let created = f
    .gateway
    .provision_account(
      f.gateway.provisioning.as_ref().unwrap(),
      id,
      30,
      "测试小马用户",
    )
    .await
    .unwrap();
  assert_eq!(
    f.gateway.account_for_user(&pool, 30).await.unwrap(),
    created
  );
  let state = f.state.lock().unwrap();
  assert_eq!(state.users.len(), 3);
  assert_eq!(state.keys.len(), 3);
}
