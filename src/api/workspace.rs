use crate::api::util::{client_version_from_headers, realtime_user_for_web_request, PayloadReader};
use crate::api::util::{compress_type_from_header_value, device_id_from_headers};
use crate::api::ws::RealtimeServerAddr;
use crate::biz;
use crate::biz::authentication::jwt::{Authorization, OptionalUserUuid, UserUuid};
use crate::biz::collab::database::check_if_row_document_collab_exists;
use crate::biz::collab::ops::{
  get_user_favorite_folder_views, get_user_recent_folder_views, get_user_trash_folder_views,
};
use crate::biz::collab::utils::{collab_from_doc_state, DUMMY_UID};
use crate::biz::workspace;
use crate::biz::workspace::duplicate::duplicate_view_tree_and_collab;
use crate::biz::workspace::invite::{
  delete_workspace_invite_code, generate_workspace_invite_token, get_invite_code_for_workspace,
  join_workspace_invite_by_code,
};
use crate::biz::workspace::join_request::{
  cancel_join_request, create_join_request, handle_join_request, list_join_requests,
};
use crate::biz::workspace::ops::{
  create_comment_on_published_view, create_reaction_on_comment, get_comments_on_published_view,
  get_reactions_on_published_view, get_workspace_owner, remove_comment_on_published_view,
  remove_reaction_on_comment, update_workspace_member_profile,
};
use crate::biz::workspace::page_view::{
  add_recent_pages, append_block_at_the_end_of_page, create_database_view, create_folder_view,
  create_orphaned_view, create_page, create_space, delete_all_pages_from_trash, delete_trash,
  favorite_page, get_page_view_collab, move_page, move_page_to_trash, publish_page,
  reorder_favorite_page, restore_all_pages_from_trash, restore_page_from_trash, unpublish_page,
  update_page, update_page_collab_data, update_page_extra, update_page_icon, update_page_name,
  update_space,
};
use crate::biz::workspace::publish::get_workspace_default_publish_view_info_meta;
use crate::biz::workspace::publish::list_collab_publish_info;
use database::publish::{
  insert_received_published_collab, select_received_published_collabs,
  select_published_collab_by_uid, select_received_published_collab_with_details,
};
use crate::biz::workspace::quick_note::{
  create_quick_note, delete_quick_note, list_quick_notes, update_quick_note,
};
use crate::domain::compression::{
  blocking_decompress, decompress, CompressionType, X_COMPRESSION_TYPE,
};
use crate::state::AppState;
use access_control::act::Action;
use actix_web::web::{Bytes, Path, Payload};
use actix_web::web::{Data, Json, PayloadConfig};
use actix_web::{web, HttpResponse, ResponseError, Scope};
use actix_web::{HttpRequest, Result};
use anyhow::{anyhow, Context};
use app_error::{AppError, ErrorCode};
use appflowy_collaborate::actix_ws::entities::{
  ClientGenerateEmbeddingMessage, ClientHttpStreamMessage, ClientHttpUpdateMessage,
};
use appflowy_collaborate::ws2::WorkspaceCollabInstanceCache;
use database::publish::select_all_published_collab_info_global;

use bytes::BytesMut;
use chrono::{DateTime, Duration, Utc};
use collab::core::collab::{default_client_id, CollabOptions, DataSource};
use collab::core::origin::CollabOrigin;
use collab::entity::EncodedCollab;
use collab::preclude::Collab;
use collab_database::entity::FieldType;
use collab_document::document::Document;
use collab_entity::CollabType;
use collab_folder::timestamp;
use collab_rt_entity::collab_proto::{CollabDocStateParams, PayloadCompressionType};
use collab_rt_entity::realtime_proto::HttpRealtimeMessage;
use collab_rt_entity::user::RealtimeUser;
use collab_rt_entity::RealtimeMessage;
use collab_rt_protocol::collab_from_encode_collab;
use database::pg_row::AFReceivedPublishedCollab;
use database_entity::dto::PublishCollabItem;
use database_entity::dto::PublishInfo;
use database_entity::dto::*;
use indexer::scheduler::{UnindexedCollabTask, UnindexedData};
use itertools::Itertools;
use prost::Message as ProstMessage;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::biz::collab::me::{get_received_collab_list, get_send_collab_list};
use crate::biz::subscription::ops::{check_user_storage_limit, get_user_resource_limit_status};
use crate::biz::workspace::collab_member::{
  add_collab_member, edit_collab_member_permission, remove_collab_member, remove_collab_member_invite,
};
use crate::biz::workspace::ops::get_collab_owner;
use database::pg_row::AFCollabMemberInvite;
use database::subscription::get_user_total_usage_bytes;
use database::workspace::{select_collab_owner, update_collab_member_permission};
use semver::Version;
use sha2::{Digest, Sha256};
use shared_entity::dto::publish_dto::DuplicatePublishedPageResponse;
use shared_entity::dto::workspace_dto::{
  AllPublishedCollabItem, ListAllPublishedCollabResponse, ReceivePublishedCollabRequest,
  ReceivePublishedCollabResponse,
};
use shared_entity::dto::workspace_dto::{SingleOrVecInvitation, *};
use shared_entity::response::AppResponseError;
use shared_entity::response::{AppResponse, JsonAppResponse};
use sqlx::types::uuid;
use std::io::Cursor;
use std::time::Instant;
use tokio_stream::StreamExt;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, event, instrument, trace};
use uuid::Uuid;
use validator::Validate;
use workspace_template::document::parser::SerdeBlock;

pub const WORKSPACE_ID_PATH: &str = "workspace_id";
pub const COLLAB_OBJECT_ID_PATH: &str = "object_id";

pub const WORKSPACE_PATTERN: &str = "/api/workspace";
pub const WORKSPACE_MEMBER_PATTERN: &str = "/api/workspace/{workspace_id}/member";
pub const WORKSPACE_INVITE_PATTERN: &str = "/api/workspace/{workspace_id}/invite";
pub const COLLAB_PATTERN: &str = "/api/workspace/{workspace_id}/collab/{object_id}";
pub const V1_COLLAB_PATTERN: &str = "/api/workspace/v1/{workspace_id}/collab/{object_id}";
pub const WORKSPACE_PUBLISH_PATTERN: &str = "/api/workspace/{workspace_id}/publish";
pub const WORKSPACE_PUBLISH_NAMESPACE_PATTERN: &str =
  "/api/workspace/{workspace_id}/publish-namespace";

pub fn workspace_scope() -> Scope {
  web::scope("/api/workspace")
        .service(
            web::resource("")
                .route(web::get().to(list_workspace_handler))
                .route(web::post().to(create_workspace_handler))
                .route(web::patch().to(patch_workspace_handler)),
        )
        .service(
            web::resource("/{workspace_id}/invite").route(web::post().to(post_workspace_invite_handler)), // invite members to workspace
        )
        .service(
            web::resource("/invite").route(web::get().to(get_workspace_invite_handler)), // show invites for user
        )
        .service(
            web::resource("/invite/{invite_id}").route(web::get().to(get_workspace_invite_by_id_handler)),
        )
        .service(
            web::resource("/accept-invite/{invite_id}")
                .route(web::post().to(post_accept_workspace_invite_handler)), // accept invitation to workspace
        )
        .service(
            web::resource("/join-by-invite-code")
                .route(web::post().to(post_join_workspace_invite_by_code_handler)),
        )

        .service(web::resource("/{workspace_id}").route(web::delete().to(delete_workspace_handler)))
        .service(
            web::resource("/{workspace_id}/settings")
                .route(web::get().to(get_workspace_settings_handler))
                .route(web::post().to(post_workspace_settings_handler)),
        )
        .service(web::resource("/{workspace_id}/open").route(web::put().to(open_workspace_handler)))
        .service(web::resource("/{workspace_id}/leave").route(web::post().to(leave_workspace_handler)))
        .service(
            web::resource("/{workspace_id}/member")
                .route(web::get().to(get_workspace_members_handler))
                .route(web::put().to(update_workspace_member_handler))
                .route(web::delete().to(remove_workspace_member_handler)),
        )
        .service(
            web::resource("/{workspace_id}/mentionable-person")
                .route(web::get().to(list_workspace_mentionable_person_handler)),
        )
        .service(
            web::resource("/{workspace_id}/mentionable-person/{contact_id}")
                .route(web::get().to(get_workspace_mentionable_person_handler)),
        )
        .service(
            web::resource("/{workspace_id}/update-member-profile")
                .route(web::put().to(put_workspace_member_profile_handler)),
        )
        // Deprecated since v0.9.24
        .service(
            web::resource("/{workspace_id}/member/user/{user_id}")
                .route(web::get().to(get_workspace_member_handler)),
        )
        .service(
            web::resource("v1/{workspace_id}/member/user/{user_id}")
                .route(web::get().to(get_workspace_member_v1_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/{object_id}")
                .app_data(
                    PayloadConfig::new(5 * 1024 * 1024), // 5 MB
                )
                .route(web::post().to(create_collab_handler))
                .route(web::get().to(get_collab_handler))
                .route(web::put().to(update_collab_handler))
                .route(web::delete().to(delete_collab_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/{object_id}/members")
                .route(web::get().to(get_collab_members_handler)),
        )
        .service(
            // 创建邀请链接（生成链接时调用）
            web::resource("/{workspace_id}/collab/{object_id}/invite-link")
                .route(web::post().to(create_share_link_invite_handler)),
        )
        .service(
            // 添加协作成员（给自己或别人）
            web::resource("/{workspace_id}/collab/{object_id}/members/{member_user_id}")
                .route(web::post().to(add_collab_member_handler)),
        )
        .service(
            // 修改已有协作成员的权限
            web::resource("/{workspace_id}/collab/{object_id}/members/{member_user_id}")
                .route(web::patch().to(update_collab_member_permission_handler)),
        )
        .service(
            // 删除协作成员
            web::resource("/{workspace_id}/collab/{object_id}/members/{member_user_id}")
                .route(web::delete().to(remove_collab_member_handler)),
        )
        .service(
            web::resource("/v1/{workspace_id}/collab/{object_id}")
                .route(web::get().to(v1_get_collab_handler)),
        )
        .service(
            web::resource("/v1/{workspace_id}/collab/{object_id}/json")
                .route(web::get().to(get_collab_json_handler)),
        )
        .service(
            web::resource("/v1/{workspace_id}/collab/{object_id}/full-sync")
                .route(web::post().to(collab_full_sync_handler)),
        )
        .service(
            web::resource("/v1/{workspace_id}/collab/{object_id}/web-update")
                .route(web::post().to(post_web_update_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/{object_id}/row-document-collab-exists")
                .route(web::get().to(get_row_document_collab_exists_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/{object_id}/embed-info")
                .route(web::get().to(get_collab_embed_info_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/{object_id}/generate-embedding")
                .route(web::get().to(force_generate_collab_embedding_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab/embed-info/list")
                .route(web::post().to(batch_get_collab_embed_info_handler)),
        )
        .service(web::resource("/{workspace_id}/space").route(web::post().to(post_space_handler)))
        .service(
            web::resource("/{workspace_id}/space/{view_id}").route(web::patch().to(update_space_handler)),
        )
        .service(
            web::resource("/{workspace_id}/spaces/{space_id}/join-requests")
                .route(web::post().to(post_join_request_handler))
                .route(web::get().to(get_join_requests_handler))
                .route(web::delete().to(cancel_join_request_handler)),
        )
        .service(
            web::resource("/{workspace_id}/spaces/{space_id}/join-requests/{request_id}")
                .route(web::post().to(handle_join_request_handler)),
        )
        .service(
            web::resource("/{workspace_id}/folder-view").route(web::post().to(post_folder_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view").route(web::post().to(post_page_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}")
                .route(web::get().to(get_page_view_handler))
                .route(web::patch().to(update_page_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/mentionable-person-with-access")
                .route(web::get().to(list_page_mentionable_person_with_access_handler))
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/page-mention")
                .route(web::put().to(put_page_mention_handler))
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/update-name")
                .route(web::post().to(update_page_name_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/update-icon")
                .route(web::post().to(update_page_icon_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/update-extra")
                .route(web::post().to(update_page_extra_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/remove-icon")
                .route(web::post().to(remove_page_icon_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/favorite")
                .route(web::post().to(favorite_page_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/append-block")
                .route(web::post().to(append_block_to_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/move")
                .route(web::post().to(move_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/reorder-favorite")
                .route(web::post().to(reorder_favorite_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/duplicate")
                .route(web::post().to(duplicate_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/database-view")
                .route(web::post().to(post_page_database_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/move-to-trash")
                .route(web::post().to(move_page_to_trash_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/restore-from-trash")
                .route(web::post().to(restore_page_from_trash_handler)),
        )
        .service(
            web::resource("/{workspace_id}/add-recent-pages")
                .route(web::post().to(add_recent_pages_handler)),
        )
        .service(
            web::resource("/{workspace_id}/restore-all-pages-from-trash")
                .route(web::post().to(restore_all_pages_from_trash_handler)),
        )
        .service(
            web::resource("/{workspace_id}/delete-all-pages-from-trash")
                .route(web::post().to(delete_all_pages_from_trash_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/publish")
                .route(web::post().to(publish_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/page-view/{view_id}/unpublish")
                .route(web::post().to(unpublish_page_handler)),
        )
        .service(
            web::resource("/{workspace_id}/orphaned-view")
                .route(web::post().to(post_orphaned_view_handler)),
        )
        .service(
            web::resource("/{workspace_id}/batch/collab")
                .route(web::post().to(batch_create_collab_handler)),
        )
        .service(
            web::resource("/{workspace_id}/usage").route(web::get().to(get_workspace_usage_handler)),
        )
        .service(
            web::resource("/{workspace_id}/usage-and-limit")
                .route(web::get().to(get_workspace_usage_and_limit_handler)),
        )
        // 接收发布的文档 API（静态路径必须在动态路径 /published/{publish_namespace} 之前注册，避免 405 冲突）
        .service(
            web::resource("/published/receive")
                .route(web::post().to(receive_published_collab_handler)),
        )
        // 查询接收的发布文档只读状态 API
        .service(
            web::resource("/published/received/{view_id}/readonly")
                .route(web::get().to(get_received_published_collab_readonly_handler)),
        )
        .service(
            web::resource("/published/{publish_namespace}")
                .route(web::get().to(get_default_published_collab_info_meta_handler)),
        )
        .service(
            web::resource("/v1/published/{publish_namespace}/{publish_name}")
                .route(web::get().to(get_v1_published_collab_handler)),
        )
        .service(
            web::resource("/published/{publish_namespace}/{publish_name}/blob")
                .route(web::get().to(get_published_collab_blob_handler)),
        )
        .service(
            web::resource("{workspace_id}/published-duplicate")
                .route(web::post().to(post_published_duplicate_handler)),
        )
        .service(
            web::resource("/{workspace_id}/published-info")
                .route(web::get().to(list_published_collab_info_handler)),
        )
        // 添加全局发布列表 API - 不限制 workspace_id，用于侧边栏显示所有发布的笔记
        .service(
            web::resource("/published-info/all")
                .route(web::get().to(list_all_published_collab_info_handler)),
        )
        .service(
            // deprecated since 0.7.4
            web::resource("/published-info/{view_id}")
                .route(web::get().to(get_published_collab_info_handler)),
        )
        .service(
            web::resource("/v1/published-info/{view_id}")
                .route(web::get().to(get_v1_published_collab_info_handler)),
        )
        .service(
            web::resource("/published-info/{view_id}/comment")
                .route(web::get().to(get_published_collab_comment_handler))
                .route(web::post().to(post_published_collab_comment_handler))
                .route(web::delete().to(delete_published_collab_comment_handler)),
        )
        .service(
            web::resource("/published-info/{view_id}/reaction")
                .route(web::get().to(get_published_collab_reaction_handler))
                .route(web::post().to(post_published_collab_reaction_handler))
                .route(web::delete().to(delete_published_collab_reaction_handler)),
        )
        .service(
            web::resource("/{workspace_id}/publish-namespace")
                .route(web::put().to(put_publish_namespace_handler))
                .route(web::get().to(get_publish_namespace_handler)),
        )
        .service(
            web::resource("/{workspace_id}/publish-default")
                .route(web::put().to(put_workspace_default_published_view_handler))
                .route(web::delete().to(delete_workspace_default_published_view_handler))
                .route(web::get().to(get_workspace_published_default_info_handler)),
        )
        .service(
            web::resource("/{workspace_id}/publish")
                .route(web::post().to(post_publish_collabs_handler))
                .route(web::delete().to(delete_published_collabs_handler))
                .route(web::patch().to(patch_published_collabs_handler)),
        )
        .service(
            web::resource("/{workspace_id}/folder").route(web::get().to(get_workspace_folder_handler)),
        )
        .service(web::resource("/{workspace_id}/recent").route(web::get().to(get_recent_views_handler)))
        .service(
            web::resource("/{workspace_id}/favorite").route(web::get().to(get_favorite_views_handler)),
        )
        .service(web::resource("/{workspace_id}/trash").route(web::get().to(get_trash_views_handler)))
        .service(
            web::resource("/{workspace_id}/trash/{view_id}")
                .route(web::delete().to(delete_page_from_trash_handler)),
        )
        .service(
            web::resource("/published-outline/{publish_namespace}")
                .route(web::get().to(get_workspace_publish_outline_handler)),
        )
        .service(
            web::resource("/{workspace_id}/collab_list")
                .route(web::get().to(batch_get_collab_handler))
                // Web browser can't carry payload when using GET method, so for browser compatibility, we use POST method
                .route(web::post().to(batch_get_collab_handler)),
        )
        .service(web::resource("/{workspace_id}/database").route(web::get().to(list_database_handler)))
        .service(
            web::resource("/{workspace_id}/database/{database_id}/row")
                .route(web::get().to(list_database_row_id_handler))
                .route(web::post().to(post_database_row_handler))
                .route(web::put().to(put_database_row_handler)),
        )
        .service(
            web::resource("/{workspace_id}/database/{database_id}/fields")
                .route(web::get().to(get_database_fields_handler))
                .route(web::post().to(post_database_fields_handler)),
        )
        .service(
            web::resource("/{workspace_id}/database/{database_id}/row/updated")
                .route(web::get().to(list_database_row_id_updated_handler)),
        )
        .service(
            web::resource("/{workspace_id}/database/{database_id}/row/detail")
                .route(web::get().to(list_database_row_details_handler)),
        )
        .service(
            web::resource("/{workspace_id}/quick-note")
                .route(web::get().to(list_quick_notes_handler))
                .route(web::post().to(post_quick_note_handler)),
        )
        .service(
            web::resource("/{workspace_id}/quick-note/{quick_note_id}")
                .route(web::put().to(update_quick_note_handler))
                .route(web::delete().to(delete_quick_note_handler)),
        )
        .service(
            web::resource("/{workspace_id}/invite-code")
                .route(web::get().to(get_workspace_invite_code_handler))
                .route(web::delete().to(delete_workspace_invite_code_handler))
                .route(web::post().to(post_workspace_invite_code_handler)),
        )
}

pub fn collab_scope() -> Scope {
  web::scope("/api/collab/me")
    .service(
      // 别人分享给我的协作视图列表
      web::resource("/received").route(web::get().to(list_received_collab_handler)),
    )
    .service(
      // 我分享给别人的协作视图列表
      web::resource("/sent").route(web::get().to(list_sent_collab_handler)),
    )
    .service(
      web::scope("/api/realtime").service(
        web::resource("post/stream")
          .app_data(
            PayloadConfig::new(10 * 1024 * 1024), // 10 MB
          )
          .route(web::post().to(post_realtime_message_stream_handler)),
      ),
    )
}

/// 添加协作成员的请求参数
#[derive(Debug, Deserialize)]
pub struct AddCollabMemberParams {
    #[serde(default = "default_permission_id")]
    pub permission_id: i32,
}

fn default_permission_id() -> i32 {
    1 // 默认只读权限
}

// Adds a workspace for user, if success, return the workspace id
#[instrument(skip_all, err)]
async fn create_workspace_handler(
  uuid: UserUuid,
  state: Data<AppState>,
  create_workspace_param: Json<CreateWorkspaceParam>,
) -> Result<Json<AppResponse<AFWorkspace>>> {
  let uid = state.user_cache.get_user_uid(&uuid).await?;
  let create_workspace_param = create_workspace_param.into_inner();
  let workspace_name = create_workspace_param
    .workspace_name
    .unwrap_or_else(|| format!("workspace_{}", chrono::Utc::now().timestamp()));

  let workspace_icon = create_workspace_param.workspace_icon.unwrap_or_default();
  let new_workspace = workspace::ops::create_workspace_for_user(
    &state.pg_pool,
    state.workspace_access_control.clone(),
    &state.collab_storage,
    &state.metrics.collab_metrics,
    &uuid,
    uid,
    &workspace_name,
    &workspace_icon,
  )
  .await?;

  Ok(AppResponse::Ok().with_data(new_workspace).into())
}

// Edit existing workspace
#[instrument(skip_all, err)]
async fn patch_workspace_handler(
  uuid: UserUuid,
  state: Data<AppState>,
  params: Json<PatchWorkspaceParam>,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &params.workspace_id, Action::Write)
    .await?;
  let params = params.into_inner();
  workspace::ops::patch_workspace(
    &state.pg_pool,
    &params.workspace_id,
    params.workspace_name.as_deref(),
    params.workspace_icon.as_deref(),
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

async fn delete_workspace_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Delete)
    .await?;
  workspace::ops::delete_workspace_for_user(
    state.pg_pool.clone(),
    state.redis_connection_manager.clone(),
    workspace_id,
    state.bucket_storage.clone(),
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

/// Get all user owned and shared workspaces
#[instrument(skip_all, err)]
async fn list_workspace_handler(
  uuid: UserUuid,
  state: Data<AppState>,
  query: web::Query<QueryWorkspaceParam>,
  req: HttpRequest,
) -> Result<JsonAppResponse<Vec<AFWorkspace>>> {
  let app_version = client_version_from_headers(req.headers())
    .ok()
    .and_then(|s| Version::parse(s).ok());
  let exclude_guest = app_version
    .map(|s| s < Version::new(0, 9, 4))
    .unwrap_or(true);
  let QueryWorkspaceParam {
    include_member_count,
    include_role,
  } = query.into_inner();

  let workspaces = workspace::ops::get_all_user_workspaces(
    &state.pg_pool,
    &uuid,
    include_member_count.unwrap_or(false),
    include_role.unwrap_or(false),
    exclude_guest,
  )
  .await?;
  Ok(AppResponse::Ok().with_data(workspaces).into())
}

#[instrument(skip(payload, state), err)]
async fn post_workspace_invite_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  payload: web::Json<SingleOrVecInvitation>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;

  // 支持单个对象或数组格式，自动转换为Vec
  let invitations = payload.into_inner().into_vec();

  workspace::ops::invite_workspace_members(
    &state.mailer,
    &state.pg_pool,
    &state.workspace_access_control,
    &user_uuid,
    &workspace_id,
    invitations,
    &state.config.appflowy_web_url,
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

async fn get_workspace_invite_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  query: web::Query<WorkspaceInviteQuery>,
) -> Result<JsonAppResponse<Vec<AFWorkspaceInvitation>>> {
  let query = query.into_inner();
  let res =
    workspace::ops::list_workspace_invitations_for_user(&state.pg_pool, &user_uuid, query.status)
      .await?;
  Ok(AppResponse::Ok().with_data(res).into())
}

async fn get_workspace_invite_by_id_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  invite_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<AFWorkspaceInvitation>> {
  let invite_id = invite_id.into_inner();
  let res =
    workspace::ops::get_workspace_invitations_for_user(&state.pg_pool, &user_uuid, &invite_id)
      .await?;
  Ok(AppResponse::Ok().with_data(res).into())
}

async fn post_accept_workspace_invite_handler(
  auth: Authorization,
  invite_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let user_uuid = auth.uuid()?;
  let user_uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let invite_id = invite_id.into_inner();
  workspace::ops::accept_workspace_invite(
    &state.pg_pool,
    state.workspace_access_control.clone(),
    user_uid,
    &user_uuid,
    &invite_id,
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

async fn post_join_workspace_invite_by_code_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<JoinWorkspaceByInviteCodeParams>,
) -> Result<JsonAppResponse<InvitedWorkspace>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let invited_workspace_id =
    join_workspace_invite_by_code(&state.pg_pool, &payload.code, uid).await?;
  state
    .workspace_access_control
    .insert_role(&uid, &invited_workspace_id, AFRole::Member)
    .await?;
  Ok(
    AppResponse::Ok()
      .with_data(InvitedWorkspace {
        workspace_id: invited_workspace_id,
      })
      .into(),
  )
}

#[instrument(level = "trace", skip_all, err, fields(user_uuid))]
async fn get_workspace_settings_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<AFWorkspaceSettings>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;
  let settings = workspace::ops::get_workspace_settings(&state.pg_pool, &workspace_id).await?;
  Ok(AppResponse::Ok().with_data(settings).into())
}

#[instrument(level = "info", skip_all, err, fields(user_uuid))]
async fn post_workspace_settings_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
  data: Json<AFWorkspaceSettingsChange>,
) -> Result<JsonAppResponse<AFWorkspaceSettings>> {
  let data = data.into_inner();
  trace!("workspace settings: {:?}", data);
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;
  let settings =
    workspace::ops::update_workspace_settings(&state.pg_pool, &workspace_id, data).await?;
  Ok(AppResponse::Ok().with_data(settings).into())
}

/// A workspace member/owner can view all members of the workspace, except for guests.
/// A guest can only view their own information.
#[instrument(skip_all, err)]
async fn get_workspace_members_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<Vec<AFWorkspaceMember>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  let requester_member_info =
    workspace::ops::get_workspace_member(uid, &state.pg_pool, &workspace_id).await?;
  let members: Vec<AFWorkspaceMember> = if requester_member_info.role == AFRole::Guest {
    let owner = get_workspace_owner(&state.pg_pool, &workspace_id).await?;
    vec![requester_member_info.into(), owner.into()]
  } else {
    workspace::ops::get_workspace_members_exclude_guest(&state.pg_pool, &workspace_id)
      .await?
      .into_iter()
      .map(|member| member.into())
      .collect()
  };

  Ok(AppResponse::Ok().with_data(members).into())
}

#[instrument(skip_all, err)]
async fn get_collab_members_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<(Uuid, Uuid)>, // (workspace_id, object_id)
) -> Result<JsonAppResponse<Vec<database::pg_row::AFExplicitCollabMemberRow>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_id, object_id) = path.into_inner();
  // Require requester to be at least a workspace member
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;

  let members = workspace::ops::get_collab_members(&state.pg_pool, &object_id).await?;
  Ok(AppResponse::Ok().with_data(members).into())
}

#[instrument(skip_all, err)]
async fn remove_workspace_member_handler(
  user_uuid: UserUuid,
  payload: Json<WorkspaceMembers>,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<()>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;

  let member_emails = payload
    .into_inner()
    .0
    .into_iter()
    .map(|member| member.0)
    .collect::<Vec<String>>();
  workspace::ops::remove_workspace_members(
    &state.pg_pool,
    &workspace_id,
    &member_emails,
    state.workspace_access_control.clone(),
  )
  .await?;

  Ok(AppResponse::Ok().into())
}

#[instrument(skip_all, err)]
async fn list_workspace_mentionable_person_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<Uuid>,
) -> Result<JsonAppResponse<MentionablePersons>> {
  let workspace_id = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  // Guest can access mentionable users, but only themselves and the owner
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  let persons = workspace::ops::list_workspace_mentionable_persons_with_last_mentioned_time(
    &state.pg_pool,
    &workspace_id,
    uid,
    &user_uuid,
  )
  .await?;
  Ok(
    AppResponse::Ok()
      .with_data(MentionablePersons { persons })
      .into(),
  )
}

#[instrument(skip_all, err)]
async fn list_page_mentionable_person_with_access_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<MentionablePersonsWithAccess>> {
  let (workspace_id, view_id) = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  let persons = workspace::page_view::list_page_mentionable_persons_with_access(
    &state.ws_server,
    &state.pg_pool,
    &workspace_id,
    &view_id,
  )
  .await?;
  Ok(
    AppResponse::Ok()
      .with_data(MentionablePersonsWithAccess { persons })
      .into(),
  )
}

#[instrument(skip_all, err)]
async fn put_page_mention_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  payload: Json<PageMentionUpdate>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, view_id) = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  workspace::page_view::update_page_mention(
    &state.pg_pool,
    &workspace_id,
    &view_id,
    uid,
    &payload.into_inner(),
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

#[instrument(skip_all, err)]
async fn get_workspace_mentionable_person_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<MentionablePerson>> {
  let (workspace_id, person_id) = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  let person =
    workspace::ops::get_workspace_mentionable_person(&state.pg_pool, &workspace_id, &person_id)
      .await?;
  Ok(AppResponse::Ok().with_data(person).into())
}

async fn put_workspace_member_profile_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<WorkspaceMemberProfile>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let workspace_id = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Guest)
    .await?;
  let updated_profile = payload.into_inner();
  update_workspace_member_profile(&state.pg_pool, &workspace_id, uid, &updated_profile).await?;
  Ok(AppResponse::Ok().into())
}

#[instrument(skip_all, err)]
async fn get_workspace_member_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<(Uuid, i64)>,
) -> Result<JsonAppResponse<AFWorkspaceMember>> {
  let (workspace_id, member_uid) = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  // Guest users can not get workspace members
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let member_row = workspace::ops::get_workspace_member(member_uid, &state.pg_pool, &workspace_id)
    .await
    .map_err(|_| {
      AppResponseError::new(
        ErrorCode::MemberNotFound,
        format!(
          "requested member uid {} is not present in workspace {}",
          member_uid, workspace_id
        ),
      )
    })?;
  let member: AFWorkspaceMember = member_row.into();

  Ok(AppResponse::Ok().with_data(member).into())
}

// This use user uuid as opposed to uid
#[instrument(skip_all, err)]
async fn get_workspace_member_v1_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  path: web::Path<(Uuid, Uuid)>,
) -> Result<JsonAppResponse<AFWorkspaceMember>> {
  let (workspace_id, member_uuid) = path.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  // Guest users can not get workspace members
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let member_row =
    workspace::ops::get_workspace_member_by_uuid(member_uuid, &state.pg_pool, workspace_id)
      .await
      .map_err(|_| {
        AppResponseError::new(
          ErrorCode::MemberNotFound,
          format!(
            "requested member uid {} is not present in workspace {}",
            member_uuid, workspace_id
          ),
        )
      })?;
  let member: AFWorkspaceMember = member_row.into();

  Ok(AppResponse::Ok().with_data(member).into())
}

#[instrument(level = "debug", skip_all, err)]
async fn open_workspace_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<AFWorkspace>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;
  let workspace =
    workspace::ops::open_workspace(&state.pg_pool, &user_uuid, uid, &workspace_id).await?;
  Ok(AppResponse::Ok().with_data(workspace).into())
}

#[instrument(level = "debug", skip_all, err)]
async fn leave_workspace_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<()>> {
  let workspace_id = workspace_id.into_inner();
  workspace::ops::leave_workspace(
    &state.pg_pool,
    &workspace_id,
    &user_uuid,
    state.workspace_access_control.clone(),
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

#[instrument(level = "debug", skip_all, err)]
async fn update_workspace_member_handler(
  user_uuid: UserUuid,
  payload: Json<WorkspaceMemberChangeset>,
  state: Data<AppState>,
  workspace_id: web::Path<Uuid>,
) -> Result<JsonAppResponse<()>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;

  let changeset = payload.into_inner();

  // 使用 uid 直接更新成员角色（uid 是用户的唯一标识符）
  workspace::ops::update_workspace_member(
    &changeset.uid,
    &state.pg_pool,
    &workspace_id,
    &changeset,
    state.workspace_access_control.clone(),
  )
  .await?;

  Ok(AppResponse::Ok().into())
}

#[instrument(skip(state, payload))]
async fn create_collab_handler(
  user_uuid: UserUuid,
  payload: Bytes,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let params = match req.headers().get(X_COMPRESSION_TYPE) {
    None => serde_json::from_slice::<CreateCollabParams>(&payload).map_err(|err| {
      AppError::InvalidRequest(format!(
        "Failed to parse CreateCollabParams from JSON: {}",
        err
      ))
    })?,
    Some(_) => match compress_type_from_header_value(req.headers())? {
      CompressionType::Brotli { buffer_size } => {
        let decompress_data = blocking_decompress(payload.to_vec(), buffer_size).await?;
        CreateCollabParams::from_bytes(&decompress_data).map_err(|err| {
          AppError::InvalidRequest(format!(
            "Failed to parse CreateCollabParams with brotli decompression data: {}",
            err
          ))
        })?
      },
    },
  };

  let (params, workspace_id) = params.split();

  if params.object_id == workspace_id {
    // Only the object with [CollabType::Folder] can have the same object_id as workspace_id. But
    // it should use create workspace API
    return Err(
      AppError::InvalidRequest("object_id cannot be the same as workspace_id".to_string()).into(),
    );
  }

  // 容量检查
  let content_size = params.encoded_collab_v1.len() as i64;
  let resource_status = get_user_resource_limit_status(&state.pg_pool, uid).await?;
  let total_limit_bytes = (resource_status.storage_limit_mb * 1024.0 * 1024.0) as i64;
  let current_usage = get_user_total_usage_bytes(&state.pg_pool, uid).await?;
  if current_usage + content_size > total_limit_bytes {
    return Err(
      AppError::PlanLimitExceeded(format!(
        "Storage limit exceeded. Current: {} bytes, Limit: {} bytes, Data: {} bytes",
        current_usage, total_limit_bytes, content_size
      ))
      .into(),
    );
  }

  let collab = collab_from_encode_collab(&params.object_id, &params.encoded_collab_v1)
    .await
    .map_err(|err| {
      AppError::NoRequiredData(format!(
        "Failed to create collab from encoded collab: {}",
        err
      ))
    })?;

  if let Err(err) = params.collab_type.validate_require_data(&collab) {
    return Err(
      AppError::NoRequiredData(format!(
        "collab doc state is not correct:{},{}",
        params.object_id, err
      ))
      .into(),
    );
  }

  if state
    .indexer_scheduler
    .can_index_workspace(&workspace_id)
    .await?
  {
    if let Ok(paragraphs) = Document::open(collab).map(|doc| doc.paragraphs()) {
      let pending = UnindexedCollabTask::new(
        workspace_id,
        params.object_id,
        params.collab_type,
        UnindexedData::Paragraphs(paragraphs),
      );
      state
        .indexer_scheduler
        .index_pending_collab_one(pending, false)?;
    }
  }

  let mut transaction = state
    .pg_pool
    .begin()
    .await
    .context("acquire transaction to upsert collab")
    .map_err(AppError::from)?;
  let start = Instant::now();

  let action = format!("Create new collab: {}", params);
  state
    .collab_storage
    .upsert_new_collab_with_transaction(workspace_id, &uid, params, &mut transaction, &action)
    .await?;

  transaction
    .commit()
    .await
    .context("fail to commit the transaction to upsert collab")
    .map_err(AppError::from)?;
  state.metrics.collab_metrics.observe_pg_tx(start.elapsed());

  Ok(Json(AppResponse::Ok()))
}

#[instrument(skip(state, payload), err)]
async fn batch_create_collab_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  mut payload: Payload,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  let compress_type = compress_type_from_header_value(req.headers())?;
  event!(tracing::Level::DEBUG, "start decompressing collab list");

  let mut payload_buffer = Vec::new();
  let mut offset_len_list = Vec::new();
  let mut current_offset = 0;
  let start = Instant::now();
  while let Some(item) = payload.next().await {
    if let Ok(bytes) = item {
      payload_buffer.extend_from_slice(&bytes);
      while current_offset + 4 <= payload_buffer.len() {
        // The length of the next frame is determined by the first 4 bytes
        let size = u32::from_be_bytes([
          payload_buffer[current_offset],
          payload_buffer[current_offset + 1],
          payload_buffer[current_offset + 2],
          payload_buffer[current_offset + 3],
        ]) as usize;

        // Ensure there is enough data for the frame (4 bytes for size + `size` bytes for data)
        if current_offset + 4 + size > payload_buffer.len() {
          break;
        }

        // Collect the (offset, len) for the current frame (data starts at current_offset + 4)
        offset_len_list.push((current_offset + 4, size));
        current_offset += 4 + size;
      }
    }
  }
  // Perform decompression and processing in a Rayon thread pool
  let mut collab_params_list = tokio::task::spawn_blocking(move || match compress_type {
    CompressionType::Brotli { buffer_size } => offset_len_list
      .into_par_iter()
      .filter_map(|(offset, len)| {
        let compressed_data = &payload_buffer[offset..offset + len];
        match decompress(compressed_data.to_vec(), buffer_size) {
          Ok(decompressed_data) => {
            let params = CreateCollabData::from_bytes(&decompressed_data).ok()?;
            let params = CollabParams::from(params);
            if params.validate().is_ok() {
              let encoded_collab =
                EncodedCollab::decode_from_bytes(&params.encoded_collab_v1).ok()?;
              let options = CollabOptions::new(params.object_id.to_string(), default_client_id())
                .with_data_source(DataSource::DocStateV1(encoded_collab.doc_state.to_vec()));
              let collab = Collab::new_with_options(CollabOrigin::Empty, options).ok()?;

              match params.collab_type.validate_require_data(&collab) {
                Ok(_) => {
                  match params.collab_type {
                    CollabType::Document => {
                      let index_text = Document::open(collab).map(|doc| doc.paragraphs());
                      Some((Some(index_text), params))
                    },
                    _ => {
                      // TODO(nathan): support other types
                      Some((None, params))
                    },
                  }
                },
                Err(_) => None,
              }
            } else {
              None
            }
          },
          Err(err) => {
            error!("Failed to decompress data: {:?}", err);
            None
          },
        }
      })
      .collect::<Vec<_>>(),
  })
  .await
  .map_err(|_| AppError::InvalidRequest("Failed to decompress data".to_string()))?;

  if collab_params_list.is_empty() {
    return Err(AppError::InvalidRequest("Empty collab params list".to_string()).into());
  }

  let total_size = collab_params_list
    .iter()
    .fold(0, |acc, x| acc + x.1.encoded_collab_v1.len());
  tracing::info!(
    "decompressed {} collab objects in {:?}",
    collab_params_list.len(),
    start.elapsed()
  );

  // 云空间容量检查
  check_user_storage_limit(&state.pg_pool, uid, total_size as i64).await?;

  let mut pending_undexed_collabs = vec![];
  if state
    .indexer_scheduler
    .can_index_workspace(&workspace_id)
    .await?
  {
    pending_undexed_collabs = collab_params_list
      .iter_mut()
      .filter(|p| state.indexer_scheduler.is_indexing_enabled(p.1.collab_type))
      .flat_map(|value| match std::mem::take(&mut value.0) {
        None => None,
        Some(text) => text
          .map(|paragraphs| {
            UnindexedCollabTask::new(
              workspace_id,
              value.1.object_id,
              value.1.collab_type,
              UnindexedData::Paragraphs(paragraphs),
            )
          })
          .ok(),
      })
      .collect::<Vec<_>>();
  }

  let collab_params_list = collab_params_list
    .into_iter()
    .map(|(_, params)| params)
    .collect::<Vec<_>>();

  let start = Instant::now();
  state
    .collab_storage
    .batch_insert_new_collab(workspace_id, &uid, collab_params_list)
    .await?;

  tracing::info!(
    "inserted collab objects to disk in {:?}, total size:{}",
    start.elapsed(),
    total_size
  );

  // Must after batch_insert_new_collab
  if !pending_undexed_collabs.is_empty() {
    state
      .indexer_scheduler
      .index_pending_collabs(pending_undexed_collabs)?;
  }

  Ok(Json(AppResponse::Ok()))
}

// Deprecated
async fn get_collab_handler(
  user_uuid: UserUuid,
  payload: Json<QueryCollabParams>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<CollabResponse>>> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let params = payload.into_inner();
  params
    .validate()
    .map_err(|err| AppError::InvalidRequest(err.to_string()))?;

  let encode_collab = state
    .collab_storage
    .get_full_encode_collab(
      uid.into(),
      &params.workspace_id,
      &params.object_id,
      params.collab_type,
    )
    .await
    .map_err(AppResponseError::from)?
    .encoded_collab;

  let resp = CollabResponse {
    encode_collab,
    object_id: params.object_id,
  };

  Ok(Json(AppResponse::Ok().with_data(resp)))
}

async fn v1_get_collab_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  query: web::Query<CollabTypeParam>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<CollabResponse>>> {
  let (workspace_id, object_id) = path.into_inner();
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let encode_collab = state
    .collab_storage
    .get_full_encode_collab(uid.into(), &workspace_id, &object_id, query.collab_type)
    .await;
  if let Err(x) = &encode_collab {
    eprintln!("Failed to encode collab object: {:?}", x);
  }
  let encode_collab = encode_collab
    .map_err(AppResponseError::from)?
    .encoded_collab;

  let resp = CollabResponse {
    encode_collab,
    object_id,
  };

  Ok(Json(AppResponse::Ok().with_data(resp)))
}

#[instrument(level = "trace", skip_all)]
async fn get_collab_json_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  query: web::Query<CollabTypeParam>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<CollabJsonResponse>>> {
  let (workspace_id, object_id) = path.into_inner();
  let collab_type = query.into_inner().collab_type;
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let doc_state = state
    .collab_storage
    .get_full_encode_collab(uid.into(), &workspace_id, &object_id, collab_type)
    .await
    .map_err(AppResponseError::from)?
    .encoded_collab
    .doc_state;
  let collab = collab_from_doc_state(doc_state.to_vec(), &object_id, default_client_id())?;

  let resp = CollabJsonResponse {
    collab: collab.to_json_value(),
  };

  Ok(Json(AppResponse::Ok().with_data(resp)))
}

#[instrument(level = "debug", skip_all)]
async fn post_web_update_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  payload: Json<UpdateCollabWebParams>,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let (workspace_id, object_id) = path.into_inner();
  state
    .collab_access_control
    .enforce_action(&workspace_id, &uid, &object_id, Action::Write)
    .await?;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  trace!("create onetime web realtime user: {}", user);

  let payload = payload.into_inner();
  let collab_type = payload.collab_type;

  update_page_collab_data(
    &state,
    user,
    workspace_id,
    object_id,
    collab_type,
    payload.doc_state,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

#[instrument(level = "debug", skip_all)]
async fn get_row_document_collab_exists_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<AFDatabaseRowDocumentCollabExistenceInfo>>> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let (workspace_id, object_id) = path.into_inner();
  state
    .collab_access_control
    .enforce_action(&workspace_id, &uid, &object_id, Action::Read)
    .await?;
  let exists = check_if_row_document_collab_exists(&state.pg_pool, &object_id).await?;
  Ok(Json(AppResponse::Ok().with_data(
    AFDatabaseRowDocumentCollabExistenceInfo { exists },
  )))
}

async fn post_space_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<CreateSpaceParams>,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<Space>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let space = create_space(
    &state,
    user,
    workspace_uuid,
    &payload.space_permission,
    &payload.name,
    &payload.space_icon,
    &payload.space_icon_color,
    payload.view_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(space)))
}

async fn update_space_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<UpdateSpaceParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<Space>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_space(
    &state,
    user,
    workspace_uuid,
    &view_id,
    &payload.space_permission,
    &payload.name,
    &payload.space_icon,
    &payload.space_icon_color,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn post_folder_view_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<CreateFolderViewParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<Page>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let page = create_folder_view(
    &state,
    user,
    workspace_uuid,
    &payload.parent_view_id,
    payload.layout.clone(),
    payload.name.as_deref(),
    payload.view_id,
    payload.database_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(page)))
}

async fn post_page_view_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<CreatePageParams>,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<Page>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let page = create_page(
    &state,
    user,
    workspace_uuid,
    &payload.parent_view_id,
    &payload.layout,
    payload.name.as_deref(),
    payload.page_data.as_ref(),
    payload.view_id,
    payload.collab_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(page)))
}

async fn post_orphaned_view_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<CreateOrphanedViewParams>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  create_orphaned_view(
    uid,
    &state.pg_pool,
    &state.collab_storage,
    workspace_uuid,
    payload.document_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn append_block_to_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<AppendBlockToPageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let serde_blocks: Vec<Result<SerdeBlock, AppError>> = payload
    .blocks
    .iter()
    .map(|value| {
      serde_json::from_value(value.clone()).map_err(|err| AppError::InvalidBlock(err.to_string()))
    })
    .collect_vec();
  let serde_blocks = serde_blocks
    .into_iter()
    .collect::<Result<Vec<SerdeBlock>, AppError>>()?;
  append_block_at_the_end_of_page(&state, user, workspace_uuid, &view_id, &serde_blocks).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn move_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<MovePageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  move_page(
    &state,
    user,
    workspace_uuid,
    &view_id,
    &payload.new_parent_view_id,
    payload.prev_view_id.clone(),
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn reorder_favorite_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<ReorderFavoritePageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  reorder_favorite_page(
    &state,
    user,
    workspace_uuid,
    &view_id,
    payload.prev_view_id.as_deref(),
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn duplicate_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  payload: Json<DuplicatePageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let suffix = payload.suffix.as_deref().unwrap_or(" (Copy)").to_string();
  duplicate_view_tree_and_collab(&state, user, workspace_uuid, view_id, &suffix).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn move_page_to_trash_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  move_page_to_trash(&state, user, workspace_uuid, &view_id).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn restore_page_from_trash_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  restore_page_from_trash(&state, user, workspace_uuid, &view_id).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn add_recent_pages_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  payload: Json<AddRecentPagesParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let AddRecentPagesParams { recent_view_ids } = payload.into_inner();
  add_recent_pages(&state, user, workspace_uuid, recent_view_ids).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn restore_all_pages_from_trash_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_uuid = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  restore_all_pages_from_trash(&state, user, workspace_uuid).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_page_from_trash_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let (workspace_id, view_id) = path.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  delete_trash(&state, user, workspace_id, &view_id).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_all_pages_from_trash_handler(
  user_uuid: UserUuid,
  path: web::Path<Uuid>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let workspace_id = path.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  delete_all_pages_from_trash(&state, user, workspace_id).await?;
  Ok(Json(AppResponse::Ok()))
}

#[instrument(level = "trace", skip_all)]
async fn publish_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  payload: Json<PublishPageParams>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let (workspace_id, view_id) = path.into_inner();
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let PublishPageParams {
    publish_name,
    visible_database_view_ids,
    comments_enabled,
    duplicate_enabled,
  } = payload.into_inner();
  publish_page(
    &state,
    uid,
    *user_uuid,
    workspace_id,
    view_id,
    visible_database_view_ids,
    publish_name,
    comments_enabled.unwrap_or(true),
    duplicate_enabled.unwrap_or(true),
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn unpublish_page_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let (workspace_uuid, view_uuid) = path.into_inner();
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_uuid, AFRole::Member)
    .await?;
  unpublish_page(
    state.published_collab_store.as_ref(),
    workspace_uuid,
    *user_uuid,
    view_uuid,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn post_page_database_view_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  payload: Json<CreatePageDatabaseViewParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let (workspace_uuid, view_id) = path.into_inner();
  create_database_view(
    &state,
    user,
    workspace_uuid,
    &view_id,
    &payload.layout,
    payload.name.as_deref(),
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn update_page_view_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<UpdatePageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let icon = payload.icon.as_ref();
  let is_locked = payload.is_locked;
  let extra = payload
    .extra
    .as_ref()
    .map(|json_value| json_value.to_string());
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_page(
    &state,
    user,
    workspace_uuid,
    &view_id,
    &payload.name,
    icon,
    is_locked,
    extra.as_ref(),
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn update_page_name_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<UpdatePageNameParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_page_name(&state, user, workspace_uuid, &view_id, &payload.name).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn update_page_icon_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<UpdatePageIconParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let icon = &payload.icon;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_page_icon(&state, user, workspace_uuid, &view_id, Some(icon)).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn update_page_extra_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<UpdatePageExtraParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_page_extra(&state, user, workspace_uuid, &view_id, &payload.extra).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn remove_page_icon_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  update_page_icon(&state, user, workspace_uuid, &view_id, None).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn get_page_view_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PageCollab>>> {
  let (workspace_uuid, view_id) = path.into_inner();
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let page_collab = get_page_view_collab(
    &state.pg_pool,
    &state.collab_storage,
    &state.ws_server,
    uid,
    workspace_uuid,
    view_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(page_collab)))
}

async fn favorite_page_view_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  payload: Json<FavoritePageParams>,
  state: Data<AppState>,

  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let (workspace_uuid, view_id) = path.into_inner();
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  favorite_page(
    &state,
    user,
    workspace_uuid,
    &view_id,
    payload.is_favorite,
    payload.is_pinned,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

#[instrument(level = "debug", skip(payload, state), err)]
async fn batch_get_collab_handler(
  user_uuid: UserUuid,
  path: Path<Uuid>,
  state: Data<AppState>,
  payload: Json<BatchQueryCollabParams>,
) -> Result<Json<AppResponse<BatchQueryCollabResult>>> {
  let workspace_id = path.into_inner();
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;
  let result = BatchQueryCollabResult(
    state
      .collab_storage
      .batch_get_collab(&uid, workspace_id, payload.into_inner().0)
      .await,
  );
  Ok(Json(AppResponse::Ok().with_data(result)))
}

#[instrument(skip(state, payload), err)]
async fn update_collab_handler(
  user_uuid: UserUuid,
  payload: Json<CreateCollabParams>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let (params, workspace_id) = payload.into_inner().split();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;

  let create_params = CreateCollabParams::from((workspace_id, params));
  let (params, workspace_id) = create_params.split();
  if state
    .indexer_scheduler
    .can_index_workspace(&workspace_id)
    .await?
  {
    match params.collab_type {
      CollabType::Document => {
        let collab = collab_from_encode_collab(&params.object_id, &params.encoded_collab_v1)
          .await
          .map_err(|err| {
            AppError::InvalidRequest(format!(
              "Failed to create collab from encoded collab: {}",
              err
            ))
          })?;
        params
          .collab_type
          .validate_require_data(&collab)
          .map_err(|err| {
            AppError::NoRequiredData(format!(
              "collab doc state is not correct:{},{}",
              params.object_id, err
            ))
          })?;

        if let Ok(paragraphs) = Document::open(collab).map(|doc| doc.paragraphs()) {
          if !paragraphs.is_empty() {
            let pending = UnindexedCollabTask::new(
              workspace_id,
              params.object_id,
              params.collab_type,
              UnindexedData::Paragraphs(paragraphs),
            );
            state
              .indexer_scheduler
              .index_pending_collab_one(pending, true)?;
          }
        }
      },
      _ => {
        // TODO(nathan): support other collab type
      },
    }
  }

  state
    .collab_storage
    .upsert_collab_background(workspace_id, &uid, params)
    .await?;
  Ok(AppResponse::Ok().into())
}

#[instrument(level = "info", skip(state, payload), err)]
async fn delete_collab_handler(
  user_uuid: UserUuid,
  payload: Json<DeleteCollabParams>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let payload = payload.into_inner();
  payload.validate().map_err(AppError::from)?;

  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  state
    .collab_storage
    .delete_collab(&payload.workspace_id, &uid, &payload.object_id)
    .await
    .map_err(AppResponseError::from)?;

  Ok(AppResponse::Ok().into())
}

async fn put_workspace_default_published_view_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  payload: Json<UpdateDefaultPublishView>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;
  let new_default_pub_view_id = payload.into_inner().view_id;
  biz::workspace::publish::set_workspace_default_publish_view(
    &state.pg_pool,
    &workspace_id,
    &new_default_pub_view_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_workspace_default_published_view_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;
  biz::workspace::publish::unset_workspace_default_publish_view(&state.pg_pool, &workspace_id)
    .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn get_workspace_published_default_info_handler(
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PublishInfo>>> {
  let workspace_id = workspace_id.into_inner();
  let info =
    biz::workspace::publish::get_workspace_default_publish_view_info(&state.pg_pool, &workspace_id)
      .await?;
  Ok(Json(AppResponse::Ok().with_data(info)))
}

async fn put_publish_namespace_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  payload: Json<UpdatePublishNamespace>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;
  let UpdatePublishNamespace {
    old_namespace,
    new_namespace,
  } = payload.into_inner();
  biz::workspace::publish::set_workspace_namespace(
    &state.pg_pool,
    &workspace_id,
    &old_namespace,
    &new_namespace,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn get_publish_namespace_handler(
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<String>>> {
  let workspace_id = workspace_id.into_inner();
  let namespace =
    biz::workspace::publish::get_workspace_publish_namespace(&state.pg_pool, &workspace_id).await?;
  Ok(Json(AppResponse::Ok().with_data(namespace)))
}

async fn get_default_published_collab_info_meta_handler(
  publish_namespace: web::Path<String>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PublishInfoMeta<serde_json::Value>>>> {
  let publish_namespace = publish_namespace.into_inner();
  let (info, meta) =
    get_workspace_default_publish_view_info_meta(&state.pg_pool, &publish_namespace).await?;
  Ok(Json(
    AppResponse::Ok().with_data(PublishInfoMeta { info, meta }),
  ))
}

async fn get_v1_published_collab_handler(
  path_param: web::Path<(String, String)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<serde_json::Value>>> {
  let (workspace_namespace, publish_name) = path_param.into_inner();
  let metadata = state
    .published_collab_store
    .get_collab_metadata(&workspace_namespace, &publish_name)
    .await?;
  Ok(Json(AppResponse::Ok().with_data(metadata)))
}

async fn get_published_collab_blob_handler(
  path_param: web::Path<(String, String)>,
  state: Data<AppState>,
) -> Result<Vec<u8>> {
  let (publish_namespace, publish_name) = path_param.into_inner();
  let collab_data = state
    .published_collab_store
    .get_collab_blob_by_publish_namespace(&publish_namespace, &publish_name)
    .await?;
  Ok(collab_data)
}

async fn post_published_duplicate_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  params: Json<PublishedDuplicate>,
) -> Result<Json<AppResponse<DuplicatePublishedPageResponse>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;
  let params = params.into_inner();
  // Manual duplicate: is_readonly = false (user gets a fully editable copy)
  let root_view_id_for_duplicate =
    biz::workspace::publish_dup::duplicate_published_collab_to_workspace(
      &state,
      uid,
      params.published_view_id,
      workspace_id,
      params.dest_view_id,
      false, // not readonly for manual duplicate
    )
    .await?;

  Ok(Json(AppResponse::Ok().with_data(
    DuplicatePublishedPageResponse {
      view_id: root_view_id_for_duplicate,
    },
  )))
}

async fn list_published_collab_info_handler(
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<Vec<PublishInfoView>>>> {
  let uid = DUMMY_UID;
  let publish_infos = list_collab_publish_info(
    state.published_collab_store.as_ref(),
    &state.ws_server,
    workspace_id.into_inner(),
    uid,
  )
  .await?;

  Ok(Json(AppResponse::Ok().with_data(publish_infos)))
}

/// 获取当前用户相关的发布文档列表
/// 只返回：1. 用户自己发布的文档 2. 用户通过深度链接接收的其他用户发布的文档
async fn list_all_published_collab_info_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<Json<AppResponse<ListAllPublishedCollabResponse>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;

  // 1. 获取当前用户自己发布的文档
  let own_published = select_published_collab_by_uid(&state.pg_pool, uid).await?;

  // 2. 获取当前用户通过深度链接接收的其他用户发布的文档（含详情）
  let received_details = select_received_published_collab_with_details(&state.pg_pool, uid).await?;

  let mut items: Vec<AllPublishedCollabItem> = Vec::new();

  // 处理自己发布的文档
  for info in &own_published {
    let real_name = match state
      .published_collab_store
      .get_collab_metadata(&info.namespace, &info.publish_name)
      .await
    {
      Ok(metadata) => {
        if let Some(name) = metadata.get("view").and_then(|v| v.get("name")) {
          name.as_str().unwrap_or(&info.publish_name).to_string()
        } else {
          info.publish_name.clone()
        }
      },
      Err(_) => info.publish_name.clone(),
    };

    items.push(AllPublishedCollabItem {
      published_view_id: info.view_id,
      view_id: info.view_id,
      workspace_id: Uuid::nil(),
      name: real_name,
      publish_name: info.publish_name.clone(),
      publisher_email: info.publisher_email.clone(),
      published_at: info.publish_timestamp,
      is_received: false,
      is_readonly: false,
    });
  }

  // 处理接收的其他用户发布的文档
  for detail in &received_details {
    let real_name = if !detail.namespace.is_empty() && !detail.publish_name.is_empty() {
      match state
        .published_collab_store
        .get_collab_metadata(&detail.namespace, &detail.publish_name)
        .await
      {
        Ok(metadata) => {
          if let Some(name) = metadata.get("view").and_then(|v| v.get("name")) {
            name.as_str().unwrap_or(&detail.publish_name).to_string()
          } else {
            detail.publish_name.clone()
          }
        },
        Err(_) => detail.publish_name.clone(),
      }
    } else {
      detail.publish_name.clone()
    };

    items.push(AllPublishedCollabItem {
      published_view_id: detail.published_view_id,
      view_id: detail.received_view_id,
      workspace_id: detail.workspace_id,
      name: real_name,
      publish_name: detail.publish_name.clone(),
      publisher_email: detail.publisher_email.clone(),
      published_at: detail.published_at,
      is_received: true,
      is_readonly: detail.is_readonly,
    });
  }

  // 按发布时间倒序排序
  items.sort_by(|a, b| b.published_at.cmp(&a.published_at));

  Ok(Json(AppResponse::Ok().with_data(ListAllPublishedCollabResponse { items })))
}

/// 接收发布的文档（复制到自己的工作区）
/// 发布的文档对接收者默认是只读的，不能协作同步
async fn receive_published_collab_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  params: Json<ReceivePublishedCollabRequest>,
) -> Result<Json<AppResponse<ReceivePublishedCollabResponse>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let params = params.into_inner();

  // 验证发布文档是否存在
  let publish_info = state
    .published_collab_store
    .get_collab_publish_info(&params.published_view_id)
    .await
    .map_err(|e| AppResponseError::new(ErrorCode::RecordNotFound, e.to_string()))?;

  if publish_info.unpublished_timestamp.is_some() {
    return Err(AppError::RecordNotFound("Collab is unpublished".to_string()).into());
  }

  // 检查是否已接收过
  let existing = sqlx::query_as!(
    AFReceivedPublishedCollab,
    r#"
      SELECT * FROM af_received_published_collab
      WHERE received_by = $1 AND published_view_id = $2
    "#,
    uid,
    params.published_view_id,
  )
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  if let Some(existing) = existing {
    // 已接收过，直接返回
    return Ok(Json(AppResponse::Ok().with_data(ReceivePublishedCollabResponse {
      view_id: existing.view_id,
      is_readonly: existing.is_readonly,
    })));
  }

  // 复制发布文档到用户工作区
  // is_readonly = true for receive operation (published doc should be readonly)
  let root_view_id = biz::workspace::publish_dup::duplicate_published_collab_to_workspace(
    &state,
    uid,
    params.published_view_id,
    params.dest_workspace_id,
    params.dest_view_id,
    true, // readonly for received published collab
  )
  .await
  .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  // 直接从 af_published_collab 表查询发布者 uid（兼容手机号和邮箱注册用户）
  let published_by_uid = sqlx::query_scalar!(
    r#"SELECT published_by FROM af_published_collab WHERE view_id = $1"#,
    params.published_view_id,
  )
  .fetch_one(&state.pg_pool)
  .await
  .map_err(|e| AppResponseError::new(ErrorCode::Internal, format!("Failed to get publisher uid: {}", e)))?;

  // 记录接收关系
  insert_received_published_collab(
    &state.pg_pool,
    uid,
    &params.published_view_id,
    &params.dest_workspace_id,
    &root_view_id,
    published_by_uid,
    &publish_info.publish_timestamp,
    true, // 发布文档默认只读
  )
  .await
  .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  Ok(Json(AppResponse::Ok().with_data(ReceivePublishedCollabResponse {
    view_id: root_view_id,
    is_readonly: true,
  })))
}

/// 查询接收的发布文档只读状态
/// 客户端传递的是复制后的 view_id（即接收者工作区中的文档ID）
/// 同时兼容 published_view_id 查询，以支持两种调用方式
async fn get_received_published_collab_readonly_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  view_id: web::Path<Uuid>,
) -> Result<Json<AppResponse<ReceivedPublishedCollabReadonlyResponse>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let query_view_id = view_id.into_inner();

  // 先按 view_id（复制后的ID）查询，再按 published_view_id（原始ID）查询
  let received = sqlx::query_as!(
    AFReceivedPublishedCollab,
    r#"
      SELECT * FROM af_received_published_collab
      WHERE received_by = $1 AND (view_id = $2 OR published_view_id = $2)
    "#,
    uid,
    query_view_id,
  )
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  match received {
    Some(rec) => Ok(Json(AppResponse::Ok().with_data(ReceivedPublishedCollabReadonlyResponse {
      is_received: true,
      is_readonly: rec.is_readonly,
    }))),
    None => Ok(Json(AppResponse::Ok().with_data(ReceivedPublishedCollabReadonlyResponse {
      is_received: false,
      is_readonly: false,
    }))),
  }
}

// Deprecated since 0.7.4
async fn get_published_collab_info_handler(
  view_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PublishInfo>>> {
  let view_id = view_id.into_inner();
  let collab_data = state
    .published_collab_store
    .get_collab_publish_info(&view_id)
    .await?;
  if collab_data.unpublished_timestamp.is_some() {
    return Err(AppError::RecordNotFound("Collab is unpublished".to_string()).into());
  }
  Ok(Json(AppResponse::Ok().with_data(collab_data)))
}

#[derive(Serialize)]
struct PublishInfoWithMetadata {
  #[serde(flatten)]
  info: PublishInfo,
  metadata: serde_json::Value,
}

async fn get_v1_published_collab_info_handler(
  view_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PublishInfoWithMetadata>>> {
  let view_id = view_id.into_inner();
  let info = state
    .published_collab_store
    .get_collab_publish_info(&view_id)
    .await?;

  let metadata = state
    .published_collab_store
    .get_collab_metadata(&info.namespace, &info.publish_name)
    .await?;

  Ok(Json(AppResponse::Ok().with_data(PublishInfoWithMetadata {
    info,
    metadata,
  })))
}

async fn get_published_collab_comment_handler(
  view_id: web::Path<Uuid>,
  optional_user_uuid: OptionalUserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<GlobalComments>> {
  let view_id = view_id.into_inner();
  let comments =
    get_comments_on_published_view(&state.pg_pool, &view_id, &optional_user_uuid).await?;
  let resp = GlobalComments { comments };
  Ok(Json(AppResponse::Ok().with_data(resp)))
}

async fn post_published_collab_comment_handler(
  user_uuid: UserUuid,
  view_id: web::Path<Uuid>,
  state: Data<AppState>,
  data: Json<CreateGlobalCommentParams>,
) -> Result<JsonAppResponse<()>> {
  let view_id = view_id.into_inner();
  create_comment_on_published_view(
    &state.pg_pool,
    &view_id,
    &data.reply_comment_id,
    &data.content,
    &user_uuid,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_published_collab_comment_handler(
  user_uuid: UserUuid,
  view_id: web::Path<Uuid>,
  state: Data<AppState>,
  data: Json<DeleteGlobalCommentParams>,
) -> Result<JsonAppResponse<()>> {
  let view_id = view_id.into_inner();
  remove_comment_on_published_view(&state.pg_pool, &view_id, &data.comment_id, &user_uuid).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn get_published_collab_reaction_handler(
  view_id: web::Path<Uuid>,
  query: web::Query<GetReactionQueryParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<Reactions>> {
  let view_id = view_id.into_inner();
  let reactions =
    get_reactions_on_published_view(&state.pg_pool, &view_id, &query.comment_id).await?;
  let resp = Reactions { reactions };
  Ok(Json(AppResponse::Ok().with_data(resp)))
}

async fn post_published_collab_reaction_handler(
  user_uuid: UserUuid,
  view_id: web::Path<Uuid>,
  data: Json<CreateReactionParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let view_id = view_id.into_inner();
  create_reaction_on_comment(
    &state.pg_pool,
    &data.comment_id,
    &view_id,
    &data.reaction_type,
    &user_uuid,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_published_collab_reaction_handler(
  user_uuid: UserUuid,
  data: Json<DeleteReactionParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  remove_reaction_on_comment(
    &state.pg_pool,
    &data.comment_id,
    &data.reaction_type,
    &user_uuid,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

// FIXME: This endpoint currently has a different behaviour from the publish page endpoint,
// as it doesn't accept parameters. We will need to deprecate this endpoint and use a new
// one that accepts parameters.
#[instrument(level = "trace", skip_all)]
async fn post_publish_collabs_handler(
  workspace_id: web::Path<Uuid>,
  user_uuid: UserUuid,
  payload: Payload,
  state: Data<AppState>,
) -> Result<Json<AppResponse<()>>> {
  let workspace_id = workspace_id.into_inner();

  let mut accumulator = Vec::<PublishCollabItem<serde_json::Value, Vec<u8>>>::new();
  let mut payload_reader: PayloadReader = PayloadReader::new(payload);

  loop {
    let meta: PublishCollabMetadata<serde_json::Value> = {
      let meta_len = payload_reader.read_u32_little_endian().await?;
      if meta_len > 4 * 1024 * 1024 {
        // 4MiB Limit for metadata
        return Err(AppError::InvalidRequest(String::from("metadata too large")).into());
      }
      if meta_len == 0 {
        break;
      }

      let mut meta_buffer = vec![0; meta_len as usize];
      payload_reader.read_exact(&mut meta_buffer).await?;
      serde_json::from_slice(&meta_buffer)?
    };

    let data = {
      let data_len = payload_reader.read_u32_little_endian().await?;
      if data_len > 32 * 1024 * 1024 {
        // 32MiB Limit for data
        return Err(AppError::InvalidRequest(String::from("data too large")).into());
      }
      let mut data_buffer = vec![0; data_len as usize];
      payload_reader.read_exact(&mut data_buffer).await?;
      data_buffer
    };

    // Set comments_enabled and duplicate_enabled to true by default, as this is the default
    // behaviour for the older web version.
    accumulator.push(PublishCollabItem {
      meta,
      data,
      comments_enabled: true,
      duplicate_enabled: true,
    });
  }

  if accumulator.is_empty() {
    return Err(
      AppError::InvalidRequest(String::from("did not receive any data to publish")).into(),
    );
  }

  // 云空间容量检查
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let total_data_size: i64 = accumulator.iter().map(|item| item.data.len() as i64).sum();
  check_user_storage_limit(&state.pg_pool, uid, total_data_size).await?;

  // 收集即将发布的 view_id 列表（用于后续清理旧接收记录）
  let published_view_ids: Vec<Uuid> = accumulator
    .iter()
    .map(|item| item.meta.view_id)
    .collect();

  state
    .published_collab_store
    .publish_collabs(accumulator, &workspace_id, &user_uuid)
    .await?;

  // 重新发布时，删除所有用户已接收的旧副本记录
  // 这样其他用户下次通过链接打开时会生成新的只读副本
  for view_id in &published_view_ids {
    let _ = sqlx::query!(
      r#"DELETE FROM af_received_published_collab WHERE published_view_id = $1"#,
      view_id,
    )
    .execute(&state.pg_pool)
    .await;
  }

  Ok(Json(AppResponse::Ok()))
}

async fn patch_published_collabs_handler(
  workspace_id: web::Path<Uuid>,
  user_uuid: UserUuid,
  state: Data<AppState>,
  patches: Json<Vec<PatchPublishedCollab>>,
) -> Result<Json<AppResponse<()>>> {
  let workspace_id = workspace_id.into_inner();
  if patches.is_empty() {
    return Err(AppError::InvalidRequest("No patches provided".to_string()).into());
  }
  state
    .published_collab_store
    .patch_collabs(&workspace_id, &user_uuid, &patches)
    .await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_published_collabs_handler(
  workspace_id: web::Path<Uuid>,
  user_uuid: UserUuid,
  state: Data<AppState>,
  view_ids: Json<Vec<Uuid>>,
) -> Result<Json<AppResponse<()>>> {
  let workspace_id = workspace_id.into_inner();
  let view_ids = view_ids.into_inner();
  if view_ids.is_empty() {
    return Err(AppError::InvalidRequest("No view_ids provided".to_string()).into());
  }
  state
    .published_collab_store
    .unpublish_collabs(&workspace_id, &view_ids, &user_uuid)
    .await?;
  Ok(Json(AppResponse::Ok()))
}

#[instrument(level = "info", skip_all, err)]
async fn post_realtime_message_stream_handler(
  user_uuid: UserUuid,
  mut payload: Payload,
  server: Data<RealtimeServerAddr>,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<Json<AppResponse<()>>> {
  let device_id = device_id_from_headers(req.headers())
    .map(|s| s.to_string())
    .unwrap_or_else(|_| "".to_string());
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let mut bytes = BytesMut::new();
  while let Some(item) = payload.next().await {
    bytes.extend_from_slice(&item?);
  }

  let device_id = device_id.to_string();

  let message = parser_realtime_msg(bytes.freeze(), req.clone()).await?;
  let stream_message = ClientHttpStreamMessage {
    uid,
    device_id,
    message,
  };

  // When the server is under heavy load, try_send may fail. In client side, it will retry to send
  // the message later.
  match server.try_send(stream_message) {
    Ok(_) => return Ok(Json(AppResponse::Ok())),
    Err(err) => Err(
      AppError::Internal(anyhow!(
        "Failed to send message to websocket server, error:{}",
        err
      ))
      .into(),
    ),
  }
}

async fn get_workspace_usage_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<WorkspaceUsage>>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let res =
    biz::workspace::ops::get_workspace_document_total_bytes(&state.pg_pool, &workspace_id).await?;
  Ok(Json(AppResponse::Ok().with_data(res)))
}

async fn get_workspace_usage_and_limit_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<shared_entity::dto::billing_dto::WorkspaceUsageAndLimit>>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let res =
    biz::workspace::ops::get_workspace_usage_and_limit(&state.pg_pool, &workspace_id).await?;
  Ok(Json(AppResponse::Ok().with_data(res)))
}

async fn get_workspace_folder_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,

  query: web::Query<QueryWorkspaceFolder>,
  req: HttpRequest,
) -> Result<Json<AppResponse<FolderView>>> {
  let depth = query.depth.unwrap_or(1);
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let user = realtime_user_for_web_request(req.headers(), uid)?;
  let workspace_id = workspace_id.into_inner();
  // shuheng: AppFlowy Web does not support guest editor yet, so we need to make sure
  // that the user is at least a member of the workspace, not just a guest.
  state
    .workspace_access_control
    .enforce_role_weak(&uid, &workspace_id, AFRole::Member)
    .await?;
  let root_view_id = query.root_view_id.unwrap_or(workspace_id);
  let folder_view = biz::collab::ops::get_user_workspace_structure(
    &state,
    user,
    workspace_id,
    depth,
    &root_view_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(folder_view)))
}

async fn get_recent_views_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<RecentSectionItems>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;
  let folder_views =
    get_user_recent_folder_views(&state.ws_server, &state.pg_pool, uid, workspace_id).await?;
  let section_items = RecentSectionItems {
    views: folder_views,
  };
  Ok(Json(AppResponse::Ok().with_data(section_items)))
}

async fn get_favorite_views_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<FavoriteSectionItems>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;
  let folder_views =
    get_user_favorite_folder_views(&state.ws_server, &state.pg_pool, uid, workspace_id).await?;
  let section_items = FavoriteSectionItems {
    views: folder_views,
  };
  Ok(Json(AppResponse::Ok().with_data(section_items)))
}

async fn get_trash_views_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<TrashSectionItems>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;
  let folder_views = get_user_trash_folder_views(&state.ws_server, uid, workspace_id).await?;
  let section_items = TrashSectionItems {
    views: folder_views,
  };
  Ok(Json(AppResponse::Ok().with_data(section_items)))
}

async fn get_workspace_publish_outline_handler(
  publish_namespace: web::Path<String>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<PublishedView>>> {
  let uid = DUMMY_UID;
  let published_view = biz::collab::ops::get_published_view(
    &state.ws_server,
    publish_namespace.into_inner(),
    &state.pg_pool,
    uid,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(published_view)))
}

async fn list_database_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<Vec<AFDatabase>>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let workspace_id = workspace_id.into_inner();
  let dbs = biz::collab::ops::list_database(
    &state.pg_pool,
    &state.ws_server,
    &state.collab_storage,
    uid,
    workspace_id,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(dbs)))
}

async fn list_database_row_id_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<Vec<AFDatabaseRow>>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;

  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;

  let db_rows =
    biz::collab::ops::list_database_row_ids(&state.collab_storage, workspace_id, db_id).await?;
  Ok(Json(AppResponse::Ok().with_data(db_rows)))
}

async fn post_database_row_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  add_database_row: Json<AddDatatabaseRow>,
) -> Result<Json<AppResponse<String>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;

  let AddDatatabaseRow { cells, document } = add_database_row.into_inner();

  let new_db_row_id =
    biz::collab::ops::insert_database_row(&state, workspace_id, db_id, uid, None, cells, document)
      .await?;
  Ok(Json(AppResponse::Ok().with_data(new_db_row_id)))
}

async fn put_database_row_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  upsert_db_row: Json<UpsertDatatabaseRow>,
) -> Result<Json<AppResponse<String>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;

  let UpsertDatatabaseRow {
    pre_hash,
    cells,
    document,
  } = upsert_db_row.into_inner();

  let row_id = {
    let mut hasher = Sha256::new();
    hasher.update(workspace_id);
    hasher.update(db_id);
    hasher.update(pre_hash);
    let hash = hasher.finalize();
    Uuid::from_bytes([
      // take 16 out of 32 bytes
      hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7], hash[8], hash[9],
      hash[10], hash[11], hash[12], hash[13], hash[14], hash[15],
    ])
  };

  biz::collab::ops::upsert_database_row(&state, workspace_id, db_id, uid, row_id, cells, document)
    .await?;
  Ok(Json(AppResponse::Ok().with_data(row_id.to_string())))
}

async fn get_database_fields_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<Vec<AFDatabaseField>>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;

  let db_fields =
    biz::collab::ops::get_database_fields(&state.collab_storage, workspace_id, db_id).await?;

  Ok(Json(AppResponse::Ok().with_data(db_fields)))
}

async fn post_database_fields_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  field: Json<AFInsertDatabaseField>,
) -> Result<Json<AppResponse<String>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Write)
    .await?;

  let field_id =
    biz::collab::ops::add_database_field(&state, workspace_id, db_id, field.into_inner()).await?;

  Ok(Json(AppResponse::Ok().with_data(field_id)))
}

async fn list_database_row_id_updated_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  param: web::Query<ListDatabaseRowUpdatedParam>,
) -> Result<Json<AppResponse<Vec<DatabaseRowUpdatedItem>>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;

  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;

  // Default to 1 hour ago
  let after: DateTime<Utc> = param
    .after
    .unwrap_or_else(|| Utc::now() - Duration::hours(1));

  let db_rows = biz::collab::ops::list_database_row_ids_updated(
    &state.collab_storage,
    &state.pg_pool,
    workspace_id,
    db_id,
    &after,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(db_rows)))
}

async fn list_database_row_details_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  param: web::Query<ListDatabaseRowDetailParam>,
) -> Result<Json<AppResponse<Vec<AFDatabaseRowDetail>>>> {
  let (workspace_id, db_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let list_db_row_query = param.into_inner();
  let with_doc = list_db_row_query.with_doc.unwrap_or_default();
  let row_ids = list_db_row_query.into_ids()?;

  state
    .workspace_access_control
    .enforce_action(&uid, &workspace_id, Action::Read)
    .await?;

  static UNSUPPORTED_FIELD_TYPES: &[FieldType] = &[FieldType::Relation];

  let db_rows = biz::collab::ops::list_database_row_details(
    &state.collab_storage,
    uid,
    workspace_id,
    db_id,
    &row_ids,
    UNSUPPORTED_FIELD_TYPES,
    with_doc,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(db_rows)))
}

#[inline]
async fn parser_realtime_msg(
  payload: Bytes,
  req: HttpRequest,
) -> Result<RealtimeMessage, AppError> {
  let HttpRealtimeMessage {
    device_id: _,
    payload,
  } =
    HttpRealtimeMessage::decode(payload.as_ref()).map_err(|err| AppError::Internal(err.into()))?;
  let payload = match req.headers().get(X_COMPRESSION_TYPE) {
    None => payload,
    Some(_) => match compress_type_from_header_value(req.headers())? {
      CompressionType::Brotli { buffer_size } => {
        let decompressed_data = blocking_decompress(payload, buffer_size).await?;
        event!(
          tracing::Level::TRACE,
          "Decompress realtime http message with len: {}",
          decompressed_data.len()
        );
        decompressed_data
      },
    },
  };
  let message = Message::from(payload);
  match message {
    Message::Binary(bytes) => {
      let realtime_msg = tokio::task::spawn_blocking(move || {
        RealtimeMessage::decode(&bytes).map_err(|err| {
          AppError::InvalidRequest(format!("Failed to parse RealtimeMessage: {}", err))
        })
      })
      .await
      .map_err(AppError::from)??;
      Ok(realtime_msg)
    },
    _ => Err(AppError::InvalidRequest(format!(
      "Unsupported message type: {:?}",
      message
    ))),
  }
}

#[instrument(level = "debug", skip_all)]
async fn get_collab_embed_info_handler(
  path: web::Path<(String, Uuid)>,
  state: Data<AppState>,
) -> Result<Json<AppResponse<AFCollabEmbedInfo>>> {
  let (_, object_id) = path.into_inner();
  let info = database::collab::select_collab_embed_info(&state.pg_pool, &object_id)
    .await
    .map_err(AppResponseError::from)?
    .ok_or_else(|| {
      AppError::RecordNotFound(format!(
        "Embedding for given object:{} not found",
        object_id
      ))
    })?;
  Ok(Json(AppResponse::Ok().with_data(info)))
}

async fn force_generate_collab_embedding_handler(
  path: web::Path<(Uuid, Uuid)>,
  server: Data<RealtimeServerAddr>,
) -> Result<Json<AppResponse<()>>> {
  let (workspace_id, object_id) = path.into_inner();
  let request = ClientGenerateEmbeddingMessage {
    workspace_id,
    object_id,
    return_tx: None,
  };
  let _ = server.try_send(request);
  Ok(Json(AppResponse::Ok()))
}

#[instrument(level = "debug", skip_all)]
async fn batch_get_collab_embed_info_handler(
  state: Data<AppState>,
  payload: Json<RepeatedEmbeddedCollabQuery>,
) -> Result<Json<AppResponse<RepeatedAFCollabEmbedInfo>>> {
  let payload = payload.into_inner();
  let info = database::collab::batch_select_collab_embed(&state.pg_pool, payload.0)
    .await
    .map_err(AppResponseError::from)?;
  Ok(Json(AppResponse::Ok().with_data(info)))
}

#[instrument(level = "debug", skip_all, err)]
async fn collab_full_sync_handler(
  user_uuid: UserUuid,
  body: Bytes,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  server: Data<RealtimeServerAddr>,
  req: HttpRequest,
) -> Result<HttpResponse> {
  if body.is_empty() {
    return Err(AppError::InvalidRequest("body is empty".to_string()).into());
  }

  // when the payload size exceeds the limit, we consider it as an invalid payload.
  const MAX_BODY_SIZE: usize = 1024 * 1024 * 50; // 50MB
  if body.len() > MAX_BODY_SIZE {
    error!("Unexpected large body size: {}", body.len());
    return Err(
      AppError::InvalidRequest(format!("body size exceeds limit: {}", MAX_BODY_SIZE)).into(),
    );
  }

  let (workspace_id, object_id) = path.into_inner();
  let params = CollabDocStateParams::decode(&mut Cursor::new(body)).map_err(|err| {
    AppError::InvalidRequest(format!("Failed to parse CollabDocStateParams: {}", err))
  })?;

  if params.doc_state.is_empty() {
    return Err(AppError::InvalidRequest("doc state is empty".to_string()).into());
  }

  let collab_type = CollabType::from(params.collab_type);
  let compression_type = PayloadCompressionType::try_from(params.compression).map_err(|err| {
    AppError::InvalidRequest(format!("Failed to parse PayloadCompressionType: {}", err))
  })?;

  let doc_state = match compression_type {
    PayloadCompressionType::None => params.doc_state,
    PayloadCompressionType::Zstd => tokio::task::spawn_blocking(move || {
      zstd::decode_all(&*params.doc_state)
        .map_err(|err| AppError::InvalidRequest(format!("Failed to decompress doc_state: {}", err)))
    })
    .await
    .map_err(AppError::from)??,
  };

  let sv = match compression_type {
    PayloadCompressionType::None => params.sv,
    PayloadCompressionType::Zstd => tokio::task::spawn_blocking(move || {
      zstd::decode_all(&*params.sv)
        .map_err(|err| AppError::InvalidRequest(format!("Failed to decompress sv: {}", err)))
    })
    .await
    .map_err(AppError::from)??,
  };

  let app_version = client_version_from_headers(req.headers())
    .map(|s| s.to_string())
    .unwrap_or_else(|_| "".to_string());
  let device_id = device_id_from_headers(req.headers())
    .map(|s| s.to_string())
    .unwrap_or_else(|_| "".to_string());

  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  // 云空间容量检查
  check_user_storage_limit(&state.pg_pool, uid, doc_state.len() as i64).await?;

  let user = RealtimeUser {
    uid,
    device_id,
    connect_at: timestamp(),
    session_id: Uuid::new_v4().to_string(),
    app_version,
  };

  let (tx, rx) = tokio::sync::oneshot::channel();
  let message = ClientHttpUpdateMessage {
    user,
    workspace_id,
    object_id,
    collab_type,
    update: Bytes::from(doc_state),
    state_vector: Some(Bytes::from(sv)),
    return_tx: Some(tx),
  };

  server
    .try_send(message)
    .map_err(|err| AppError::Internal(anyhow!("Failed to send message to server: {}", err)))?;

  match rx
    .await
    .map_err(|err| AppError::Internal(anyhow!("Failed to receive message from server: {}", err)))?
  {
    Ok(Some(data)) => {
      let encoded = tokio::task::spawn_blocking(move || zstd::encode_all(Cursor::new(data), 3))
        .await
        .map_err(|err| AppError::Internal(anyhow!("Failed to compress data: {}", err)))??;

      Ok(HttpResponse::Ok().body(encoded))
    },
    Ok(None) => Ok(HttpResponse::InternalServerError().finish()),
    Err(err) => Ok(err.error_response()),
  }
}

async fn post_quick_note_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  data: Json<CreateQuickNoteParams>,
) -> Result<JsonAppResponse<QuickNote>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Member)
    .await?;
  let data = data.into_inner();
  let quick_note = create_quick_note(&state.pg_pool, uid, workspace_id, data.data.as_ref()).await?;
  Ok(Json(AppResponse::Ok().with_data(quick_note)))
}

async fn list_quick_notes_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  query: web::Query<ListQuickNotesQueryParams>,
) -> Result<JsonAppResponse<QuickNotes>> {
  let workspace_id = workspace_id.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Member)
    .await?;
  let ListQuickNotesQueryParams {
    search_term,
    offset,
    limit,
  } = query.into_inner();
  let quick_notes = list_quick_notes(
    &state.pg_pool,
    uid,
    workspace_id,
    search_term,
    offset,
    limit,
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(quick_notes)))
}

async fn update_quick_note_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  data: Json<UpdateQuickNoteParams>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, quick_note_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Member)
    .await?;
  update_quick_note(&state.pg_pool, quick_note_id, &data.data).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_quick_note_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, quick_note_id) = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Member)
    .await?;
  delete_quick_note(&state.pg_pool, quick_note_id).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn delete_workspace_invite_code_handler(
  user_uuid: UserUuid,
  path_param: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let workspace_id = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;
  delete_workspace_invite_code(&state.pg_pool, &workspace_id).await?;
  Ok(Json(AppResponse::Ok()))
}

async fn get_workspace_invite_code_handler(
  user_uuid: UserUuid,
  path_param: web::Path<Uuid>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<WorkspaceInviteToken>> {
  let workspace_id = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Member)
    .await?;
  let code = get_invite_code_for_workspace(&state.pg_pool, &workspace_id).await?;
  Ok(Json(
    AppResponse::Ok().with_data(WorkspaceInviteToken { code }),
  ))
}

async fn post_workspace_invite_code_handler(
  user_uuid: UserUuid,
  path_param: web::Path<Uuid>,
  state: Data<AppState>,
  data: Json<WorkspaceInviteCodeParams>,
) -> Result<JsonAppResponse<WorkspaceInviteToken>> {
  let workspace_id = path_param.into_inner();
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  state
    .workspace_access_control
    .enforce_role_strong(&uid, &workspace_id, AFRole::Owner)
    .await?;
  let workspace_invite_link =
    generate_workspace_invite_token(&state.pg_pool, &workspace_id, data.validity_period_hours)
      .await?;
  Ok(Json(AppResponse::Ok().with_data(workspace_invite_link)))
}
/// 添加协作成员到笔记
///
/// 业务逻辑：
/// 创建邀请链接时调用
/// 在 af_collab_member_invite 表中创建一条记录，用于生成分享链接
/// 这样被邀请者点击链接后就可以直接加入协作了
#[tracing::instrument(skip_all, err)]
async fn create_share_link_invite_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
  params: Json<AddCollabMemberParams>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, view_id) = path_param.into_inner();
  let params = params.into_inner();
  
  tracing::info!(
    "create_share_link_invite: workspace_id={}, view_id={}, permission_id={}",
    workspace_id,
    view_id,
    params.permission_id,
  );

  // 获取当前用户ID
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  tracing::info!("current user uid: {}", uid);

  // 获取视图名称
  let folder = state.ws_server.get_folder(workspace_id).await?;
  let view_name = folder
    .get_view(&view_id.to_string(), uid)
    .map(|v| v.name.clone())
    .unwrap_or_else(|| {
      format!("共享文档 {}", &view_id.to_string()[..8])
    });

  // 检查是否已存在邀请记录
  let existing = sqlx::query!(
    r#"
      SELECT * FROM af_collab_member_invite
      WHERE oid = $1 AND send_uid = $2 AND received_uid IS NULL
    "#,
    view_id.to_string(),
    uid,
  )
  .fetch_optional(&state.pg_pool)
  .await
  .map_err(AppError::from)?;

  if existing.is_some() {
    // 更新现有记录的权限
    sqlx::query!(
      r#"
        UPDATE af_collab_member_invite
        SET permission_id = $1
        WHERE oid = $2 AND send_uid = $3 AND received_uid IS NULL
      "#,
      params.permission_id,
      view_id.to_string(),
      uid,
    )
    .execute(&state.pg_pool)
    .await
    .map_err(AppError::from)?;
    
    tracing::info!("updated existing invite record");
  } else {
    // 创建新的邀请记录（received_uid 为 NULL 表示待接受）
    sqlx::query!(
      r#"
        INSERT INTO af_collab_member_invite (oid, send_uid, name, permission_id)
        VALUES ($1, $2, $3, $4)
      "#,
      view_id.to_string(),
      uid,
      view_name,
      params.permission_id,
    )
    .execute(&state.pg_pool)
    .await
    .map_err(AppError::from)?;
    
    tracing::info!("created new invite record");
  }

  Ok(Json(AppResponse::Ok()))
}

/// 1. 将被邀请者添加到工作区（这样协作者才能同步文档）
/// 2. 将被邀请者添加到文档协作成员列表
/// 3. 设置权限（默认只读，可通过permission_id参数指定）
///
#[tracing::instrument(skip_all, err)]
async fn add_collab_member_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid, Uuid)>,
  state: Data<AppState>,
  params: Json<AddCollabMemberParams>,
) -> Result<JsonAppResponse<()>> {
  // 检查是否是自己的笔记
  // 不能是添加自己
  // 用户必须存在
  // 添加到 zf_collab_member
  // 添加到分享表
  let (workspace_id, view_id, received_uid) = path_param.into_inner();
  let params = params.into_inner();

  tracing::info!(
    "add_collab_member request: workspace_id={}, view_id={}, received_uid={}, permission_id={}",
    workspace_id,
    view_id,
    received_uid,
    params.permission_id
  );

  // 找到这个笔记的拥有者
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  tracing::info!("current user uid: {}", uid);

  let received_uid = state.user_cache.get_user_uid(&received_uid).await?;
  tracing::info!("received user uid: {}", received_uid);

  // 如果是自己点击自己的分享链接（接收者点击链接访问自己的文档）
  // 需要向 af_collab_member_invite 表中插入一条新记录，记录接收者信息
  // 而不是更新现有的邀请记录（received_uid = NULL 的记录是邀请模板）
  if uid == received_uid {
    tracing::info!("user {} is accessing their own share link, will insert new invite record for this receiver", uid);
    
    // 查询 af_collab_member_invite 表中是否有待接受的邀请记录
    let existing_invite = sqlx::query!(
      r#"
        SELECT send_uid, permission_id, name 
        FROM af_collab_member_invite
        WHERE oid = $1 AND received_uid IS NULL
      "#,
      view_id.to_string(),
    )
    .fetch_optional(&state.pg_pool)
    .await
    .map_err(AppError::from)?;
    
    if let Some(invite) = existing_invite {
      // 新插入一条记录，记录接收者信息
      // 注意：不是更新现有的邀请记录（邀请模板），而是新插入一条记录
      // 这样每个接收者都有一条独立的记录
      sqlx::query!(
        r#"
          INSERT INTO af_collab_member_invite (oid, send_uid, received_uid, name, permission_id)
          VALUES ($1, $2, $3, $4, $5)
        "#,
        view_id.to_string(),
        invite.send_uid,
        received_uid,
        invite.name,
        invite.permission_id,
      )
      .execute(&state.pg_pool)
      .await
      .map_err(AppError::from)?;
      
      tracing::info!("inserted new invite record for receiver: oid={}, send_uid={}, received_uid={}, permission_id={}", 
        view_id, invite.send_uid, received_uid, invite.permission_id);
      
      // 检查 af_collab_member 表中是否已有记录，如果没有则添加
      let existing_member = sqlx::query!(
        r#"
          SELECT uid FROM af_collab_member
          WHERE oid = $1 AND uid = $2
        "#,
        view_id.to_string(),
        received_uid,
      )
      .fetch_optional(&state.pg_pool)
      .await
      .map_err(AppError::from)?;
      
      if existing_member.is_none() {
        // 添加到 af_collab_member 表
        sqlx::query!(
          r#"
            INSERT INTO af_collab_member (uid, oid, permission_id)
            VALUES ($1, $2, $3)
          "#,
          received_uid,
          view_id.to_string(),
          invite.permission_id,
        )
        .execute(&state.pg_pool)
        .await
        .map_err(AppError::from)?;
        
        tracing::info!("added collab member: oid={}, uid={}, permission_id={}", 
          view_id, received_uid, invite.permission_id);
      }
      
      return Ok(Json(AppResponse::Ok()));
    }
    
    tracing::info!("no pending invite found, skipping");
    return Ok(Json(AppResponse::Ok()));
  }

  // 获取视图名称
  let folder = state.ws_server.get_folder(workspace_id).await?;
  tracing::info!("got folder for workspace: {}", workspace_id);

  let view_name = folder
    .get_view(&view_id.to_string(), uid)
    .map(|v| {
      tracing::info!("found view: {}", v.name.clone());
      v.name.clone()
    })
    .unwrap_or_else(|| {
      tracing::warn!("view not found in folder: {}", view_id);
      format!("共享文档 {}", &view_id.to_string()[..8])
    });

  // 注意：不再将用户添加到工作区级别
  // 这样被邀请者只能访问被分享的单个文档，而不是整个工作区
  // 文档协作通过 af_collab_member 表来控制权限

  // Step 1: 将被邀请者添加到文档协作成员列表
  // 使用传入的 permission_id 参数
  tracing::info!("adding collab member: workspace_id={}, view_id={}, received_uid={}, permission_id={}", workspace_id, view_id, received_uid, params.permission_id);
  add_collab_member(
    &state.pg_pool,
    state.collab_access_control.clone(),
    &workspace_id,
    &view_id,
    uid,
    received_uid,
    &view_name,
    params.permission_id, // 传递权限参数
  )
  .await?;
  tracing::info!("add_collab_member success!");

  // 权限已在 add_collab_member 内部处理，这里不再重复更新

  Ok(Json(AppResponse::Ok()))
}

/// 更新协作成员权限，这个需要检查权限的。暂定为，只能笔记拥有者有修改权
async fn update_collab_member_permission_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid, Uuid)>,
  state: Data<AppState>,
  data: Json<EditCollabMemberParams>,
) -> Result<JsonAppResponse<()>> {
  // 检查笔记是否是自己的
  // 用户必须存在
  // 不能是变更自己的权限
  let data = data.into_inner();
  let (workspace_id, view_id, opt_uid) = path_param.into_inner();
  let edit_uid = state.user_cache.get_user_uid(&opt_uid).await?;
  let user_uid = state.user_cache.get_user_uid(&user_uuid).await?;
  edit_collab_member_permission(
    &state.pg_pool,
    state.collab_access_control.clone(),
    &workspace_id,
    &view_id,
    user_uid,
    edit_uid,
    data.permission_id,
  )
  .await?;
  // 变更权限
  Ok(Json(AppResponse::Ok()))
}

/// 删除协作成员
///
/// 业务逻辑：
/// 1. 检查操作者是否是文档拥有者
/// 2. 不能删除自己
/// 3. 从 af_collab_member 表中删除成员
/// 4. 删除 Casbin 访问控制策略
async fn remove_collab_member_handler(
  user_uuid: UserUuid,
  path_param: web::Path<(Uuid, Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, view_id, remove_uid_uuid) = path_param.into_inner();

  // 1. 获取当前用户和要删除的用户的 uid
  let user_uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let remove_uid = state.user_cache.get_user_uid(&remove_uid_uuid).await?;

  // 2. 获取文档拥有者
  let collab_owner = workspace::ops::get_collab_owner(&state.pg_pool, &workspace_id, &view_id)
    .await
    .map_err(|e| AppResponseError::new(ErrorCode::Internal, e.to_string()))?;

  // 3. 只有文档拥有者可以删除成员
  if user_uid != collab_owner.uid {
    return Err(AppResponseError::new(
      ErrorCode::NotEnoughPermissions,
      "只有文档拥有者可以删除成员".to_string(),
    ).into());
  }

  // 4. 不能删除自己
  if user_uid == remove_uid {
    return Err(AppResponseError::new(
      ErrorCode::InvalidRequest,
      "不能删除自己".to_string(),
    ).into());
  }

  // 5. 获取发送者的 uid（文档拥有者）
  let send_uid = collab_owner.uid;

  // 6. 删除协作成员和邀请记录
  workspace::collab_member::remove_collab_member(
    &state.pg_pool,
    state.collab_access_control.clone(),
    &workspace_id,
    &view_id,
    user_uid, // 操作者 uid
    remove_uid,
    &view_id.to_string(),
  )
  .await?;

  // 7. 同时删除邀请记录
  let _ = workspace::collab_member::remove_collab_member_invite(
    &state.pg_pool,
    send_uid,
    remove_uid,
    &view_id.to_string(),
  )
  .await;

  Ok(Json(AppResponse::Ok()))
}

/// 我分析给别人的笔记
async fn list_sent_collab_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<Vec<AFCollabMemberInvite>>> {
  // 查询分享表实现  过滤 shared_by = user_uuid（由我发起的）
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let list = get_send_collab_list(&state.pg_pool, uid).await?;
  Ok(Json(AppResponse::Ok().with_data(list)))
}

/// 别人分享给我的笔记
async fn list_received_collab_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<Vec<AFCollabMemberInvite>>> {
  // 查询分享表实现  过滤 shared_to = user_uuid（接收者是我）
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let list = get_received_collab_list(&state.pg_pool, uid).await?;
  Ok(Json(AppResponse::Ok().with_data(list)))
}

#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
pub struct CreateJoinRequestPayload {
  pub requester_id: i64,
  pub reason: Option<String>,
}

#[derive(Debug, Clone, Validate, Serialize, Deserialize)]
pub struct HandleJoinRequestPayload {
  pub approve: bool,
}

/// Create a join request for a space
async fn post_join_request_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  payload: Json<CreateJoinRequestPayload>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<JoinRequest>> {
  let (workspace_id, space_id) = path.into_inner();
  let join_request = create_join_request(
    &state,
    &user_uuid,
    &workspace_id,
    &space_id,
    payload.reason.clone(),
  )
  .await?;
  Ok(Json(AppResponse::Ok().with_data(join_request)))
}

/// List join requests for a space (space owner only)
async fn get_join_requests_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<Vec<JoinRequest>>> {
  let (workspace_id, space_id) = path.into_inner();
  let requests = list_join_requests(&state, &user_uuid, &workspace_id, &space_id).await?;
  Ok(Json(AppResponse::Ok().with_data(requests)))
}

/// Handle join request (approve/reject) - space owner only
async fn handle_join_request_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid, Uuid)>,
  payload: Json<HandleJoinRequestPayload>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, space_id, request_id) = path.into_inner();
  handle_join_request(
    &state,
    &user_uuid,
    &workspace_id,
    &space_id,
    &request_id,
    payload.approve,
  )
  .await?;
  Ok(Json(AppResponse::Ok()))
}

/// Cancel join request - requester only
async fn cancel_join_request_handler(
  user_uuid: UserUuid,
  path: web::Path<(Uuid, Uuid)>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let (workspace_id, space_id) = path.into_inner();
  cancel_join_request(&state, &user_uuid, &workspace_id, &space_id).await?;
  Ok(Json(AppResponse::Ok()))
}
