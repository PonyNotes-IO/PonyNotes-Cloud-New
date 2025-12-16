use crate::api::util::client_version_from_headers;
use crate::biz::authentication::jwt::{Authorization, UserUuid};
use crate::biz::user::image_asset::{get_user_image_asset, upload_user_image_asset};
use crate::biz::user::user_delete::delete_user;
use crate::biz::user::user_info::{get_profile, get_user_workspace_info, update_user};
use crate::biz::user::user_search::{get_uid_by_email_or_phone, search_users_by_email};
use crate::biz::user::user_verify::{send_phone_otp, verify_and_bind_phone, verify_token};
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
  DeleteUserQuery, GetUidByEmailOrPhoneQuery, GetUidByEmailOrPhoneResponse, SearchUserQuery,
  SearchUserResponse, SendPhoneOtpParams, SignInTokenResponse, UpdateUserParams,
  VerifyAndBindPhoneParams,
};
use shared_entity::response::AppResponseError;
use shared_entity::response::{AppResponse, JsonAppResponse};
use tracing::event;
use uuid::Uuid;

pub fn user_scope() -> Scope {
  web::scope("/api/user")
    .service(web::resource("/verify/{access_token}").route(web::get().to(verify_user_handler)))
    .service(web::resource("/update").route(web::post().to(update_user_handler)))
    .service(web::resource("/send-phone-otp").route(web::post().to(send_phone_otp_handler)))
    .service(web::resource("/verify-phone").route(web::post().to(verify_and_bind_phone_handler)))
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
) -> Result<JsonAppResponse<()>> {
  let user_uuid = auth.uuid()?;
  let params = payload.into_inner();

  verify_and_bind_phone(&user_uuid, &params.phone, &params.otp, state.as_ref()).await?;

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
  let file_id =
    upload_user_image_asset(state.bucket_client.clone(), &form.asset, &user_uuid).await?;

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

  // This endpoint is for logged-in users to bind or change their phone number
  // Note: Phone number registration (signup) goes through GoTrue's /otp or /signup endpoints,
  // which don't require authentication and don't go through this handler.
  //
  // For logged-in users:
  // - SSO users (WeChat/DouYin) already have a temporary phone number created during login
  // - So binding a real phone number is actually a "phone change" scenario
  // - Even if a logged-in user somehow has no phone (shouldn't happen due to DB constraints),
  //   we still use the standard flow (send_phone_otp) since the user is already authenticated
  let user_info = state
    .gotrue_client
    .user_info(&auth.token)
    .await
    .map_err(AppResponseError::from)?;
  
  let current_phone = user_info.phone;
  
  // Always use standard phone change flow for logged-in users
  // This works for:
  // 1. SSO users with temporary phone (e.g., +86temp...) - will set phone_change state
  // 2. Users with real phone - will set phone_change state for phone change
  // 3. Users with no phone (unexpected but handled) - will set phone_change state
  event!(
    tracing::Level::INFO,
    "Logged-in user {} (current phone: {}) binding/changing to phone {} - using standard phone change flow",
    user_uuid,
    if current_phone.is_empty() { "(none)" } else { &current_phone },
    params.phone
  );
  send_phone_otp(&auth.token, &params.phone, state.as_ref()).await?;
  
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

#[tracing::instrument(skip(state, auth, query), err)]
async fn get_uid_by_email_or_phone_handler(
  auth: Authorization,
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
