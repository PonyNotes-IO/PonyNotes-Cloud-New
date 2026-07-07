use std::cmp::Ordering;

use app_error::AppError;
use chrono::{Datelike, Months, NaiveDate, Utc};
use database::subscription::{aggregate_user_usage, calculate_addon_period_end, extend_user_subscription, get_or_create_free_subscription, get_plan_level, get_subscription_addon, get_subscription_plan, get_subscription_plan_by_code, get_user_active_subscription, get_user_owned_workspace_count, get_user_total_usage_bytes, insert_user_addon, list_subscription_addons, list_subscription_plans, list_user_addons, restore_blobs_within_budget, restore_workspaces_within_limit, upsert_usage_record, upsert_user_subscription, SubscriptionAddonRow, SubscriptionPlanRow, UserAddonRow, UserSubscriptionRow};
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
  // 如果用户没有订阅，自动创建免费版订阅
  let subscription = get_or_create_free_subscription(pg_pool, uid).await?;
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
  let existing_sub = get_user_active_subscription(pg_pool, uid).await?;

  let new_level = get_plan_level(&plan.plan_code);

  let subscription = if let Some(ref old_sub) = existing_sub {
    let old_plan = get_subscription_plan(pg_pool, old_sub.plan_id).await?;
    let old_level = get_plan_level(&old_plan.plan_code);

    if old_sub.plan_id == plan.id {
      // 规则3：同级别续费只延长有效期。不作废、不新建，
      // 保持同一 subscription_id，当期 AI 用量/剩余额度延续，不会被重置。
      log::info!(
        "[订阅续费] uid: {}, 套餐 {} 续费 {} 个月，仅顺延有效期（订阅 id={} 不变，AI 用量不重置）",
        uid, plan.plan_code, request.billing_type.months(), old_sub.id
      );
      extend_user_subscription(
        pg_pool, old_sub.id, request.billing_type.months(), billing_type_str,
      ).await?
    } else if old_level > 0 && new_level < old_level {
      // 规则2：高级别会员生效期间，不允许再购买低级别会员。
      return Err(AppError::InvalidRequest(format!(
        "当前 {} 会员生效期间，不能购买更低级别的 {}",
        old_plan.plan_name_cn, plan.plan_name_cn
      )));
    } else if old_level > 0 && new_level > old_level {
      // 规则4：年付会员生效期间，只能购买年付的更高级别会员。
      if old_sub.billing_type == "yearly" && request.billing_type.as_str() == "monthly" {
        return Err(AppError::InvalidRequest(
          "年付会员生效期间，只能购买年付的更高级别会员".to_string(),
        ));
      }
      // 规则1/4：升级 → 低级别套餐直接废弃（canceled），高级别立即生效。
      log::info!(
        "[订阅升级] uid: {}, 从 {} (等级{}) 升级到 {} (等级{})，旧套餐直接废弃",
        uid, old_plan.plan_code, old_level, plan.plan_code, new_level
      );
      upsert_user_subscription(
        pg_pool, uid, plan.id, billing_type_str, start_date, end_date, None, None,
      ).await?
    } else {
      // 从免费版(等级0)购买付费套餐 / 同级换套餐：
      // 若此前有更高级别套餐已过期（高级别过期后续费较低级别），给 15 天宽限期，
      // 期满后由清理任务对超出新套餐限额的内容做逻辑删除。
      let (grace_end, downgraded_from) =
        grace_for_rebuy_lower_plan(pg_pool, uid, new_level, start_date).await?;
      upsert_user_subscription(
        pg_pool, uid, plan.id, billing_type_str, start_date, end_date,
        grace_end, downgraded_from,
      ).await?
    }
  } else {
    // 无现有 active：直接插入新 active（同样检查"过期后续费较低级别"的宽限期）。
    let (grace_end, downgraded_from) =
      grace_for_rebuy_lower_plan(pg_pool, uid, new_level, start_date).await?;
    upsert_user_subscription(
      pg_pool, uid, plan.id, billing_type_str, start_date, end_date,
      grace_end, downgraded_from,
    ).await?
  };

  // 购买成功后，在新套餐限额内自动恢复此前被逻辑删除的内容（工作区/文件）。
  // 恢复失败不影响订阅生效，仅记录日志。
  if let Err(e) = restore_resources_within_plan_limits(pg_pool, uid, &plan).await {
    log::error!("[订阅购买] uid: {}, 自动恢复逻辑删除内容失败: {:?}", uid, e);
  }

  build_current_subscription_with_plan(pg_pool, uid, subscription, plan).await
}

/// 判断本次购买是否属于"更高级别套餐过期后，续费较低级别"：
/// 是则返回 15 天宽限期（期间保留超额内容，期满由清理任务逻辑删除多余部分）。
async fn grace_for_rebuy_lower_plan(
  pg_pool: &PgPool,
  uid: i64,
  new_level: i32,
  start_date: chrono::DateTime<Utc>,
) -> Result<(Option<chrono::DateTime<Utc>>, Option<i64>), AppError> {
  if new_level <= 0 {
    return Ok((None, None));
  }
  let recently_expired =
    database::subscription::get_user_recently_expired_subscription(pg_pool, uid).await?;
  if let Some(prev) = recently_expired {
    let prev_plan = get_subscription_plan(pg_pool, prev.plan_id).await?;
    if get_plan_level(&prev_plan.plan_code) > new_level {
      let grace_end = start_date + chrono::Duration::days(15);
      log::info!(
        "[订阅降级续费] uid: {}, 此前 {} 已过期, 现购买较低级别, 宽限期至 {}",
        uid, prev_plan.plan_code, grace_end
      );
      return Ok((Some(grace_end), Some(prev.plan_id)));
    }
  }
  Ok((None, None))
}

/// 购买/续费成功后，在新套餐限额内自动恢复被逻辑删除的工作区与文件。
async fn restore_resources_within_plan_limits(
  pg_pool: &PgPool,
  uid: i64,
  plan: &SubscriptionPlanRow,
) -> Result<(), AppError> {
  let workspace_limit = plan.collaborative_workspace_limit as i64;
  let restored_workspaces =
    restore_workspaces_within_limit(pg_pool, uid, workspace_limit).await?;

  let limit_bytes =
    (plan.cloud_storage_gb.to_f64().unwrap_or(0.0) * STORAGE_MB_IN_BYTES) as i64;
  let current_usage = get_user_total_usage_bytes(pg_pool, uid).await?;
  let restored_blobs =
    restore_blobs_within_budget(pg_pool, uid, limit_bytes - current_usage).await?;

  if restored_workspaces > 0 || restored_blobs > 0 {
    log::info!(
      "[订阅购买] uid: {}, 自动恢复逻辑删除内容: 工作区 {} 个, 文件 {} 个",
      uid, restored_workspaces, restored_blobs
    );
  }
  Ok(())
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
  // 解析用户当前订阅（不存在则自动创建免费订阅），把 subscription_id 一并记入用量。
  // 解析失败时不阻断用量记录，退化为 None（仅极端情况，正常用户必有 active 订阅）。
  let subscription_id = get_or_create_free_subscription(pg_pool, uid)
    .await
    .ok()
    .map(|s| s.id);
  upsert_usage_record(
    pg_pool,
    uid,
    subscription_id,
    usage_type,
    usage_date,
    payload.usage_count,
  )
  .await?;
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

  // AI 次数额度按"计费月"统计：以订阅 start_date 为锚点逐月推进，取 now 所在的那个月窗口。
  // 同套餐续费顺延 end_date 后，每个计费月各自享有整月额度；续费当月已用次数延续、不重置。
  let (usage_start_date, usage_end_date) =
    current_billing_period(subscription.start_date, subscription.end_date);

  // Real AI Usage（只统计当前订阅套餐的用量）
  let usage =
    aggregate_user_usage(pg_pool, uid, usage_start_date, usage_end_date, Some(subscription.id))
      .await?;
  let ai_chat_used = usage.iter().find(|u| u.usage_type == "ai_chat").map(|u| u.total).unwrap_or(0);
  let ai_image_used = usage.iter().find(|u| u.usage_type == "ai_image").map(|u| u.total).unwrap_or(0);
  
  // Real Storage Usage
  let storage_used_bytes = get_user_total_usage_bytes(pg_pool, uid).await?; // 使用的字节 这个是对的，统计了所有的file_size  文件统计
  let storage_total_mb = plan.cloud_storage_gb.to_f64().unwrap_or(0.0); // 总 mb
  let storage_total_bytes = (storage_total_mb * STORAGE_MB_IN_BYTES) as i64; // 总 字节
  let storage_remaining_bytes = storage_total_bytes - storage_used_bytes; // 剩余
  
  // Real Workspace Usage
  let workspace_used = get_user_owned_workspace_count(pg_pool, uid).await?; // 工作空间数量
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

      // 注：sub 来自 get_user_active_subscription，查询条件已保证 end_date > NOW()，
      // 因此这里不会再出现"active 但已过期"的记录，已过期场景统一由下方
      // None 分支（走 get_user_recently_expired_subscription）处理，避免两套宽限期
      // 计算并存导致口径不一致（2026-07-07 清理原 sub.end_date < now 死分支）。

      // 订阅有效，检查是否处于降级宽限期
      if let (Some(grace_end), Some(old_plan_id)) = (sub.grace_period_end, sub.downgraded_from_plan_id) {
        if now <= grace_end {
          // 降级宽限期内，使用旧套餐的资源限制
          let old_plan = get_subscription_plan(pg_pool, old_plan_id as i64).await?;
          return Ok(ResourceLimitStatus {
            plan_code: plan.plan_code.clone(),
            storage_limit_mb: old_plan.cloud_storage_gb.to_f64().unwrap_or(0.0),
            workspace_limit: old_plan.collaborative_workspace_limit as i64,
            member_limit: old_plan.workspace_member_limit as i64,
            is_grace_period: true,
            grace_period_end: Some(grace_end),
          });
        }
      }

      Ok(ResourceLimitStatus {
        plan_code: plan.plan_code,
        storage_limit_mb: plan.cloud_storage_gb.to_f64().unwrap_or(0.0),
        workspace_limit: plan.collaborative_workspace_limit as i64,
        member_limit: plan.workspace_member_limit as i64,
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
            member_limit: plan.workspace_member_limit as i64,
            is_grace_period: true,
            grace_period_end: Some(grace_end),
          });
        }
      }

      // 没有订阅记录，返回免费版限制（适用于新用户或完全无订阅用户）
      let free_plan = get_subscription_plan_by_code(pg_pool, "mfb").await?;
      Ok(ResourceLimitStatus {
        plan_code: free_plan.plan_code,
        storage_limit_mb: free_plan.cloud_storage_gb.to_f64().unwrap_or(0.0),
        workspace_limit: free_plan.collaborative_workspace_limit as i64,
        member_limit: free_plan.workspace_member_limit as i64,
        is_grace_period: false,
        grace_period_end: None,
      })
    }
  }
}

/// 检查用户存储容量是否足够写入 `data_size_bytes` 字节的数据
pub async fn check_user_storage_limit(
  pg_pool: &PgPool,
  uid: i64,
  data_size_bytes: i64,
) -> Result<(), AppError> {
  let resource_status = get_user_resource_limit_status(pg_pool, uid).await?;
  let total_limit_bytes = (resource_status.storage_limit_mb * STORAGE_MB_IN_BYTES) as i64;
  let current_usage = get_user_total_usage_bytes(pg_pool, uid).await?;
  
  log::info!(
    "[STORAGE_CHECK] uid: {}, current_usage: {} bytes, limit: {} bytes, data_size: {} bytes, plan: {}",
    uid, current_usage, total_limit_bytes, data_size_bytes, resource_status.plan_code
  );
  
  if current_usage + data_size_bytes > total_limit_bytes {
    log::error!(
      "[STORAGE_CHECK] Storage limit exceeded! uid: {}, current: {}, limit: {}, data: {}",
      uid, current_usage, total_limit_bytes, data_size_bytes
    );
    return Err(AppError::PlanLimitExceeded(format!(
      "Storage limit exceeded. Current: {} bytes, Limit: {} bytes, Data: {} bytes",
      current_usage, total_limit_bytes, data_size_bytes
    )));
  }
  Ok(())
}

#[derive(Debug, Clone)]
pub struct ResourceLimitStatus {
  pub plan_code: String,
  pub storage_limit_mb: f64,
  pub workspace_limit: i64,
  pub member_limit: i64,
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

/// 计算当前时间落在订阅的哪个"计费月"窗口内（以 start_date 为锚点按整月推进）。
/// 返回 (窗口起始日, 窗口结束日)，结束日不超过订阅 end_date。
fn current_billing_period(
  start: chrono::DateTime<Utc>,
  end: chrono::DateTime<Utc>,
) -> (NaiveDate, NaiveDate) {
  let now = Utc::now();
  let mut months =
    ((now.year() - start.year()) * 12 + now.month() as i32 - start.month() as i32).max(0) as u32;
  // 若本月锚点日尚未到达（如 15 日订阅、今天是下月 10 日），回退一个月
  if add_months(start, months).map_or(false, |p| p > now) {
    months = months.saturating_sub(1);
  }
  let period_start = add_months(start, months).unwrap_or(start).min(end);
  let period_end = add_months(period_start, 1).unwrap_or(end).min(end);
  (period_start.date_naive(), period_end.date_naive())
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
      grace_period_end: self.record.grace_period_end,
      downgraded_from_plan_id: self.record.downgraded_from_plan_id.map(|v| v as i64),
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
