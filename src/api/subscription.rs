use actix_web::{
  web::{self, Data, Json, Query},
  Result, Scope,
};
use serde::Deserialize;

use crate::biz::authentication::jwt::UserUuid;
use crate::biz::subscription::ops::{
  cancel_subscription, fetch_current_subscription, fetch_usage, fetch_user_addons,
  fetch_subscription_plans, list_addons, purchase_addon, record_usage, subscribe_plan,
};
use crate::state::AppState;
use shared_entity::dto::subscription_dto::{
  AddonStatus, AddonType, CancelSubscriptionRequest, PurchaseAddonRequest, SubscribeRequest,
  SubscriptionAddonInfo, SubscriptionCurrentResponse, SubscriptionPlanInfo, SubscriptionUsageQuery,
  SubscriptionUsageResponse, UsageRecordRequest, UserAddonRecord, UserSubscriptionRecord,
};
use shared_entity::response::{AppResponse, JsonAppResponse};

pub fn subscription_scope() -> Scope {
  web::scope("/api/subscription")
    .service(web::resource("/plans").route(web::get().to(get_subscription_plans_handler)))
    .service(web::resource("/current").route(web::get().to(get_current_subscription_handler)))
    .service(web::resource("/subscribe").route(web::post().to(post_subscribe_handler)))
    .service(web::resource("/cancel").route(web::post().to(post_cancel_handler)))
    .service(web::resource("/addons").route(web::get().to(get_addons_handler)))
    .service(
      web::resource("/addons/purchase").route(web::post().to(post_purchase_addon_handler)),
    )
    .service(web::resource("/addons/my").route(web::get().to(get_user_addons_handler)))
    .service(web::resource("/usage").route(web::get().to(get_usage_handler)))
    .service(web::resource("/usage/record").route(web::post().to(post_usage_record_handler)))
}

async fn get_subscription_plans_handler(
  state: Data<AppState>,
) -> Result<JsonAppResponse<Vec<SubscriptionPlanInfo>>> {
  let plans = fetch_subscription_plans(&state.pg_pool).await?;
  Ok(Json(AppResponse::Ok().with_data(plans)))
}

async fn get_current_subscription_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<SubscriptionCurrentResponse>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let subscription = fetch_current_subscription(&state.pg_pool, uid).await?;
  Ok(Json(AppResponse::Ok().with_data(subscription)))
}

async fn post_subscribe_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<SubscribeRequest>,
) -> Result<JsonAppResponse<SubscriptionCurrentResponse>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let response = subscribe_plan(&state.pg_pool, uid, payload.into_inner()).await?;
  Ok(Json(AppResponse::Ok().with_data(response)))
}

async fn post_cancel_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<CancelSubscriptionRequest>,
) -> Result<JsonAppResponse<UserSubscriptionRecord>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let record = cancel_subscription(&state.pg_pool, uid, payload.into_inner()).await?;
  Ok(Json(AppResponse::Ok().with_data(record)))
}

#[derive(Debug, Deserialize)]
struct AddonListQuery {
  #[serde(default)]
  addon_type: Option<AddonType>,
}

async fn get_addons_handler(
  state: Data<AppState>,
  query: Query<AddonListQuery>,
) -> Result<JsonAppResponse<Vec<SubscriptionAddonInfo>>> {
  let addons = list_addons(&state.pg_pool, query.addon_type.clone()).await?;
  Ok(Json(AppResponse::Ok().with_data(addons)))
}

async fn post_purchase_addon_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<PurchaseAddonRequest>,
) -> Result<JsonAppResponse<UserAddonRecord>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let addon = purchase_addon(&state.pg_pool, uid, payload.into_inner()).await?;
  Ok(Json(AppResponse::Ok().with_data(addon)))
}

#[derive(Debug, Deserialize)]
struct UserAddonQuery {
  #[serde(default)]
  status: Option<AddonStatus>,
}

async fn get_user_addons_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  query: Query<UserAddonQuery>,
) -> Result<JsonAppResponse<Vec<UserAddonRecord>>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let addons = fetch_user_addons(&state.pg_pool, uid, query.status.clone()).await?;
  Ok(Json(AppResponse::Ok().with_data(addons)))
}

async fn get_usage_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  query: Query<SubscriptionUsageQuery>,
) -> Result<JsonAppResponse<SubscriptionUsageResponse>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  let usage = fetch_usage(&state.pg_pool, uid, query.into_inner()).await?;
  Ok(Json(AppResponse::Ok().with_data(usage)))
}

async fn post_usage_record_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<UsageRecordRequest>,
) -> Result<JsonAppResponse<()>> {
  let uid = state.user_cache.get_user_uid(&user_uuid).await?;
  record_usage(&state.pg_pool, uid, payload.into_inner()).await?;
  Ok(Json(AppResponse::Ok()))
}

