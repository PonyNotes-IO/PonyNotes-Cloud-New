//! Preserve the deployed Agent's isolated tenant indexes and conversation identities.
//! These are internal Agent indexes; the public library uses one shared KB per workspace.
use super::{reconcile, user_token};
use crate::{array, not_found, optional_array, text, Client};
use anyhow::{anyhow, ensure, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

async fn persist(connection: &mut PgConnection, uid: &str, record: &Value) -> Result<()> {
  super::persist(connection, "users", Some(uid), record).await
}

async fn provision(
  client: &Client,
  connection: &mut PgConnection,
  uid: &str,
  record: &mut Value,
) -> Result<()> {
  if record.is_null() {
    *record = json!({"email":format!("pn-{uid}@cloud.ponynotes.local"),"password":format!("Pn!{}7aA",&Uuid::new_v4().simple().to_string()[..24]),"workspaces":{}});
    persist(connection, uid, record).await?;
  }
  if record["user_id"].is_null() {
    // Older provisioning generated 38 characters; WeKnora rejects passwords over 32.
    if record["password"].as_str().is_some_and(|p| p.len() > 32) {
      record["password"] = json!(format!(
        "Pn!{}7aA",
        &Uuid::new_v4().simple().to_string()[..24]
      ));
      persist(connection, uid, record).await?;
    }
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
    let created=client.agent_request(text(&login,"token")?,"system/admin/users/create",Method::POST,Some(&json!({"username":format!("pn-{uid}"),"email":record["email"],"password":record["password"]}))).await?;
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
      client.agent_request(token,&path,Method::POST,Some(&json!({"name":"PonyNotes sync","full_access":false,"knowledge_base_ids":[],"capabilities":["retrieve","ingest","manage_kbs"]}))).await?["data"].clone()
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
      let desired=client.scan(&token,wid,uid).await?;
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

pub(super) async fn run(connection: &mut PgConnection, client: &Client) -> Result<Value> {
  let enabled: bool =
    sqlx::query_scalar("SELECT to_regclass('public.af_weknora_agent_sessions') IS NOT NULL")
      .fetch_one(&mut *connection)
      .await?;
  if !enabled {
    return Ok(json!({"workspaces":[],"errors":[]}));
  }
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
  super::persist(connection, "agent_last_run", None, &report).await?;
  Ok(report)
}
