use std::cmp::Ordering;

use app_error::AppError;
use chrono::{Datelike, Months, NaiveDate, Utc};
use database::subscription::{
  aggregate_user_usage, calculate_addon_period_end, get_subscription_addon, get_subscription_plan,
  get_user_active_subscription, insert_user_addon, list_subscription_addons,
  list_user_addons, list_subscription_plans, upsert_usage_record, upsert_user_subscription,
  get_user_total_storage_usage, get_user_owned_workspace_count,
  SubscriptionAddonRow, SubscriptionPlanRow, UserAddonRow, UserSubscriptionRow,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use shared_entity::dto::subscription_dto::{
  AddonStatus, AddonType, BillingType, CancelSubscriptionRequest, PurchaseAddonRequest,
  SubscribeRequest, SubscriptionAddonInfo, SubscriptionAddonUsage, SubscriptionCurrentResponse,
  SubscriptionCurrentUsage, SubscriptionPlanInfo, SubscriptionStatus,
  SubscriptionUsageLimits, SubscriptionUsageMetrics, SubscriptionUsageQuery,
  SubscriptionUsageRemaining, SubscriptionUsageResponse, UsageRecordRequest, UsageType,
  UserAddonRecord, UserSubscriptionRecord,
};
use sqlx::PgPool;

const STORAGE_GB_IN_BYTES: f64 = 1024.0 * 1024.0 * 1024.0;
const STORAGE_MB_IN_BYTES: f64 = 1024.0 * 1024.0;

fn format_storage_bytes(bytes: i64) -> String {
  let mb = bytes as f64 / STORAGE_MB_IN_BYTES;
  if mb < 1024.0 {
    format!("{:.2} MB", mb)
  } else {
    format!("{:.2} GB", mb / 1024.0)
  }
}

fn format_storage_mb(mb: f64) -> String {
  if mb < 1024.0 {
    format!("{:.2} MB", mb)
  } else {
    format!("{:.2} GB", mb / 1024.0)
  }
}

pub async fn fetch_subscription_plans(pg_pool: &PgPool) -> Result<Vec<SubscriptionPlanInfo>, AppError> {
  let plans = list_subscription_plans(pg_pool).await?;
  Ok(plans.into_iter().map(to_plan_info).collect())
}

pub async fn fetch_current_subscription(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<SubscriptionCurrentResponse, AppError> {
  let subscription =
    get_user_active_subscription(pg_pool, uid).await?.ok_or_else(|| {
      AppError::RecordNotFound("subscription not found for current user".to_string())
    })?;
  build_current_subscription(pg_pool, uid, subscription).await
}

pub async fn subscribe_plan(
  pg_pool: &PgPool,
  uid: i64,
  request: SubscribeRequest,
) -> Result<SubscriptionCurrentResponse, AppError> {
  let plan = get_subscription_plan(pg_pool, request.plan_id).await?;
  if !plan.is_active {
    return Err(AppError::InvalidRequest("subscription plan is not active".into()));
  }

  let start_date = Utc::now();
  let end_date = add_months(start_date, request.billing_type.months())
    .ok_or_else(|| AppError::InvalidRequest("failed to calculate subscription end date".into()))?;

  let billing_type_str = request.billing_type.as_str();
  let subscription =
    upsert_user_subscription(pg_pool, uid, plan.id, billing_type_str, start_date, end_date).await?;
  build_current_subscription_with_plan(pg_pool, uid, subscription, plan).await
}

pub async fn cancel_subscription(
  pg_pool: &PgPool,
  uid: i64,
  payload: CancelSubscriptionRequest,
) -> Result<UserSubscriptionRecord, AppError> {
  let subscription = cancel_user_subscription_with_reason(pg_pool, uid, payload.reason).await?;
  let plan = get_subscription_plan(pg_pool, subscription.plan_id).await?;
  convert_subscription_row(subscription, &plan)?.into_record()
}

// 以下 addon 相关函数保留，但不再通过 API 暴露
pub async fn list_addons(
  pg_pool: &PgPool,
  addon_type: Option<AddonType>,
) -> Result<Vec<SubscriptionAddonInfo>, AppError> {
  let filter = addon_type.as_ref().map(|ty| ty.as_str());
  let addons = list_subscription_addons(pg_pool, filter).await?;
  Ok(addons.into_iter().map(to_addon_info).collect())
}

pub async fn purchase_addon(
  pg_pool: &PgPool,
  uid: i64,
  payload: PurchaseAddonRequest,
) -> Result<UserAddonRecord, AppError> {
  if payload.quantity <= 0 {
    return Err(AppError::InvalidRequest(
      "quantity must be greater than zero".into(),
    ));
  }

  let addon = get_subscription_addon(pg_pool, payload.addon_id).await?;
  if !addon.is_active {
    return Err(AppError::InvalidRequest("addon is not active".into()));
  }

  let start_date = Utc::now();
  let end_date = calculate_addon_period_end(start_date);
  let user_addon =
    insert_user_addon(pg_pool, uid, &addon, payload.quantity, start_date, end_date).await?;
  convert_user_addon(user_addon)
}

pub async fn fetch_user_addons(
  pg_pool: &PgPool,
  uid: i64,
  status: Option<AddonStatus>,
) -> Result<Vec<UserAddonRecord>, AppError> {
  let status_str = status.as_ref().map(|s| s.as_str());
  let rows = list_user_addons(pg_pool, uid, status_str).await?;
  rows.into_iter().map(convert_user_addon).collect()
}

pub async fn fetch_usage(
  pg_pool: &PgPool,
  uid: i64,
  query: SubscriptionUsageQuery,
) -> Result<SubscriptionUsageResponse, AppError> {
  let subscription =
    get_user_active_subscription(pg_pool, uid).await?.ok_or_else(|| {
      AppError::RecordNotFound("subscription not found for current user".to_string())
    })?;
  let plan = get_subscription_plan(pg_pool, subscription.plan_id).await?;
  build_usage_response(pg_pool, uid, subscription, plan, query).await
}

pub async fn record_usage(
  pg_pool: &PgPool,
  uid: i64,
  payload: UsageRecordRequest,
) -> Result<(), AppError> {
  let usage_date = payload
    .usage_date
    .unwrap_or_else(|| Utc::now().date_naive());
  let usage_type = usage_type_to_str(payload.usage_type);
  upsert_usage_record(pg_pool, uid, usage_type, usage_date, payload.usage_count).await?;
  Ok(())
}

async fn build_current_subscription(
  pg_pool: &PgPool,
  uid: i64,
  subscription: UserSubscriptionRow,
) -> Result<SubscriptionCurrentResponse, AppError> {
  let plan = get_subscription_plan(pg_pool, subscription.plan_id).await?;
  build_current_subscription_with_plan(pg_pool, uid, subscription, plan).await
}

async fn build_current_subscription_with_plan(
  pg_pool: &PgPool,
  uid: i64,
  subscription: UserSubscriptionRow,
  plan: SubscriptionPlanRow,
) -> Result<SubscriptionCurrentResponse, AppError> {
  let plan_info = to_plan_info(plan.clone());
  let limits = PlanLimitsContext::from(&plan);

  let (usage_start_date, usage_end_date) = (
    subscription.start_date.date_naive(),
    subscription.end_date.date_naive(),
  );
  
  // Real AI Usage
  let usage = aggregate_user_usage(pg_pool, uid, usage_start_date, usage_end_date).await?;
  let ai_chat_used = usage.iter().find(|u| u.usage_type == "ai_chat").map(|u| u.total).unwrap_or(0);
  let ai_image_used = usage.iter().find(|u| u.usage_type == "ai_image").map(|u| u.total).unwrap_or(0);
  
  // Real Storage Usage
  let storage_used_bytes = get_user_total_storage_usage(pg_pool, uid).await?;
  let storage_total_mb = plan.cloud_storage_gb.to_f64().unwrap_or(0.0);
  let storage_total_bytes = (storage_total_mb * STORAGE_MB_IN_BYTES) as i64;
  let storage_remaining_bytes = storage_total_bytes - storage_used_bytes;
  
  // Real Workspace Usage
  let workspace_used = get_user_owned_workspace_count(pg_pool, uid).await?;
  let workspace_total = plan.collaborative_workspace_limit as i64;

  let current_usage = SubscriptionCurrentUsage {
    ai_chat_used_this_month: ai_chat_used,
    ai_chat_remaining_this_month: limits.ai_chat_limit.map(|l| (l - ai_chat_used).max(0)),
    ai_image_used_this_month: ai_image_used,
    ai_image_remaining_this_month: limits.ai_image_limit.map(|l| (l - ai_image_used).max(0)),
    
    storage_used: format_storage_bytes(storage_used_bytes),
    storage_total: format_storage_mb(storage_total_mb),
    storage_remaining: format_storage_bytes(storage_remaining_bytes.max(0)),
    storage_used_gb: bytes_to_gb(storage_used_bytes),
    storage_total_gb: limits.storage_limit_gb,
    
    collaborative_workspace_used: workspace_used,
    collaborative_workspace_total: workspace_total,
    collaborative_workspace_remaining: (workspace_total - workspace_used).max(0),
  };

  Ok(SubscriptionCurrentResponse {
    subscription: convert_subscription_row(subscription, &plan)?.into_record()?,
    plan_details: plan_info,
    usage: current_usage,
  })
}

async fn build_usage_response(
  _pg_pool: &PgPool,
  _uid: i64,
  subscription: UserSubscriptionRow,
  plan: SubscriptionPlanRow,
  query: SubscriptionUsageQuery,
) -> Result<SubscriptionUsageResponse, AppError> {
  let plan_limits = PlanLimitsContext::from(&plan);

  let (start_date, end_date) = resolve_date_range(query);
  if start_date > end_date {
    return Err(AppError::InvalidRequest(
      "start_date must be before end_date".into(),
    ));
  }

  // 简化：返回空的 addon usage
  let addon_usage = SubscriptionAddonUsage {
    storage_addon_total_gb: 0.0,
    ai_token_addon_chat_count: 0,
    ai_token_addon_image_count: 0,
    ai_token_addon_chat_used: 0,
    ai_token_addon_image_used: 0,
  };

  // ensure subscription still valid? for now just return
  let _ = convert_subscription_row(subscription, &plan)?;

  Ok(SubscriptionUsageResponse {
    subscription_limits: SubscriptionUsageLimits {
      ai_chat_count_per_month: plan_limits.ai_chat_limit,
      ai_image_generation_per_month: plan_limits.ai_image_limit,
      cloud_storage_gb: plan_limits.storage_limit_gb,
    },
    current_usage: SubscriptionUsageMetrics {
      ai_chat_used_this_month: 0,
      ai_image_used_this_month: 0,
      storage_used_gb: 0.0,
    },
    remaining: SubscriptionUsageRemaining {
      ai_chat_remaining_this_month: plan_limits.ai_chat_limit,
      ai_image_remaining_this_month: plan_limits.ai_image_limit,
      storage_remaining_gb: plan_limits.storage_limit_gb,
    },
    addon_usage,
    daily_usage: vec![],
  })
}

pub async fn get_user_resource_limit_status(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<ResourceLimitStatus, AppError> {
  let now = Utc::now();
  let subscription = get_user_active_subscription(pg_pool, uid).await?;
  
  match subscription {
    Some(sub) => {
      let plan = get_subscription_plan(pg_pool, sub.plan_id).await?;
      Ok(ResourceLimitStatus {
        plan_code: plan.plan_code,
        storage_limit_mb: plan.cloud_storage_gb.to_f64().unwrap_or(0.0),
        workspace_limit: plan.collaborative_workspace_limit as i64,
        is_grace_period: false,
        grace_period_end: None,
      })
    }
    None => {
      // Check for recently expired subscription (within 15 days)
      let expired_sub = database::subscription::get_user_recently_expired_subscription(pg_pool, uid).await?;
      if let Some(sub) = expired_sub {
          let grace_end = sub.end_date + chrono::Duration::days(15);
          if now <= grace_end {
              let plan = get_subscription_plan(pg_pool, sub.plan_id).await?;
              return Ok(ResourceLimitStatus {
                  plan_code: plan.plan_code,
                  storage_limit_mb: plan.cloud_storage_gb.to_f64().unwrap_or(0.0),
                  workspace_limit: plan.collaborative_workspace_limit as i64,
                  is_grace_period: true,
                  grace_period_end: Some(grace_end),
              });
          }
      }
      
      // Default Free plan limits
      Ok(ResourceLimitStatus {
        plan_code: "free".to_string(),
        storage_limit_mb: 300.0,
        workspace_limit: 2,
        is_grace_period: false,
        grace_period_end: None,
      })
    }
  }
}

#[derive(Debug, Clone)]
pub struct ResourceLimitStatus {
  pub plan_code: String,
  pub storage_limit_mb: f64,
  pub workspace_limit: i64,
  pub is_grace_period: bool,
  pub grace_period_end: Option<chrono::DateTime<Utc>>,
}

fn resolve_date_range(query: SubscriptionUsageQuery) -> (NaiveDate, NaiveDate) {
  let today = Utc::now().date_naive();
  let (default_start, _) = month_range(Utc::now());

  let start = query.start_date.unwrap_or(default_start);
  let end = query.end_date.unwrap_or(today);
  (start, end)
}

fn convert_subscription_row(
  subscription: UserSubscriptionRow,
  plan: &SubscriptionPlanRow,
) -> Result<UserSubscriptionContext, AppError> {
  let billing = parse_billing_type(subscription.billing_type.as_str())?;
  let status = parse_status(subscription.status.as_str())?;
  Ok(UserSubscriptionContext {
    record: subscription,
    billing_type: billing,
    status,
    plan_code: plan.plan_code.clone(),
    plan_name_cn: plan.plan_name_cn.clone(),
  })
}

fn convert_user_addon(row: UserAddonRow) -> Result<UserAddonRecord, AppError> {
  let addon_type = parse_addon_type(row.addon_type.as_str())?;
  let status = parse_addon_status(row.status.as_str())?;
  Ok(UserAddonRecord {
    id: row.id,
    addon_id: row.addon_id,
    addon_code: row.addon_code,
    addon_name_cn: row.addon_name_cn,
    addon_type,
    quantity: row.quantity,
    price_yuan: decimal_to_f64(&row.price_yuan),
    storage_gb: row.storage_gb.map(|v| v as f64),
    ai_chat_count: row.ai_chat_count,
    ai_image_count: row.ai_image_count,
    start_date: row.start_date,
    end_date: row.end_date,
    status,
  })
}

fn to_plan_info(plan: SubscriptionPlanRow) -> SubscriptionPlanInfo {
  SubscriptionPlanInfo {
    id: plan.id,
    plan_code: plan.plan_code,
    plan_name: plan.plan_name,
    plan_name_cn: plan.plan_name_cn,
    monthly_price_yuan: decimal_to_f64(&plan.monthly_price_yuan),
    yearly_price_yuan: decimal_to_f64(&plan.yearly_price_yuan),
    cloud_storage_gb: decimal_to_f64(&plan.cloud_storage_gb),
    has_inbox: plan.has_inbox,
    has_multi_device_sync: plan.has_multi_device_sync,
    has_api_support: plan.has_api_support,
    version_history_days: plan.version_history_days,
    ai_chat_count_per_month: plan.ai_chat_count_per_month,
    ai_image_generation_per_month: plan.ai_image_generation_per_month,
    has_share_link: plan.has_share_link,
    has_publish: plan.has_publish,
    workspace_member_limit: plan.workspace_member_limit,
    collaborative_workspace_limit: plan.collaborative_workspace_limit,
    page_permission_guest_editors: plan.page_permission_guest_editors,
    has_space_member_management: plan.has_space_member_management,
    has_space_member_grouping: plan.has_space_member_grouping,
    is_active: plan.is_active,
  }
}

fn to_addon_info(addon: SubscriptionAddonRow) -> SubscriptionAddonInfo {
  SubscriptionAddonInfo {
    id: addon.id,
    addon_code: addon.addon_code,
    addon_name: addon.addon_name,
    addon_name_cn: addon.addon_name_cn,
    addon_type: parse_addon_type(addon.addon_type.as_str()).unwrap_or(AddonType::Storage),
    price_yuan: decimal_to_f64(&addon.price_yuan),
    storage_gb: addon.storage_gb.map(|v| v as f64),
    ai_chat_count: addon.ai_chat_count,
    ai_image_count: addon.ai_image_count,
    is_active: addon.is_active,
  }
}

fn decimal_to_f64(value: &Decimal) -> f64 {
  value.to_f64().unwrap_or(0.0)
}

fn parse_billing_type(value: &str) -> Result<BillingType, AppError> {
  match value {
    "monthly" => Ok(BillingType::Monthly),
    "yearly" => Ok(BillingType::Yearly),
    other => Err(AppError::InvalidRequest(format!(
      "unknown billing_type: {}",
      other
    ))),
  }
}

fn parse_status(value: &str) -> Result<SubscriptionStatus, AppError> {
  match value {
    "active" => Ok(SubscriptionStatus::Active),
    "canceled" => Ok(SubscriptionStatus::Canceled),
    "expired" => Ok(SubscriptionStatus::Expired),
    "pending" => Ok(SubscriptionStatus::Pending),
    other => Err(AppError::InvalidRequest(format!(
      "unknown subscription status: {}",
      other
    ))),
  }
}

fn parse_addon_type(value: &str) -> Result<AddonType, AppError> {
  match value {
    "storage" => Ok(AddonType::Storage),
    "ai_token" => Ok(AddonType::AiToken),
    other => Err(AppError::InvalidRequest(format!(
      "unknown addon_type: {}",
      other
    ))),
  }
}

fn parse_addon_status(value: &str) -> Result<AddonStatus, AppError> {
  match value {
    "active" => Ok(AddonStatus::Active),
    "expired" => Ok(AddonStatus::Expired),
    "used" => Ok(AddonStatus::Used),
    other => Err(AppError::InvalidRequest(format!(
      "unknown addon status: {}",
      other
    ))),
  }
}

fn usage_type_to_str(usage_type: UsageType) -> &'static str {
  match usage_type {
    UsageType::AiChat => "ai_chat",
    UsageType::AiImage => "ai_image",
    UsageType::StorageBytes => "storage_bytes",
  }
}

fn add_months(
  start: chrono::DateTime<Utc>,
  months: u32,
) -> Option<chrono::DateTime<Utc>> {
  start.checked_add_months(Months::new(months.into()))
}

fn month_range(reference: chrono::DateTime<Utc>) -> (NaiveDate, NaiveDate) {
  let start = NaiveDate::from_ymd_opt(reference.year(), reference.month(), 1)
    .unwrap_or_else(|| reference.date_naive());
  (start, reference.date_naive())
}

#[derive(Clone)]
struct UserSubscriptionContext {
  record: UserSubscriptionRow,
  billing_type: BillingType,
  status: SubscriptionStatus,
  plan_code: String,
  plan_name_cn: String,
}

impl UserSubscriptionContext {
  fn into_record(self) -> Result<UserSubscriptionRecord, AppError> {
    Ok(UserSubscriptionRecord {
      id: self.record.id,
      plan_id: self.record.plan_id,
      plan_code: self.plan_code,
      plan_name_cn: self.plan_name_cn,
      billing_type: self.billing_type,
      status: self.status,
      start_date: self.record.start_date,
      end_date: self.record.end_date,
      canceled_at: self.record.canceled_at,
      cancel_reason: self.record.cancel_reason,
    })
  }
}

#[derive(Debug, Clone)]
struct PlanLimitsContext {
  ai_chat_limit: Option<i64>,
  ai_image_limit: Option<i64>,
  storage_limit_gb: Option<f64>,
}

impl From<&SubscriptionPlanRow> for PlanLimitsContext {
  fn from(plan: &SubscriptionPlanRow) -> Self {
    let ai_chat_limit = normalize_limit(plan.ai_chat_count_per_month);
    let ai_image_limit = normalize_limit(plan.ai_image_generation_per_month);
    // cloud_storage_gb 现在是 Decimal 类型（单位为MB），转换为 GB
    let storage_mb = plan.cloud_storage_gb.to_f64().unwrap_or(0.0);
    let storage_limit_gb = match storage_mb {
      x if x < 0.0 => None,  // -1 表示无限制
      _ => Some(storage_mb / 1024.0),  // MB 转 GB
    };
    Self {
      ai_chat_limit,
      ai_image_limit,
      storage_limit_gb,
    }
  }
}

fn bytes_to_gb(bytes: i64) -> f64 {
  (bytes as f64) / STORAGE_GB_IN_BYTES
}

fn normalize_limit(value: i32) -> Option<i64> {
  match value.cmp(&0) {
    Ordering::Less => None,
    Ordering::Equal => None,
    Ordering::Greater => Some(value as i64),
  }
}

async fn cancel_user_subscription_with_reason(
  pg_pool: &PgPool,
  uid: i64,
  reason: Option<String>,
) -> Result<UserSubscriptionRow, AppError> {
  database::subscription::cancel_user_subscription(pg_pool, uid, reason).await
}
