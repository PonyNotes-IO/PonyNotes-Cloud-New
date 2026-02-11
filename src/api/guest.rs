use actix_web::{
  web::{Data, Json},
  Result,
};
use app_error::ErrorCode;
use shared_entity::{
  dto::guest_dto::{
    RevokeSharedViewAccessRequest, ShareViewWithGuestRequest, SharedViewDetails,
    SharedViewDetailsRequest, SharedView, SharedViews, ListSharedViewResponse, SharedViewInfo,
  },
  response::{AppResponseError, JsonAppResponse},
};

use actix_web::{
  web::{self},
  Scope,
};
use uuid::Uuid;

use crate::biz::authentication::jwt::UserUuid;
use crate::state::AppState;
use crate::biz::collab::me::{get_received_collab_list, get_send_collab_list};
use database_entity::dto::AFAccessLevel;
use database::pg_row::AFCollabMemberInvite;

pub fn sharing_scope() -> Scope {
  web::scope("/api/sharing/workspace")
    .service(
      web::resource("{workspace_id}/view")
        .route(web::get().to(list_shared_views_handler))
        .route(web::put().to(put_shared_view_handler)),
    )
    .service(
      web::resource("{workspace_id}/view/{view_id}/access-details")
        .route(web::post().to(shared_view_access_details_handler)),
    )
    .service(
      web::resource("{workspace_id}/view/{view_id}/revoke-access")
        .route(web::post().to(revoke_shared_view_access_handler)),
    )
}

async fn list_shared_views_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<Uuid>,
) -> Result<JsonAppResponse<ListSharedViewResponse>> {
  // 注意：path 中的 workspace_id 可能不是共享文档的实际所属 workspace
  // 我们需要查询 af_collab_member_invite 表获取所有共享给当前用户的文档
  let _workspace_id = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await.map_err(|e| {
    AppResponseError::new(ErrorCode::UserNotFound, e.to_string())
  })?;

  // 从 af_collab_member_invite 获取被分享给我的所有文档（作为接收者）
  let invites = get_received_collab_list(&state.pg_pool, uid).await.map_err(|e| {
    AppResponseError::new(ErrorCode::Internal, e.to_string())
  })?;

  // 转换为 SharedViewInfo 格式
  let mut shared_views: Vec<SharedViewInfo> = Vec::new();
  
  for invite in invites {
    let view_id = match invite.oid.parse::<Uuid>() {
      Ok(v) => v,
      Err(_) => continue,
    };
    
    // 从 af_collab 表获取文档实际所属的 workspace_id
    let doc_workspace_id = sqlx::query_scalar!(
      "SELECT workspace_id FROM af_collab WHERE oid = $1",
      invite.oid
    )
    .fetch_one(&state.pg_pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(_workspace_id);
    
    // 从 af_collab_member 表获取用户的权限
    let permission_id = sqlx::query_scalar!(
      "SELECT permission_id FROM af_collab_member WHERE oid = $1 AND uid = $2",
      invite.oid,
      uid
    )
    .fetch_one(&state.pg_pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(1); // 默认只读权限
    
    // 将 permission_id 转换为 AFAccessLevel
    let access_level = match permission_id {
      1 => AFAccessLevel::ReadOnly,
      2 => AFAccessLevel::ReadAndComment,
      3 => AFAccessLevel::ReadAndWrite,
      4 => AFAccessLevel::FullAccess,
      _ => AFAccessLevel::ReadOnly,
    };
    
    shared_views.push(SharedViewInfo {
      view_id,
      view_name: invite.name.clone().unwrap_or_else(|| "共享文档".to_string()),
      shared_users: Vec::new(),
      created_at: invite.created_at,
      last_modified: invite.created_at,
      workspace_id: Some(doc_workspace_id),
    });
  }

  Ok(JsonAppResponse::Ok().with_data(ListSharedViewResponse {
    shared_views,
  }))
}

async fn put_shared_view_handler(
  _user_uuid: UserUuid,
  _state: Data<AppState>,
  _payload: web::Json<ShareViewWithGuestRequest>,
  _path: web::Path<Uuid>,
) -> Result<JsonAppResponse<()>> {
  Err(
    AppResponseError::new(
      ErrorCode::FeatureNotAvailable,
      "this version of appflowy cloud server does not support guest editors",
    )
    .into(),
  )
}

async fn shared_view_access_details_handler(
  _user_uuid: UserUuid,
  _state: Data<AppState>,
  _json: Json<SharedViewDetailsRequest>,
  _path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<SharedViewDetails>> {
  Err(
    AppResponseError::new(
      ErrorCode::FeatureNotAvailable,
      "this version of appflowy cloud server does not support guest editors",
    )
    .into(),
  )
}

async fn revoke_shared_view_access_handler(
  _user_uuid: UserUuid,
  _state: Data<AppState>,
  _payload: web::Json<RevokeSharedViewAccessRequest>,
  _path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<()>> {
  Err(
    AppResponseError::new(
      ErrorCode::FeatureNotAvailable,
      "this version of appflowy cloud server does not support guest editors",
    )
    .into(),
  )
}
