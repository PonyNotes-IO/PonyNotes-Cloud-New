use crate::notes::Notes;
use crate::{array, managed, not_found, optional_array, text, Client};
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Defaults off. Existing deployments keep working until migration explicitly enables it.
pub async fn run(pool: PgPool) {
  if !crate::enabled() {
    return;
  }
  let client = match Client::from_env() {
    Ok(client) => client,
    Err(_) => {
      tracing::error!("WeKnora client configuration failed");
      return;
    },
  };
  loop {
    match run_once(&pool, &client).await {
      Ok(report) => tracing::info!(
        workspaces = report["workspaces"].as_array().map(Vec::len),
        errors = report["errors"].as_array().map(Vec::len),
        "WeKnora sync completed"
      ),
      Err(_) => tracing::error!("WeKnora sync failed; existing knowledge preserved"),
    }
    // ponytail: keep the existing five-minute scan; use the existing Redis queue if scans outgrow it.
    tokio::time::sleep(Duration::from_secs(300)).await;
  }
}

pub async fn run_once(pool: &PgPool, client: &Client) -> Result<Value> {
  let mut connection = pool.acquire().await?;
  let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(781142063)")
    .fetch_one(&mut *connection)
    .await?;
  if !locked {
    return Ok(json!({"workspaces":[],"errors":[],"skipped":"already_running"}));
  }
  let result = cycle(&mut connection, client).await;
  // Close rather than pool a session whose advisory lock could not be released.
  if sqlx::query("SELECT pg_advisory_unlock(781142063)")
    .execute(&mut *connection)
    .await
    .is_err()
  {
    connection.close().await?;
  }
  result
}

async fn persist(connection: &mut PgConnection, uid: &str, record: &Value) -> Result<()> {
  let result = sqlx::query("UPDATE af_weknora_sync_state SET state=jsonb_set(state,ARRAY['users',$1],$2),updated_at=now() WHERE singleton")
    .bind(uid).bind(record).execute(connection).await?;
  ensure!(
    result.rows_affected() == 1,
    "sync state missing; restore legacy mapping before enabling"
  );
  Ok(())
}

fn user_token(user: &Value) -> Result<String> {
  let secret =
    std::env::var("APPFLOWY_GOTRUE_JWT_SECRET").context("missing Cloud JWT configuration")?;
  let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
  jsonwebtoken::encode(&jsonwebtoken::Header::default(),&json!({
    "sub":text(user,"uuid")?,"aud":"authenticated","iat":now,"exp":now+600,
    "email":user["email"].as_str().unwrap_or(""),"phone":"","role":"authenticated","app_metadata":{},"user_metadata":{}
  }),&jsonwebtoken::EncodingKey::from_secret(secret.as_bytes())).map_err(|_|anyhow!("cannot sign user token"))
}

async fn provision(
  client: &Client,
  connection: &mut PgConnection,
  uid: &str,
  record: &mut Value,
) -> Result<()> {
  if record.is_null() {
    *record = json!({"email":format!("pn-{uid}@cloud.ponynotes.local"),"password":format!("Pn!{}7aA",Uuid::new_v4().simple()),"workspaces":{}});
    persist(connection, uid, record).await?;
  }
  if record["user_id"].is_null() {
    let admin_path = std::env::var("PONYNOTES_WEKNORA_ADMIN_FILE")
      .context("missing WeKnora administrator file")?;
    let credentials: Value = serde_json::from_slice(&tokio::fs::read(admin_path).await?)?;
    let login = client
      .request(
        &(client.base.clone() + "auth/login"),
        Method::POST,
        None,
        Some(&credentials),
      )
      .await?;
    let created=client.bearer(text(&login,"token")?,"system/admin/users/create",&json!({"username":format!("pn-{uid}"),"email":record["email"],"password":record["password"]})).await?;
    let own = client
      .request(
        &(client.base.clone() + "auth/login"),
        Method::POST,
        None,
        Some(&json!({"email":record["email"],"password":record["password"]})),
      )
      .await?;
    ensure!(
      own["user"]["id"].is_string() && own["user"]["id"] == created["user"]["id"],
      "provisioned identity mismatch"
    );
    record["user_id"] = own["user"]["id"].clone();
    record["tenant_id"] = own["active_tenant"]["id"].clone();
    ensure!(!record["tenant_id"].is_null(), "missing tenant");
    persist(connection, uid, record).await?;
  }
  if record["api_key"].is_null() {
    let own = client
      .request(
        &(client.base.clone() + "auth/login"),
        Method::POST,
        None,
        Some(&json!({"email":record["email"],"password":record["password"]})),
      )
      .await?;
    ensure!(
      own["user"]["id"] == record["user_id"] && own["active_tenant"]["id"] == record["tenant_id"],
      "provisioned tenant mismatch"
    );
    let token = text(&own, "token")?;
    let path = format!("tenants/{}/api-keys", display_id(&record["tenant_id"])?);
    let keys = client
      .request(
        &(client.base.clone() + &path),
        Method::GET,
        Some(("Authorization", &format!("Bearer {token}"))),
        None,
      )
      .await?;
    let key = optional_array(&keys["data"])?
      .into_iter()
      .find(|k| k["name"] == "PonyNotes sync")
      .cloned();
    let key = if let Some(key) = key {
      key
    } else {
      client.bearer(token,&path,&json!({"name":"PonyNotes sync","full_access":false,"knowledge_base_ids":[],"capabilities":["retrieve","ingest","manage_kbs"]})).await?["data"].clone()
    };
    record["api_key"] = key
      .get("token")
      .or_else(|| key.get("api_key"))
      .cloned()
      .ok_or_else(|| anyhow!("missing tenant key"))?;
    persist(connection, uid, record).await?;
  }
  Ok(())
}

fn display_id(value: &Value) -> Result<String> {
  if let Some(id) = value.as_str() {
    Ok(id.to_owned())
  } else if value.is_number() {
    Ok(value.to_string())
  } else {
    anyhow::bail!("invalid identifier")
  }
}

pub(crate) async fn reconcile(
  client: &Client,
  key: &str,
  kb: &Value,
  desired: &Notes,
) -> Result<Value> {
  let id = text(kb, "id")?;
  let mut existing = HashMap::new();
  let mut duplicates = Vec::new();
  for entry in client.entries(key, id).await? {
    let channel = entry["channel"].as_str().unwrap_or("").to_owned();
    if !managed(&channel) || entry["parse_status"] == "deleting" {
      continue;
    }
    if existing.contains_key(&channel) {
      duplicates.push(entry);
    } else {
      existing.insert(channel, entry);
    }
  }
  let status = if kb["embedding_model_id"]
    .as_str()
    .is_some_and(|s| !s.is_empty())
  {
    "publish"
  } else {
    "draft"
  };
  let mut counts = json!({"created":0,"updated":0,"unchanged":0,"deleted":0});
  for (channel, content) in desired {
    let payload = json!({"title":content["title"],"content":content["content"],"status":status});
    if let Some(entry) = existing.get(channel) {
      let unchanged = entry["title"] == payload["title"]
        && entry["metadata"]["content"] == payload["content"]
        && entry["parse_status"] != "failed"
        && ((status == "draft" && entry["parse_status"] == "draft")
          || (status == "publish" && entry["parse_status"] != "draft"));
      if unchanged {
        increment(&mut counts, "unchanged");
        continue;
      }
      client
        .weknora(
          key,
          &format!("knowledge/manual/{}", text(entry, "id")?),
          Method::PUT,
          Some(&payload),
        )
        .await?;
      increment(&mut counts, "updated");
    } else {
      let mut payload = payload;
      payload["channel"] = json!(channel);
      client
        .weknora(
          key,
          &format!("knowledge-bases/{id}/knowledge/manual"),
          Method::POST,
          Some(&payload),
        )
        .await?;
      increment(&mut counts, "created");
    }
  }
  // No pruning until the source scan and every destination write have succeeded.
  for entry in existing
    .iter()
    .filter(|(channel, _)| !desired.contains_key(*channel))
    .map(|(_, v)| v)
    .chain(duplicates.iter())
  {
    client
      .weknora(
        key,
        &format!("knowledge/{}", text(entry, "id")?),
        Method::DELETE,
        None,
      )
      .await?;
    increment(&mut counts, "deleted");
  }
  Ok(counts)
}

fn increment(counts: &mut Value, key: &str) {
  counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
}

async fn remove_workspace(client: &Client, key: &str, record: &mut Value, wid: &str) -> Result<()> {
  let id = text(&record["workspaces"][wid], "id")?;
  if let Err(error) = client
    .weknora(key, &format!("knowledge-bases/{id}"), Method::DELETE, None)
    .await
  {
    if !not_found(&error) {
      return Err(error);
    }
  }
  record["workspaces"]
    .as_object_mut()
    .ok_or_else(|| anyhow!("invalid workspace mapping"))?
    .remove(wid);
  Ok(())
}

async fn sync_user(
  client: &Client,
  connection: &mut PgConnection,
  user: &Value,
  record: &mut Value,
  report: &mut Value,
) -> Result<()> {
  let uid = text(user, "uuid")?;
  let token = user_token(user)?;
  let workspaces = client.cloud(&token, "workspace").await?;
  let workspaces = array(&workspaces)?;
  let allowed = workspaces
    .iter()
    .map(|w| text(w, "workspace_id").map(str::to_owned))
    .collect::<Result<HashSet<_>>>()?;
  provision(client, connection, uid, record).await?;
  let key = text(record, "api_key")?.to_owned();
  let stale = record["workspaces"]
    .as_object()
    .ok_or_else(|| anyhow!("invalid workspace mapping"))?
    .keys()
    .filter(|w| !allowed.contains(*w))
    .cloned()
    .collect::<Vec<_>>();
  for wid in stale {
    remove_workspace(client, &key, record, &wid).await?;
    persist(connection, uid, record).await?;
  }
  for workspace in workspaces {
    let wid = text(workspace, "workspace_id")?;
    Uuid::parse_str(wid)?;
    let result: Result<Value>=async {
      let desired=client.scan(&token,wid).await?;
      if record["workspaces"].get(wid).is_none() {
        record["workspaces"][wid]=json!({"id":Uuid::new_v4().to_string(),"notes":{}});
        persist(connection,uid,record).await?;
      }
      let id=text(&record["workspaces"][wid],"id")?;
      let kb=match client.weknora(&key,&format!("knowledge-bases/{id}"),Method::GET,None).await {
        Ok(response)=>response["data"].clone(),
        Err(error) if not_found(&error)=>{
          client.weknora(&key,"knowledge-bases",Method::POST,Some(&json!({"id":id,"name":format!("小马笔记 · {}",workspace["workspace_name"].as_str().or_else(||workspace["name"].as_str()).unwrap_or(wid)),"type":"document","description":"小马云端笔记自动同步；请在小马笔记中编辑原文。"}))).await?["data"].clone()
        },
        Err(error)=>return Err(error),
      };
      let counts=reconcile(client,&key,&kb,&desired).await?;
      record["workspaces"][wid]["notes"]=Value::Object(desired.keys().map(|c|(c.clone(),json!({}))).collect());
      persist(connection,uid,record).await?;
      Ok(json!({"user":uid,"workspace":wid,"parts":desired.len(),"sync":counts}))
    }.await;
    match result {
      Ok(result) => report["workspaces"].as_array_mut().unwrap().push(result),
      Err(_) => report["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"user":uid,"workspace":wid,"error":"sync_failed"})),
    }
  }
  Ok(())
}

async fn cycle(connection: &mut PgConnection, client: &Client) -> Result<Value> {
  let state: Value = sqlx::query_scalar("SELECT state FROM af_weknora_sync_state WHERE singleton")
    .fetch_one(&mut *connection)
    .await
    .context("sync state missing; import original mapping before enabling")?;
  let mut accounts = state["users"]
    .as_object()
    .ok_or_else(|| anyhow!("invalid sync state"))?
    .clone();
  let users:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('uuid',u.uuid,'email',u.email) FROM af_user u JOIN auth.users a ON a.id=u.uuid WHERE a.deleted_at IS NULL AND (a.banned_until IS NULL OR a.banned_until<now())")
    .fetch_all(&mut *connection).await?;
  let mut report = json!({"at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),"users":users.len(),"workspaces":[],"errors":[]});
  let active = users
    .iter()
    .map(|u| text(u, "uuid").map(str::to_owned))
    .collect::<Result<HashSet<_>>>()?;
  for user in users {
    let uid = text(&user, "uuid")?;
    let record = accounts.entry(uid.to_owned()).or_insert(Value::Null);
    if sync_user(client, connection, &user, record, &mut report)
      .await
      .is_err()
    {
      report["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!({"user":uid,"error":"sync_failed"}));
    }
  }
  for (uid, record) in &mut accounts {
    if active.contains(uid) || record["api_key"].is_null() {
      continue;
    }
    let key = text(record, "api_key")?.to_owned();
    let stale = record["workspaces"]
      .as_object()
      .ok_or_else(|| anyhow!("invalid mapping"))?
      .keys()
      .cloned()
      .collect::<Vec<_>>();
    for wid in stale {
      if remove_workspace(client, &key, record, &wid).await.is_ok() {
        persist(connection, uid, record).await?;
      } else {
        report["errors"]
          .as_array_mut()
          .unwrap()
          .push(json!({"user":uid,"workspace":wid,"error":"cleanup_failed"}));
      }
    }
  }
  sqlx::query("UPDATE af_weknora_sync_state SET last_run=$1,updated_at=now() WHERE singleton")
    .bind(&report)
    .execute(connection)
    .await?;
  Ok(report)
}
