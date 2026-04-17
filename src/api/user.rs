use crate::api::util::client_version_from_headers;
use crate::biz::authentication::jwt::{Authorization, UserUuid};
use crate::biz::user::image_asset::{get_user_image_asset, upload_user_image_asset};
use crate::biz::user::user_delete::delete_user;
use crate::biz::user::user_info::{get_profile, get_user_workspace_info, update_user};
use crate::biz::user::user_search::{get_uid_by_email_or_phone, search_users_by_email};
use crate::biz::user::user_verify::{
  check_email_registered, send_email_change_otp, send_phone_otp, send_phone_reauth_otp,
  verify_and_bind_email, verify_and_bind_phone, verify_phone_reauthentication, verify_token,
};
use crate::biz::subscription::ops::check_user_storage_limit;
use crate::state::AppState;
use actix_http::StatusCode;
use actix_multipart::form::bytes::Bytes;
use actix_multipart::form::MultipartForm;
use actix_web::web::{Data, Json};
use actix_web::{web, HttpResponse, Scope};
use actix_web::{HttpRequest, Result};
use app_error::AppError;
use database_entity::dto::{AFUserProfile, AFUserWorkspaceInfo, UserImageAssetSource};
use semver::Version;
use shared_entity::dto::auth_dto::{
  BindPhoneResponse, CheckEmailParams, DeleteUserQuery, GetUidByEmailOrPhoneQuery,
  GetUidByEmailOrPhoneResponse, SearchUserQuery, SearchUserResponse, SendEmailChangeOtpParams,
  SendPhoneOtpParams, SignInTokenResponse, UpdateUserParams, VerifyAndBindEmailParams,
  VerifyAndBindPhoneParams,
};
use shared_entity::response::AppResponseError;
use shared_entity::response::{AppResponse, JsonAppResponse};
use tracing::event;
use uuid::Uuid;
use serde_json::json;

pub fn user_scope() -> Scope {
  web::scope("/api/user")
    .service(web::resource("/verify/{access_token}").route(web::get().to(verify_user_handler)))
    .service(web::resource("/update").route(web::post().to(update_user_handler)))
    .service(web::resource("/send-phone-otp").route(web::post().to(send_phone_otp_handler)))
    .service(web::resource("/send-phone-reauth-otp").route(web::post().to(send_phone_reauth_otp_handler)))
    .service(web::resource("/send-email-change-otp").route(web::post().to(send_email_change_otp_handler)))
    .service(web::resource("/verify-phone").route(web::post().to(verify_and_bind_phone_handler)))
    .service(web::resource("/verify-phone-reauthentication").route(web::post().to(verify_phone_reauthentication_handler)))
    .service(web::resource("/verify-email").route(web::post().to(verify_and_bind_email_handler)))
    .service(web::resource("/check-email-registered").route(web::post().to(check_email_registered_handler)))
    .service(web::resource("/profile").route(web::get().to(get_user_profile_handler)))
    .service(web::resource("/workspace").route(web::get().to(get_user_workspace_info_handler)))
    .service(web::resource("/asset/image").route(web::post().to(post_user_image_asset_handler)))
    .service(
      web::resource("/asset/image/person/{person_id}/file/{file_id}")
        .route(web::get().to(get_user_image_asset_handler)),
    )
    .service(web::resource("/search").route(web::get().to(search_user_handler)))
    .service(web::resource("/get-uid").route(web::get().to(get_uid_by_email_or_phone_handler)))
    .service(web::resource("").route(web::delete().to(delete_user_handler)))
    .service(
      web::resource("/notification-preferences")
        .route(web::get().to(get_notification_preferences_handler))
        .route(web::post().to(post_notification_preferences_handler)),
    )
    // 诊断接口：查询当前用户的所有通知（含已处理）
    .service(web::resource("/notifications").route(web::get().to(list_user_notifications_handler)))
}

#[tracing::instrument(skip(state, path), err)]
async fn verify_user_handler(
  path: web::Path<String>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<SignInTokenResponse>> {
  let access_token = path.into_inner();
  let is_new = verify_token(&access_token, state.as_ref())
    .await
    .map_err(AppResponseError::from)?;
  let resp = SignInTokenResponse { is_new };
  Ok(AppResponse::Ok().with_data(resp).into())
}

#[tracing::instrument(skip(state), err)]
async fn get_user_profile_handler(
  uuid: UserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<AFUserProfile>> {
  let profile = get_profile(&state.pg_pool, &uuid)
    .await
    .map_err(AppResponseError::from)?;
  Ok(AppResponse::Ok().with_data(profile).into())
}

#[tracing::instrument(skip(state), err)]
async fn get_user_workspace_info_handler(
  uuid: UserUuid,
  state: Data<AppState>,
  req: HttpRequest,
) -> Result<JsonAppResponse<AFUserWorkspaceInfo>> {
  let app_version = client_version_from_headers(req.headers())
    .ok()
    .and_then(|s| Version::parse(s).ok());
  let exclude_guest = app_version
    .map(|s| s < Version::new(0, 9, 4))
    .unwrap_or(true);

  let info = get_user_workspace_info(&state.pg_pool, &uuid, exclude_guest).await?;
  Ok(AppResponse::Ok().with_data(info).into())
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn update_user_handler(
  auth: Authorization,
  payload: Json<UpdateUserParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let params = payload.into_inner();
  update_user(&state.pg_pool, auth.uuid()?, params).await?;
  Ok(AppResponse::Ok().into())
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn verify_and_bind_phone_handler(
  auth: Authorization,
  payload: Json<VerifyAndBindPhoneParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<BindPhoneResponse>> {
  let user_uuid = auth.uuid()?;
  let params = payload.into_inner();

  let result = verify_and_bind_phone(&user_uuid, &params.phone, &params.otp, state.as_ref()).await?;

  Ok(AppResponse::Ok().with_data(result).into())
}

/// Verify phone OTP for identity reauthentication (does NOT bind or update phone).
/// Uses type=sms so GoTrue only verifies the OTP without modifying any user data.
#[derive(Debug, serde::Deserialize)]
struct VerifyPhoneReauthenticationParams {
  pub phone: String,
  pub otp: String,
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn verify_phone_reauthentication_handler(
  auth: Authorization,
  payload: Json<VerifyPhoneReauthenticationParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let user_uuid = auth.uuid()?;
  let params = payload.into_inner();

  verify_phone_reauthentication(&user_uuid, &params.phone, &params.otp, state.as_ref()).await?;

  Ok(AppResponse::Ok().into())
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn verify_and_bind_email_handler(
  auth: Authorization,
  payload: Json<VerifyAndBindEmailParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let user_uuid = auth.uuid()?;
  let params = payload.into_inner();

  verify_and_bind_email(&user_uuid, &params.email, &params.otp, state.as_ref()).await?;

  Ok(AppResponse::Ok().into())
}

#[tracing::instrument(skip(state), err)]
async fn delete_user_handler(
  auth: Authorization,
  state: Data<AppState>,
  query: web::Query<DeleteUserQuery>,
) -> Result<JsonAppResponse<()>, actix_web::Error> {
  let user_uuid = auth.uuid()?;
  let DeleteUserQuery {
    provider_access_token,
    provider_refresh_token,
  } = query.into_inner();
  delete_user(
    &state.pg_pool,
    &state.redis_connection_manager,
    &state.bucket_storage,
    &state.gotrue_client,
    &state.gotrue_admin,
    &state.config.apple_oauth,
    auth,
    user_uuid,
    provider_access_token,
    provider_refresh_token,
  )
  .await?;
  Ok(AppResponse::Ok().into())
}

#[derive(MultipartForm)]
#[multipart(duplicate_field = "deny")]
struct UploadUserImageAssetForm {
  #[multipart(limit = "1MB")]
  asset: Bytes,
}

async fn get_user_image_asset_handler(
  _user_uuid: UserUuid,
  path: web::Path<(Uuid, String)>,
  state: Data<AppState>,
) -> Result<HttpResponse> {
  let (person_id, file_id) = path.into_inner();
  let avatar = get_user_image_asset(state.bucket_client.clone(), &person_id, file_id).await?;
  Ok(
    HttpResponse::build(StatusCode::OK)
      .content_type(avatar.content_type)
      .body(avatar.data),
  )
}

async fn post_user_image_asset_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  MultipartForm(form): MultipartForm<UploadUserImageAssetForm>,
) -> Result<JsonAppResponse<UserImageAssetSource>> {
  // 云空间容量限制
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  check_user_storage_limit(&state.pg_pool, uid, form.asset.data.len() as i64).await?;

  let file_id =
    upload_user_image_asset(state.bucket_client.clone(), &form.asset, &user_uuid).await?;
  let avatar_file_size = form.asset.data.len() as i64;
  database::user::update_user(
    &state.pg_pool,
    &user_uuid,
    None, None, None,
    Some(json!({"avatar_file_size": avatar_file_size})),
  ).await?;
  Ok(
    AppResponse::Ok()
      .with_data(UserImageAssetSource { file_id })
      .into(),
  )
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn send_phone_otp_handler(
  auth: Authorization,
  payload: Json<SendPhoneOtpParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let params = payload.into_inner();
  let user_uuid = auth.uuid()?;

  event!(
    tracing::Level::INFO,
    "Logged-in user {} binding/changing to phone {}",
    user_uuid,
    params.phone
  );
  send_phone_otp(&auth.token, &user_uuid, &params.phone, state.as_ref()).await?;

  Ok(AppResponse::Ok().into())
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn send_phone_reauth_otp_handler(
  auth: Authorization,
  payload: Json<SendPhoneOtpParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let params = payload.into_inner();
  let _user_uuid = auth.uuid()?;

  event!(
    tracing::Level::INFO,
    "User {} requesting phone reauth OTP for: {}",
    _user_uuid,
    params.phone
  );
  send_phone_reauth_otp(&auth.token, &params.phone, state.as_ref()).await?;

  Ok(AppResponse::Ok().into())
}

/// 发送邮箱变更 OTP（用于绑定新邮箱）
/// 调用 GoTrue PUT /user 接口，GoTrue 将 email_change 存入当前用户行，
/// 并向新邮箱发送 OTP，验证时使用 type=email_change（而非 magiclink）
#[tracing::instrument(skip(state, auth, payload), err)]
async fn send_email_change_otp_handler(
  auth: Authorization,
  payload: Json<SendEmailChangeOtpParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let params = payload.into_inner();
  let _user_uuid = auth.uuid()?;

  event!(
    tracing::Level::INFO,
    "User {} requesting email change OTP for: {}",
    _user_uuid,
    params.email
  );
  send_email_change_otp(&auth.token, &params.email, state.as_ref()).await?;

  Ok(AppResponse::Ok().into())
}

#[derive(Debug, serde::Serialize)]
struct CheckEmailRegisteredResponse {
  email_exists: bool,
  is_own_email: bool,
  existing_uid: Option<i64>,
  message: Option<String>,
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn check_email_registered_handler(
  auth: Authorization,
  payload: Json<CheckEmailParams>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<CheckEmailRegisteredResponse>> {
  let params = payload.into_inner();
  let user_uuid = auth.uuid()?;

  let result = check_email_registered(&user_uuid, &params.email, state.as_ref()).await?;

  Ok(AppResponse::Ok()
    .with_data(CheckEmailRegisteredResponse {
      email_exists: result.email_exists,
      is_own_email: result.is_own_email,
      existing_uid: result.existing_uid,
      message: result.message,
    })
    .into())
}

#[tracing::instrument(skip(state, auth), err)]
async fn get_notification_preferences_handler(
  auth: Authorization,
  state: Data<AppState>,
) -> Result<JsonAppResponse<serde_json::Value>> {
  let user_uuid = auth.uuid()?;

  // read user profile to get metadata
  let profile_opt = database::workspace::select_user_profile(&state.pg_pool, &user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let notification_settings = profile_opt
    .as_ref()
    .and_then(|p| p.metadata.as_ref())
    .and_then(|m| m.get("notification_settings").cloned())
    .unwrap_or_else(|| json!({
      "notify_at_me": true,
      "notify_pending": true,
      "notify_permission_change": true,
      "notify_join_team": true,
      "notify_clip": true
    }));

  Ok(AppResponse::Ok().with_data(notification_settings).into())
}

#[derive(serde::Deserialize)]
struct NotificationPreferencesPayload {
  notify_at_me: Option<bool>,
  notify_pending: Option<bool>,
  notify_permission_change: Option<bool>,
  notify_join_team: Option<bool>,
  notify_clip: Option<bool>,
}

#[tracing::instrument(skip(state, auth, payload), err)]
async fn post_notification_preferences_handler(
  auth: Authorization,
  payload: Json<NotificationPreferencesPayload>,
  state: Data<AppState>,
) -> Result<JsonAppResponse<()>> {
  let user_uuid = auth.uuid()?;
  let prefs = payload.into_inner();

  let mut obj = serde_json::Map::new();
  if let Some(v) = prefs.notify_at_me { obj.insert("notify_at_me".to_string(), serde_json::json!(v)); }
  if let Some(v) = prefs.notify_pending { obj.insert("notify_pending".to_string(), serde_json::json!(v)); }
  if let Some(v) = prefs.notify_permission_change { obj.insert("notify_permission_change".to_string(), serde_json::json!(v)); }
  if let Some(v) = prefs.notify_join_team { obj.insert("notify_join_team".to_string(), serde_json::json!(v)); }
  if let Some(v) = prefs.notify_clip { obj.insert("notify_clip".to_string(), serde_json::json!(v)); }

  let metadata = json!({ "notification_settings": serde_json::Value::Object(obj) });

  // merge into user metadata
  database::user::update_user(&state.pg_pool, &user_uuid, None, None, None, Some(metadata.into()))
    .await
    .map_err(AppResponseError::from)?;

  Ok(AppResponse::Ok().into())
}

async fn search_user_handler(
  state: Data<AppState>,
  query: web::Query<SearchUserQuery>,
) -> Result<JsonAppResponse<Vec<SearchUserResponse>>, AppError> {
  let SearchUserQuery { q, page_no } = query.into_inner();
  match q {
    None => Err(AppError::MissingPayload("q is required.".to_string()).into()),
    Some(q) => {
      let af_users = search_users_by_email(&state.pg_pool, &q, page_no.unwrap_or(1)).await?;
      let users = af_users
        .into_iter()
        .map(|u| SearchUserResponse {
          uid: u.uid,
          uuid: u.uuid,
          email: u.email,
          phone: u.phone,
          name: u.name,
          metadata: u.metadata,
          deleted_at: u.deleted_at,
          updated_at: u.updated_at,
          created_at: u.created_at,
        })
        .collect();
      Ok(AppResponse::Ok().with_data(users).into())
    },
  }
}

#[tracing::instrument(skip(state, _auth, query), err)]
async fn get_uid_by_email_or_phone_handler(
  _auth: Authorization,
  state: Data<AppState>,
  query: web::Query<GetUidByEmailOrPhoneQuery>,
) -> Result<JsonAppResponse<GetUidByEmailOrPhoneResponse>, AppError> {
  let identifier = query.identifier.trim();

  if identifier.is_empty() {
    return Err(AppError::InvalidRequest(
      "identifier parameter is required".to_string(),
    )
    .into());
  }

  let (uid, identifier_type) =
    get_uid_by_email_or_phone(&state.pg_pool, identifier).await?;

  let response = GetUidByEmailOrPhoneResponse {
    uid,
    identifier_type,
  };

  Ok(AppResponse::Ok().with_data(response).into())
}

/// 诊断接口：查询当前用户在 af_notification 中的最近 50 条通知（含已处理）
/// GET /api/user/notifications
async fn list_user_notifications_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<HttpResponse> {
  let uid = state
    .user_cache
    .get_user_uid(&user_uuid)
    .await
    .map_err(AppResponseError::from)?;

  let rows = sqlx::query(
    r#"
    SELECT id, workspace_id, notification_type, payload, recipient_uid, created_at, processed
    FROM af_notification
    WHERE recipient_uid = $1
    ORDER BY created_at DESC
    LIMIT 50
    "#,
  )
  .bind(uid)
  .fetch_all(&state.pg_pool)
  .await
  .map_err(|e| AppResponseError::from(AppError::from(e)))?;

  let notifications: Vec<serde_json::Value> = rows
    .iter()
    .map(|row| {
      use sqlx::Row;
      json!({
        "id": row.get::<uuid::Uuid, _>("id").to_string(),
        "workspace_id": row.get::<Option<uuid::Uuid>, _>("workspace_id").map(|v| v.to_string()),
        "notification_type": row.get::<String, _>("notification_type"),
        "payload": row.get::<serde_json::Value, _>("payload"),
        "recipient_uid": row.get::<Option<i64>, _>("recipient_uid"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at").to_rfc3339(),
        "processed": row.get::<bool, _>("processed"),
      })
    })
    .collect();

  Ok(HttpResponse::Ok().json(json!({
    "uid": uid,
    "count": notifications.len(),
    "notifications": notifications,
  })))
}
