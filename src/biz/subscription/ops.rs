use std::cmp::Ordering;

use app_error::AppError;
use chrono::{Datelike, Months, NaiveDate, Utc};
use database::subscription::{
  aggregate_user_usage, calculate_addon_period_end, get_subscription_addon, get_subscription_plan,
  get_user_active_subscription, insert_user_addon, list_daily_usage, list_subscription_addons,
  list_user_addons, list_subscription_plans, upsert_usage_record, upsert_user_subscription,
  SubscriptionAddonRow, SubscriptionPlanRow, UserAddonRow, UserSubscriptionRow,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use shared_entity::dto::subscription_dto::{
  AddonStatus, AddonType, BillingType, CancelSubscriptionRequest, PurchaseAddonRequest,
  SubscribeRequest, SubscriptionAddonInfo, SubscriptionAddonUsage, SubscriptionCurrentResponse,
  SubscriptionCurrentUsage, SubscriptionDailyUsage, SubscriptionPlanInfo, SubscriptionStatus,
  SubscriptionUsageLimits, SubscriptionUsageMetrics, SubscriptionUsageQuery,
  SubscriptionUsageRemaining, SubscriptionUsageResponse, UsageRecordRequest, UsageType,
  UserAddonRecord, UserSubscriptionRecord,
};
use sqlx::PgPool;

const STORAGE_GB_IN_BYTES: f64 = 1024.0 * 1024.0 * 1024.0;

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
  let active_addons = list_user_addons(pg_pool, uid, Some("active")).await?;
  let addon_ctx = AddonContext::from_rows(&active_addons);

  let (start_date, end_date) = month_range(Utc::now());
  let usage_aggregates = aggregate_user_usage(pg_pool, uid, start_date, end_date).await?;
  let usage = UsageAggregate::from_rows(&usage_aggregates);

  let current_usage =
    build_current_usage_summary(&limits, &addon_ctx, &usage);

  Ok(SubscriptionCurrentResponse {
    subscription: convert_subscription_row(subscription, &plan)?.into_record()?,
    plan_details: plan_info,
    usage: current_usage,
  })
}

async fn build_usage_response(
  pg_pool: &PgPool,
  uid: i64,
  subscription: UserSubscriptionRow,
  plan: SubscriptionPlanRow,
  query: SubscriptionUsageQuery,
) -> Result<SubscriptionUsageResponse, AppError> {
  let plan_limits = PlanLimitsContext::from(&plan);
  let active_addons = list_user_addons(pg_pool, uid, Some("active")).await?;
  let addon_ctx = AddonContext::from_rows(&active_addons);

  let (start_date, end_date) = resolve_date_range(query);
  if start_date > end_date {
    return Err(AppError::InvalidRequest(
      "start_date must be before end_date".into(),
    ));
  }

  let aggregates = aggregate_user_usage(pg_pool, uid, start_date, end_date).await?;
  let daily_usage_rows = list_daily_usage(pg_pool, uid, start_date, end_date).await?;

  let usage = UsageAggregate::from_rows(&aggregates);
  let subscription_limits = plan_limits.combine_with_addons(&addon_ctx);
  let current_usage = usage.to_metrics();
  let remaining = calculate_remaining(&subscription_limits, &usage);
  let addon_usage = addon_ctx.to_usage(&plan_limits, &usage);
  let daily_usage = daily_usage_rows.into_iter().map(convert_daily_usage_row).collect();

  // ensure subscription still valid? for now just return
  let _ = convert_subscription_row(subscription, &plan)?;

  Ok(SubscriptionUsageResponse {
    subscription_limits,
    current_usage,
    remaining,
    addon_usage,
    daily_usage,
  })
}

fn resolve_date_range(query: SubscriptionUsageQuery) -> (NaiveDate, NaiveDate) {
  let today = Utc::now().date_naive();
  let (default_start, _) = month_range(Utc::now());

  let start = query.start_date.unwrap_or(default_start);
  let end = query.end_date.unwrap_or(today);
  (start, end)
}

fn build_current_usage_summary(
  limits: &PlanLimitsContext,
  addon_ctx: &AddonContext,
  usage: &UsageAggregate,
) -> SubscriptionCurrentUsage {
  let plan_storage = limits.storage_gb();
  let total_storage_gb = match (plan_storage, addon_ctx.storage_gb()) {
    (Some(plan), Some(addon)) => Some(plan + addon),
    (Some(plan), None) => Some(plan),
    (None, Some(addon)) => Some(addon),
    (None, None) => None,
  };

  let remaining_chat = limits
    .ai_chat_limit
    .map(|limit| (limit + addon_ctx.ai_chat_total()).saturating_sub(usage.ai_chat));

  let remaining_image = limits
    .ai_image_limit
    .map(|limit| (limit + addon_ctx.ai_image_total()).saturating_sub(usage.ai_image));

  let _remaining_storage = total_storage_gb.map(|total| (total - usage.storage_gb).max(0.0));

  SubscriptionCurrentUsage {
    ai_chat_used_this_month: usage.ai_chat,
    ai_chat_remaining_this_month: remaining_chat,
    ai_image_used_this_month: usage.ai_image,
    ai_image_remaining_this_month: remaining_image,
    storage_used_gb: usage.storage_gb,
    storage_total_gb: total_storage_gb,
  }
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
    storage_gb: row.storage_gb,
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
    cloud_storage_gb: plan.cloud_storage_gb,
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
    storage_gb: addon.storage_gb,
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
    let storage_limit_gb = match plan.cloud_storage_gb {
      x if x < 0 => None,
      0 => None,
      other => Some(other as f64),
    };
    Self {
      ai_chat_limit,
      ai_image_limit,
      storage_limit_gb,
    }
  }
}

impl PlanLimitsContext {
  fn storage_gb(&self) -> Option<f64> {
    self.storage_limit_gb
  }

  fn combine_with_addons(&self, addon: &AddonContext) -> SubscriptionUsageLimits {
    let storage = match (self.storage_limit_gb, addon.storage_gb()) {
      (Some(plan), Some(addon)) => Some(plan + addon),
      (Some(plan), None) => Some(plan),
      (None, Some(addon)) => Some(addon),
      (None, None) => None,
    };
    SubscriptionUsageLimits {
      ai_chat_count_per_month: self
        .ai_chat_limit
        .map(|limit| limit + addon.ai_chat_total()),
      ai_image_generation_per_month: self
        .ai_image_limit
        .map(|limit| limit + addon.ai_image_total()),
      cloud_storage_gb: storage,
    }
  }
}

#[derive(Debug, Clone, Default)]
struct AddonContext {
  storage_total_gb: f64,
  ai_chat_total: i64,
  ai_image_total: i64,
}

impl AddonContext {
  fn from_rows(rows: &[UserAddonRow]) -> Self {
    let mut ctx = Self::default();
    for row in rows {
      if row.status != "active" {
        continue;
      }
      match row.addon_type.as_str() {
        "storage" => {
          if let Some(storage) = row.storage_gb {
            ctx.storage_total_gb += (storage * row.quantity) as f64;
          }
        },
        "ai_token" => {
          if let Some(ai_chat) = row.ai_chat_count {
            ctx.ai_chat_total += (ai_chat * row.quantity) as i64;
          }
          if let Some(ai_image) = row.ai_image_count {
            ctx.ai_image_total += (ai_image * row.quantity) as i64;
          }
        },
        _ => {},
      }
    }
    ctx
  }

  fn storage_gb(&self) -> Option<f64> {
    if self.storage_total_gb > 0.0 {
      Some(self.storage_total_gb)
    } else {
      None
    }
  }

  fn ai_chat_total(&self) -> i64 {
    self.ai_chat_total
  }

  fn ai_image_total(&self) -> i64 {
    self.ai_image_total
  }

  fn to_usage(&self, plan_limits: &PlanLimitsContext, usage: &UsageAggregate) -> SubscriptionAddonUsage {
    let total_storage = self.storage_total_gb;
    let total_chat = self.ai_chat_total;
    let total_image = self.ai_image_total;

    let plan_chat_limit = plan_limits.ai_chat_limit.unwrap_or(i64::MAX);
    let plan_image_limit = plan_limits.ai_image_limit.unwrap_or(i64::MAX);

    let addon_chat_used = usage.ai_chat.saturating_sub(plan_chat_limit).clamp(0, total_chat);
    let addon_image_used = usage
      .ai_image
      .saturating_sub(plan_image_limit)
      .clamp(0, total_image);

    SubscriptionAddonUsage {
      storage_addon_total_gb: total_storage,
      ai_token_addon_chat_count: total_chat,
      ai_token_addon_image_count: total_image,
      ai_token_addon_chat_used: addon_chat_used,
      ai_token_addon_image_used: addon_image_used,
    }
  }
}

#[derive(Debug, Default)]
struct UsageAggregate {
  ai_chat: i64,
  ai_image: i64,
  storage_bytes: i64,
  storage_gb: f64,
}

impl UsageAggregate {
  fn from_rows(rows: &[database::subscription::UsageAggregateRow]) -> Self {
    let mut aggregate = UsageAggregate::default();
    for row in rows {
      match row.usage_type.as_str() {
        "ai_chat" => aggregate.ai_chat += row.total,
        "ai_image" => aggregate.ai_image += row.total,
        "storage_bytes" => {
          aggregate.storage_bytes += row.total;
        },
        _ => {},
      }
    }
    aggregate.storage_gb = bytes_to_gb(aggregate.storage_bytes);
    aggregate
  }

  fn to_metrics(&self) -> SubscriptionUsageMetrics {
    SubscriptionUsageMetrics {
      ai_chat_used_this_month: self.ai_chat,
      ai_image_used_this_month: self.ai_image,
      storage_used_gb: self.storage_gb,
    }
  }
}

fn calculate_remaining(
  limits: &SubscriptionUsageLimits,
  usage: &UsageAggregate,
) -> SubscriptionUsageRemaining {
  let ai_chat_remaining = limits
    .ai_chat_count_per_month
    .map(|limit| limit.saturating_sub(usage.ai_chat));
  let ai_image_remaining = limits
    .ai_image_generation_per_month
    .map(|limit| limit.saturating_sub(usage.ai_image));

  let storage_remaining = limits.cloud_storage_gb.map(|limit| {
    let remaining = limit - usage.storage_gb;
    if remaining < 0.0 { 0.0 } else { remaining }
  });

  SubscriptionUsageRemaining {
    ai_chat_remaining_this_month: ai_chat_remaining,
    ai_image_remaining_this_month: ai_image_remaining,
    storage_remaining_gb: storage_remaining,
  }
}

fn convert_daily_usage_row(row: database::subscription::DailyUsageRow) -> SubscriptionDailyUsage {
  SubscriptionDailyUsage {
    date: row.usage_date,
    ai_chat_count: row.ai_chat_count,
    ai_image_count: row.ai_image_count,
    storage_bytes: row.storage_bytes,
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

