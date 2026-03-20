use actix_web::{
  web::{Data, Json},
  Result,
};
use app_error::ErrorCode;
use shared_entity::{
  dto::guest_dto::{
    RevokeSharedViewAccessRequest, ShareViewWithGuestRequest, SharedUser, SharedViewDetails,
    SharedViewDetailsRequest, SharedView, SharedViews, ListSharedViewResponse, SharedViewInfo,
  },
  response::{AppResponse, AppResponseError, JsonAppResponse},
};
use sqlx;

use actix_web::{
  web::{self},
  Scope,
};
use uuid::Uuid;

use crate::biz::authentication::jwt::UserUuid;
use crate::state::AppState;
use crate::biz::collab::me::{get_received_collab_list, get_send_collab_list};
use crate::biz::workspace;
use database_entity::dto::{AFAccessLevel, AFRole};
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
    AppResponseError::new(ErrorCode::UserUnAuthorized, e.to_string())
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
    
    // 优先从 af_collab_member_invite 中获取 owner_workspace_id
    // 这是解决网格类视图（Grid/Board/Calendar）workspace_id 错误的核心修复：
    // 对于网格类视图，af_collab 表只有 database_id 的条目，没有 view_id 的条目
    // 所以直接查询 af_collab WHERE oid=view_id 会失败，回退到当前用户自己的 workspace_id（错误！）
    let doc_workspace_id: Uuid = {
      // 首先尝试从邀请记录直接获取 owner_workspace_id（最准确，支持所有视图类型）
      let owner_ws: Option<Uuid> = sqlx::query_scalar(
        "SELECT owner_workspace_id FROM af_collab_member_invite WHERE oid = $1 AND received_uid = $2",
      )
      .bind(&invite.oid)
      .bind(uid)
      .fetch_one(&state.pg_pool)
      .await
      .ok();

      if let Some(ws) = owner_ws {
        ws
      } else {
        // 兼容旧数据：回退到 af_collab 查询（适用于普通文档视图）
        sqlx::query_scalar(
          "SELECT workspace_id FROM af_collab WHERE oid = $1",
        )
        .bind(&invite.oid)
        .fetch_one(&state.pg_pool)
        .await
        .unwrap_or(_workspace_id)
      }
    };
    
    // 从 af_collab_member 表获取用户的权限（使用运行时查询）
    let permission_id: i32 = sqlx::query_scalar(
      "SELECT permission_id FROM af_collab_member WHERE oid = $1 AND uid = $2",
    )
    .bind(&invite.oid)
    .bind(uid)
    .fetch_one(&state.pg_pool)
    .await
    .unwrap_or(1); // 默认只读权限
    
    // 将 permission_id 转换为 AFAccessLevel
    let _access_level = match permission_id {
      1 => AFAccessLevel::ReadOnly,
      2 => AFAccessLevel::ReadAndComment,
      3 => AFAccessLevel::ReadAndWrite,
      4 => AFAccessLevel::FullAccess,
      _ => AFAccessLevel::ReadOnly,
    };
    
    shared_views.push(SharedViewInfo {
      view_id,
      view_name: invite.name.clone(),
      shared_users: Vec::new(),
      created_at: invite.created_at,
      last_modified: invite.created_at,
      workspace_id: Some(doc_workspace_id),
    });
  }

  Ok(Json(AppResponse::Ok().with_data(ListSharedViewResponse {
    shared_views,
  })))
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
  user_uuid: UserUuid,
  state: Data<AppState>,
  _json: Json<SharedViewDetailsRequest>,
  path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<SharedViewDetails>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await.map_err(|e| {
    AppResponseError::new(ErrorCode::UserUnAuthorized, e.to_string())
  })?;
  let (_, view_id) = path.into_inner();
  let view_id_str = view_id.to_string();

  // Get all collab members for this view
  let members = workspace::ops::get_collab_members(&state.pg_pool, &view_id)
    .await
    .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  // Only allow collab members or workspace members of the document's workspace to view the list
  let is_collab_member = members.iter().any(|m| m.uid == uid);
  let is_workspace_member = if !is_collab_member {
    sqlx::query_scalar::<_, bool>(
      r#"
      SELECT EXISTS(
        SELECT 1 FROM af_collab c
        JOIN af_workspace_member wm ON c.workspace_id = wm.workspace_id
        WHERE c.oid = $1 AND wm.uid = $2
      )
      "#,
    )
    .bind(&view_id_str)
    .bind(uid)
    .fetch_one(&state.pg_pool)
    .await
    .unwrap_or(false)
  } else {
    false
  };

  if !is_collab_member && !is_workspace_member {
    return Err(AppResponseError::new(
      ErrorCode::NotEnoughPermissions,
      "Not authorized to view this document's members",
    )
    .into());
  }

  let shared_with: Vec<SharedUser> = members
    .into_iter()
    .map(|m| {
      let access_level = match m.permission_id {
        2 => AFAccessLevel::ReadAndComment,
        3 => AFAccessLevel::ReadAndWrite,
        4 => AFAccessLevel::FullAccess,
        _ => AFAccessLevel::ReadOnly,
      };
      SharedUser {
        view_id,
        email: m.email.unwrap_or_default(),
        name: m.name.unwrap_or_default(),
        access_level,
        role: AFRole::Member,
        avatar_url: m.avatar_url,
        pending_invitation: false,
      }
    })
    .collect();

  Ok(Json(AppResponse::Ok().with_data(SharedViewDetails {
    view_id,
    shared_with,
  })))
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
