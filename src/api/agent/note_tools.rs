//! Streamable HTTP MCP relay. Actual note access and approval stay in Flutter.
use super::conversations::{unavailable, Document};
use crate::{biz::authentication::jwt::Authorization, state::AppState};
use access_control::act::Action;
use actix_web::{error, web, HttpRequest, HttpResponse};
use redis::AsyncCommands;
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

fn document_key(uid: i64, wid: Uuid, doc: Uuid) -> String {
  format!("agent:notes:document:{uid}:{wid}:{doc}")
}
fn token_key(token: &str) -> String {
  format!("agent:notes:token:{token}")
}
fn lease_key(token: &str) -> String {
  format!("agent:notes:lease:{token}")
}
fn queue_key(token: &str) -> String {
  format!("agent:notes:queue:{token}")
}

pub(super) async fn authorize(
  auth: &Authorization,
  path: &Document,
  state: &AppState,
) -> actix_web::Result<i64> {
  let uid = state.user_cache.get_user_uid(&auth.uuid()?).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &path.as_ref().0, Action::Read)
    .await
    .map_err(|_| error::ErrorForbidden("无权操作此工作区"))?;
  Ok(uid)
}

async fn record(state: &AppState, token: &str) -> actix_web::Result<Value> {
  let mut redis = state.redis_connection_manager.clone();
  let raw: Option<String> = redis.get(token_key(token)).await.map_err(unavailable)?;
  serde_json::from_str(&raw.ok_or_else(|| error::ErrorUnauthorized("笔记工具授权已失效"))?)
    .map_err(unavailable)
}

pub(super) async fn channel(
  state: &AppState,
  uid: i64,
  wid: Uuid,
  doc: Uuid,
  client_id: Uuid,
) -> actix_web::Result<String> {
  let mut redis = state.redis_connection_manager.clone();
  let token: Option<String> = redis
    .get(document_key(uid, wid, doc))
    .await
    .map_err(unavailable)?;
  let token = token.ok_or_else(|| error::ErrorConflict("请重新连接小马 Agent 以启用笔记操作"))?;
  let value = record(state, &token).await?;
  if value["client_id"] != client_id.to_string() {
    return Err(error::ErrorConflict(
      "笔记工具已连接到另一个窗口，请重新连接",
    ));
  }
  Ok(token)
}

#[derive(Deserialize)]
pub(super) struct Registration {
  client_id: Uuid,
  tools: Vec<Value>,
}

fn valid_tools(tools: &[Value]) -> bool {
  tools.len() <= 100
    && tools.iter().all(|tool| {
      tool["name"].as_str().is_some_and(|name| {
        !name.is_empty()
          && name.len() <= 100
          && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
      }) && tool["inputSchema"].is_object()
        && tool.to_string().len() <= 32000
    })
}

pub(super) async fn register(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  payload: web::Json<Registration>,
) -> actix_web::Result<HttpResponse> {
  if !valid_tools(&payload.tools) {
    return Err(error::ErrorBadRequest("笔记工具定义无效"));
  }
  let uid = authorize(&auth, &path, &state).await?;
  let mut redis = state.redis_connection_manager.clone();
  let key = document_key(uid, path.as_ref().0, path.as_ref().1);
  let previous: Option<String> = redis.get(&key).await.map_err(unavailable)?;
  let token = previous.unwrap_or_else(|| format!("pn-notes-{}", nanoid::nanoid!(48)));
  let value = json!({"uid":uid,"workspace":path.as_ref().0,"document":path.as_ref().1,"client_id":payload.client_id,"generation":Uuid::new_v4(),"tools":payload.tools});
  let _: () = redis::pipe()
    .atomic()
    .set_ex(&key, &token, 86400)
    .set_ex(token_key(&token), value.to_string(), 86400)
    .set_ex(
      lease_key(&token),
      value["generation"].as_str().unwrap_or(""),
      15,
    )
    .query_async(&mut redis)
    .await
    .map_err(unavailable)?;
  Ok(HttpResponse::NoContent().finish())
}

#[derive(Deserialize)]
pub(super) struct Poll {
  client_id: Uuid,
}
pub(super) async fn poll(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  query: web::Query<Poll>,
) -> actix_web::Result<HttpResponse> {
  let uid = authorize(&auth, &path, &state).await?;
  let token = channel(
    &state,
    uid,
    path.as_ref().0,
    path.as_ref().1,
    query.client_id,
  )
  .await?;
  let mut redis = state.redis_connection_manager.clone();
  let _: bool = redis
    .expire(lease_key(&token), 15)
    .await
    .map_err(unavailable)?;
  let pending: Vec<String> = redis
    .lrange(queue_key(&token), 0, 0)
    .await
    .map_err(unavailable)?;
  let mut calls = Vec::new();
  for id in pending {
    let raw: Option<String> = redis
      .get(format!("agent:notes:pending:{id}"))
      .await
      .map_err(unavailable)?;
    if let Some(raw) = raw {
      let value: Value = serde_json::from_str(&raw).map_err(unavailable)?;
      if value["client_id"] == query.client_id.to_string()
        && value["generation"] == record(&state, &token).await?["generation"]
      {
        calls.push(value["call"].clone());
      } else {
        let _: usize = redis
          .lrem(queue_key(&token), 1, &id)
          .await
          .map_err(unavailable)?;
      }
    } else {
      let _: usize = redis
        .lrem(queue_key(&token), 1, &id)
        .await
        .map_err(unavailable)?;
    }
  }
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"calls":calls})),
  )
}

#[derive(Deserialize)]
pub(super) struct ResultPayload {
  client_id: Uuid,
  id: Uuid,
  result: Value,
}
pub(super) async fn result(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  payload: web::Json<ResultPayload>,
) -> actix_web::Result<HttpResponse> {
  let uid = authorize(&auth, &path, &state).await?;
  let token = channel(
    &state,
    uid,
    path.as_ref().0,
    path.as_ref().1,
    payload.client_id,
  )
  .await?;
  if !payload.result["content"].is_array() || payload.result.to_string().len() > 512 * 1024 {
    return Err(error::ErrorBadRequest("笔记工具结果无效"));
  }
  let mut redis = state.redis_connection_manager.clone();
  let raw: Option<String> = redis
    .get(format!("agent:notes:pending:{}", payload.id))
    .await
    .map_err(unavailable)?;
  let pending: Value =
    serde_json::from_str(&raw.ok_or_else(|| error::ErrorConflict("此操作已结束或取消"))?)
      .map_err(unavailable)?;
  if pending["token"] != token
    || pending["client_id"] != payload.client_id.to_string()
    || pending["generation"] != record(&state, &token).await?["generation"]
  {
    return Err(error::ErrorForbidden("此操作不属于当前会话"));
  }
  let _: Option<String> = redis::cmd("SET")
    .arg(format!("agent:notes:result:{}", payload.id))
    .arg(payload.result.to_string())
    .arg("NX")
    .arg("EX")
    .arg(150)
    .query_async(&mut redis)
    .await
    .map_err(unavailable)?;
  let _: usize = redis
    .lrem(queue_key(&token), 1, payload.id.to_string())
    .await
    .map_err(unavailable)?;
  Ok(HttpResponse::NoContent().finish())
}

pub(super) async fn cancel(
  state: &AppState,
  uid: i64,
  wid: Uuid,
  doc: Uuid,
) -> actix_web::Result<()> {
  super::revoke_conversation_model(state, uid, wid, doc).await?;
  let mut redis = state.redis_connection_manager.clone();
  let token: Option<String> = redis
    .get(document_key(uid, wid, doc))
    .await
    .map_err(unavailable)?;
  if let Some(token) = token {
    let mut value = record(state, &token).await?;
    value["generation"] = json!(Uuid::new_v4());
    let _: () = redis::pipe()
      .atomic()
      .set_ex(token_key(&token), value.to_string(), 86400)
      .del(lease_key(&token))
      .del(queue_key(&token))
      .query_async(&mut redis)
      .await
      .map_err(unavailable)?;
  }
  Ok(())
}
pub(super) async fn revoke(
  auth: Authorization,
  path: Document,
  state: web::Data<AppState>,
  query: web::Query<Poll>,
) -> actix_web::Result<HttpResponse> {
  let uid = authorize(&auth, &path, &state).await?;
  channel(
    &state,
    uid,
    path.as_ref().0,
    path.as_ref().1,
    query.client_id,
  )
  .await?;
  cancel(&state, uid, path.as_ref().0, path.as_ref().1).await?;
  Ok(HttpResponse::NoContent().finish())
}

fn tool_error(message: &str) -> Value {
  json!({"content":[{"type":"text","text":message}],"isError":true})
}

pub(super) async fn rpc(
  req: HttpRequest,
  state: web::Data<AppState>,
  payload: web::Json<Value>,
) -> actix_web::Result<HttpResponse> {
  let token = req
    .headers()
    .get("Authorization")
    .and_then(|h| h.to_str().ok())
    .and_then(|h| h.strip_prefix("Bearer "))
    .filter(|s| {
      s.starts_with("pn-notes-")
        && s.len() == 57
        && s
          .bytes()
          .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
    .ok_or_else(|| error::ErrorUnauthorized("笔记工具授权无效"))?;
  let value = record(&state, token).await?;
  let mut redis = state.redis_connection_manager.clone();
  let lease: Option<String> = redis.get(lease_key(token)).await.map_err(unavailable)?;
  if lease.as_deref() != value["generation"].as_str() {
    return Err(error::ErrorUnauthorized("笔记客户端已离线或撤权"));
  }
  if payload["id"].is_null() {
    return Ok(HttpResponse::Accepted().finish());
  }
  let result=match payload["method"].as_str() {
    Some("initialize")=>json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"小马笔记","version":"1.0.0"},"instructions":"小马 Agent 的笔记操作入口。客户端负责实际授权、审批与执行。"}),
    Some("ping")=>json!({}),
    Some("tools/list")=>json!({"tools":value["tools"]}),
    Some("tools/call")=>{
      let name=payload["params"]["name"].as_str().unwrap_or("");
      let mut args=payload["params"].get("arguments").cloned().unwrap_or(json!({}));
      if !value["tools"].as_array().is_some_and(|tools|tools.iter().any(|tool|tool["name"]==name)) || !args.is_object() || (args.get("workspace_id").is_some() && args["workspace_id"]!=value["workspace"]) { tool_error("未知、禁用或越权的笔记工具") }
      else {
        let uid=value["uid"].as_i64().ok_or_else(||unavailable("invalid note identity"))?;
        let wid=Uuid::parse_str(value["workspace"].as_str().unwrap_or("")).map_err(unavailable)?;
        state.workspace_access_control.enforce_action(&uid,&wid,Action::Read).await.map_err(|_|error::ErrorForbidden("笔记权限已失效"))?;
        if let Some(properties)=value["tools"].as_array().and_then(|tools|tools.iter().find(|tool|tool["name"]==name)).and_then(|tool|tool["inputSchema"]["properties"].as_object()) { if properties.contains_key("workspace_id") { args["workspace_id"]=value["workspace"].clone(); } }
        let id=Uuid::new_v4();let pending_key=format!("agent:notes:pending:{id}");
        let pending=json!({"token":token,"client_id":value["client_id"],"generation":value["generation"],"call":{"id":id,"name":name,"arguments":args}});
        let _:()=redis::pipe().atomic().set_ex(&pending_key,pending.to_string(),150).rpush(queue_key(token),id.to_string()).expire(queue_key(token),150).query_async(&mut redis).await.map_err(unavailable)?;
        let mut output=tool_error("笔记操作未完成；请查询实际结果，不要重复写入");
        for _ in 0..560 {
          let response:Option<String>=redis.get(format!("agent:notes:result:{id}")).await.map_err(unavailable)?;
          if let Some(response)=response { output=serde_json::from_str(&response).map_err(unavailable)?;break; }
          let lease:Option<String>=redis.get(lease_key(token)).await.map_err(unavailable)?;
          if lease.as_deref()!=value["generation"].as_str() { output=tool_error("笔记客户端已断开、停止或撤权");break; }
          tokio::time::sleep(Duration::from_millis(250)).await;
        }
        let _:()=redis::pipe().del(&pending_key).del(format!("agent:notes:result:{id}")).lrem(queue_key(token),1,id.to_string()).query_async(&mut redis).await.map_err(unavailable)?;
        output
      }
    },
    _=>return Ok(HttpResponse::Ok().json(json!({"jsonrpc":"2.0","id":payload["id"],"error":{"code":-32601,"message":"不支持的笔记工具方法"}}))),
  };
  Ok(
    HttpResponse::Ok()
      .insert_header(("Cache-Control", "no-store"))
      .json(json!({"jsonrpc":"2.0","id":payload["id"],"result":result})),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn tool_definitions_cannot_inject_names_or_oversized_schemas() {
    assert!(valid_tools(&[
      json!({"name":"create_page","inputSchema":{"type":"object"}})
    ]));
    assert!(!valid_tools(&[
      json!({"name":"../private","inputSchema":{}})
    ]));
    assert!(!valid_tools(&[
      json!({"name":"get_page","inputSchema":"invalid"})
    ]));
  }
}
