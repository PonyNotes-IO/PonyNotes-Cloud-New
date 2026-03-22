// src/api/internal.rs
//
// 内部 API - 用于 GoTrue 后端调用，执行跨用户数据迁移
//
// 该 API 由 PonyNotes-gotrue 在账号合并时调用，
// 完成认证层合并后，通知 cloud 服务执行数据迁移。
//
// 端点：
//   POST /internal/migrate-user-data   - 执行用户数据迁移
//   POST /internal/delete-secondary-user - 软删除 secondary 用户（由 cloud 调用 Gotrue）
//
// 注意：这些 API 不需要用户认证（通过 service role JWT 保护），
// 应在网络层限制只允许 GoTrue 服务访问。

use crate::biz::user::user_merge::{
  get_user_workspaces, migrate_user_workspaces,
  GetUserWorkspacesRequest, GetUserWorkspacesResponse,
  MigrateUserDataRequest, MigrateUserDataResponse,
};
use crate::state::AppState;
use actix_web::{web, Scope};
use app_error::AppError;
use gotrue::params::AdminDeleteUserParams;
use shared_entity::response::{AppResponse, AppResponseError, JsonAppResponse};
use tracing::info;

/// 内部 API scope
pub fn internal_scope() -> Scope {
  web::scope("/internal")
    .service(
      web::resource("/migrate-user-data")
        .route(web::post().to(migrate_user_data_handler)),
    )
    .service(
      web::resource("/delete-secondary-user")
        .route(web::post().to(delete_secondary_user_handler)),
    )
    .service(
      web::resource("/get-user-workspaces")
        .route(web::post().to(get_user_workspaces_handler)),
    )
}

/// POST /internal/migrate-user-data
///
/// 执行用户数据迁移：将 secondary 用户的工作区和文档迁移到 primary 用户。
///
/// 请求体：
/// ```json
/// {
///   "primary_user_uuid": "uuid",
///   "secondary_user_uuid": "uuid"
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "primary_uid": 123,
///   "secondary_uid": 456,
///   "migrated_workspace_count": 2,
///   "workspaces": [
///     { "workspace_id": "...", "workspace_name": "...", "collab_count": 10, "status": "success", "error": null },
///     ...
///   ]
/// }
/// ```
async fn migrate_user_data_handler(
  state: web::Data<AppState>,
  payload: web::Json<MigrateUserDataRequest>,
) -> Result<JsonAppResponse<MigrateUserDataResponse>, AppResponseError> {
  info!(
    "[InternalAPI] Received migrate-user-data request: primary={}, secondary={}",
    payload.primary_user_uuid, payload.secondary_user_uuid
  );

  let resp = migrate_user_workspaces(
    &state.pg_pool,
    &state.collab_storage,
    &state.workspace_access_control,
    payload.into_inner(),
  )
  .await
  .map_err(AppResponseError::from)?;

  info!(
    "[InternalAPI] Migration completed: {} workspaces migrated",
    resp.migrated_workspace_count
  );
  Ok(AppResponse::Ok().with_data(resp).into())
}

/// POST /internal/get-user-workspaces
///
/// 获取指定用户的所有工作区信息（用于迁移前预览）。
async fn get_user_workspaces_handler(
  state: web::Data<AppState>,
  payload: web::Json<GetUserWorkspacesRequest>,
) -> Result<JsonAppResponse<GetUserWorkspacesResponse>, AppResponseError> {
  info!(
    "[InternalAPI] Received get-user-workspaces request: user={}",
    payload.user_uuid
  );

  let resp = get_user_workspaces(&state.pg_pool, &payload.user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  info!(
    "[InternalAPI] Got {} workspaces for user {}",
    resp.workspaces.len(),
    resp.uid
  );
  Ok(AppResponse::Ok().with_data(resp).into())
}

/// POST /internal/delete-secondary-user
///
/// 在 Gotrue 中软删除 secondary 用户。
/// 此接口由 cloud 服务在数据迁移完成后调用，通知 Gotrue 执行最终清理。
///
/// 请求体：
/// ```json
/// {
///   "user_uuid": "uuid"
/// }
/// ```
#[derive(serde::Deserialize)]
struct DeleteSecondaryUserRequest {
  user_uuid: uuid::Uuid,
}

async fn delete_secondary_user_handler(
  state: web::Data<AppState>,
  payload: web::Json<DeleteSecondaryUserRequest>,
) -> Result<JsonAppResponse<()>, AppResponseError> {
  info!(
    "[InternalAPI] Received delete-secondary-user request: user={}",
    payload.user_uuid
  );

  let admin_token = state
    .gotrue_admin
    .token()
    .await
    .map_err(|e| {
      tracing::error!("[InternalAPI] Failed to get admin token: {:?}", e);
      AppResponseError::from(AppError::Internal(anyhow::anyhow!(
        "Failed to get admin token: {}",
        e
      )))
    })?;

  state
    .gotrue_client
    .admin_delete_user(
      &admin_token,
      &payload.user_uuid.to_string(),
      &AdminDeleteUserParams {
        should_soft_delete: true,
      },
    )
    .await
    .map_err(|e| {
      tracing::error!("[InternalAPI] Failed to delete user in Gotrue: {:?}", e);
      AppResponseError::from(AppError::Internal(anyhow::anyhow!(
        "Failed to delete user: {}",
        e
      )))
    })?;

  info!(
    "[InternalAPI] Successfully soft-deleted user {} in Gotrue",
    payload.user_uuid
  );

  Ok(AppResponse::Ok().into())
}
