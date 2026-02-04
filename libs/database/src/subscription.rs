use app_error::AppError;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use tracing::instrument;

// Row structures
#[derive(Debug, Clone)]
pub struct SubscriptionPlanRow {
  pub id: i64,
  pub plan_code: String,
  pub plan_name: String,
  pub plan_name_cn: String,
  pub monthly_price_yuan: Decimal,
  pub yearly_price_yuan: Decimal,
  pub cloud_storage_gb: Decimal,  // 改为 Decimal 类型，支持 MB 单位（300表示300MB）
  pub has_inbox: bool,
  pub has_multi_device_sync: bool,
  pub has_api_support: bool,
  pub version_history_days: i32,
  pub ai_chat_count_per_month: i32,
  pub ai_image_generation_per_month: i32,
  pub has_share_link: bool,
  pub has_publish: bool,
  pub workspace_member_limit: i32,
  pub collaborative_workspace_limit: i32,
  pub page_permission_guest_editors: i32,
  pub has_space_member_management: bool,
  pub has_space_member_grouping: bool,
  pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct UserSubscriptionRow {
  pub id: i64,
  pub uid: i64,
  pub plan_id: i64,
  pub billing_type: String,
  pub status: String,
  pub start_date: DateTime<Utc>,
  pub end_date: DateTime<Utc>,
  pub canceled_at: Option<DateTime<Utc>>,
  pub cancel_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SubscriptionAddonRow {
  pub id: i64,
  pub addon_code: String,
  pub addon_name: String,
  pub addon_name_cn: String,
  pub addon_type: String,
  pub price_yuan: Decimal,
  pub storage_gb: Option<i32>,
  pub ai_chat_count: Option<i32>,
  pub ai_image_count: Option<i32>,
  pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct UserAddonRow {
  pub id: i64,
  pub uid: i64,
  pub addon_id: i64,
  pub addon_code: String,
  pub addon_name_cn: String,
  pub addon_type: String,
  pub quantity: i32,
  pub price_yuan: Decimal,
  pub storage_gb: Option<i32>,
  pub ai_chat_count: Option<i32>,
  pub ai_image_count: Option<i32>,
  pub start_date: DateTime<Utc>,
  pub end_date: DateTime<Utc>,
  pub status: String,
}

#[derive(Debug, Clone)]
pub struct UsageAggregateRow {
  pub usage_type: String,
  pub total: i64,
}

#[derive(Debug, Clone)]
pub struct DailyUsageRow {
  pub usage_date: NaiveDate,
  pub ai_chat_count: i64,
  pub ai_image_count: i64,
  pub storage_bytes: i64,
}

// Subscription Plans
#[instrument(skip_all, err)]
pub async fn list_subscription_plans(pg_pool: &PgPool) -> Result<Vec<SubscriptionPlanRow>, AppError> {
  let rows = sqlx::query(
    r#"
    SELECT id, plan_code, plan_name, plan_name_cn,
           monthly_price_yuan, yearly_price_yuan,
           cloud_storage_gb, has_inbox, has_multi_device_sync, has_api_support,
           version_history_days, ai_chat_count_per_month, ai_image_generation_per_month,
           has_share_link, has_publish, workspace_member_limit,
           collaborative_workspace_limit, page_permission_guest_editors,
           has_space_member_management, has_space_member_grouping, is_active
    FROM af_subscription_plans
    ORDER BY id
    "#,
  )
  .fetch_all(pg_pool)
  .await?;

  Ok(rows
    .into_iter()
    .map(|row| SubscriptionPlanRow {
      id: row.get(0),
      plan_code: row.get(1),
      plan_name: row.get(2),
      plan_name_cn: row.get(3),
      monthly_price_yuan: row.get(4),
      yearly_price_yuan: row.get(5),
      cloud_storage_gb: row.get(6),
      has_inbox: row.get(7),
      has_multi_device_sync: row.get(8),
      has_api_support: row.get(9),
      version_history_days: row.get(10),
      ai_chat_count_per_month: row.get(11),
      ai_image_generation_per_month: row.get(12),
      has_share_link: row.get(13),
      has_publish: row.get(14),
      workspace_member_limit: row.get(15),
      collaborative_workspace_limit: row.get(16),
      page_permission_guest_editors: row.get(17),
      has_space_member_management: row.get(18),
      has_space_member_grouping: row.get(19),
      is_active: row.get(20),
    })
    .collect())
}

#[instrument(skip_all, err)]
pub async fn get_subscription_plan(
  pg_pool: &PgPool,
  plan_id: i64,
) -> Result<SubscriptionPlanRow, AppError> {
  let row = sqlx::query(
    r#"
    SELECT id, plan_code, plan_name, plan_name_cn,
           monthly_price_yuan, yearly_price_yuan,
           cloud_storage_gb, has_inbox, has_multi_device_sync, has_api_support,
           version_history_days, ai_chat_count_per_month, ai_image_generation_per_month,
           has_share_link, has_publish, workspace_member_limit,
           collaborative_workspace_limit, page_permission_guest_editors,
           has_space_member_management, has_space_member_grouping, is_active
    FROM af_subscription_plans
    WHERE id = $1
    "#,
  )
  .bind(plan_id)
  .fetch_one(pg_pool)
  .await?;

  Ok(SubscriptionPlanRow {
    id: row.get(0),
    plan_code: row.get(1),
    plan_name: row.get(2),
    plan_name_cn: row.get(3),
    monthly_price_yuan: row.get(4),
    yearly_price_yuan: row.get(5),
    cloud_storage_gb: row.get(6),
    has_inbox: row.get(7),
    has_multi_device_sync: row.get(8),
    has_api_support: row.get(9),
    version_history_days: row.get(10),
    ai_chat_count_per_month: row.get(11),
    ai_image_generation_per_month: row.get(12),
    has_share_link: row.get(13),
    has_publish: row.get(14),
    workspace_member_limit: row.get(15),
    collaborative_workspace_limit: row.get(16),
    page_permission_guest_editors: row.get(17),
    has_space_member_management: row.get(18),
    has_space_member_grouping: row.get(19),
    is_active: row.get(20),
  })
}

#[instrument(skip_all, err)]
pub async fn get_user_recently_expired_subscription(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Option<UserSubscriptionRow>, AppError> {
  let row = sqlx::query(
    r#"
    SELECT id, uid, plan_id, billing_type, status,
           start_date, end_date, canceled_at, cancel_reason
    FROM af_user_subscriptions
    WHERE uid = $1 AND (status = 'active' OR status = 'canceled') AND end_date <= NOW()
    ORDER BY end_date DESC
    LIMIT 1
    "#,
  )
  .bind(uid)
  .fetch_optional(pg_pool)
  .await?;

  if let Some(row) = row {
    Ok(Some(UserSubscriptionRow {
      id: row.get(0),
      uid: row.get(1),
      plan_id: row.get(2),
      billing_type: row.get(3),
      status: row.get(4),
      start_date: row.get(5),
      end_date: row.get(6),
      canceled_at: row.get(7),
      cancel_reason: row.get(8),
    }))
  } else {
    Ok(None)
  }
}

// User Subscriptions
#[instrument(skip_all, err)]
pub async fn get_user_active_subscription(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Option<UserSubscriptionRow>, AppError> {
  let row = sqlx::query(
    r#"
    SELECT id, uid, plan_id, billing_type, status,
           start_date, end_date, canceled_at, cancel_reason
    FROM af_user_subscriptions
    WHERE uid = $1 AND status = 'active' AND end_date > NOW()
    ORDER BY id DESC
    LIMIT 1
    "#,
  )
  .bind(uid)
  .fetch_optional(pg_pool)
  .await?;

  if let Some(row) = row {
    Ok(Some(UserSubscriptionRow {
      id: row.get(0),
      uid: row.get(1),
      plan_id: row.get(2),
      billing_type: row.get(3),
      status: row.get(4),
      start_date: row.get(5),
      end_date: row.get(6),
      canceled_at: row.get(7),
      cancel_reason: row.get(8),
    }))
  } else {
    Ok(None)
  }
}

#[instrument(skip_all, err)]
pub async fn upsert_user_subscription(
  pg_pool: &PgPool,
  uid: i64,
  plan_id: i64,
  billing_type: &str,
  start_date: DateTime<Utc>,
  end_date: DateTime<Utc>,
) -> Result<UserSubscriptionRow, AppError> {
  // First, cancel any existing active subscriptions
  sqlx::query(
    r#"
    UPDATE af_user_subscriptions
    SET status = 'canceled', canceled_at = NOW()
    WHERE uid = $1 AND status = 'active'
    "#,
  )
  .bind(uid)
  .execute(pg_pool)
  .await?;

  // Insert new subscription
  let row = sqlx::query(
    r#"
    INSERT INTO af_user_subscriptions (uid, plan_id, billing_type, status, start_date, end_date)
    VALUES ($1, $2, $3, 'active', $4, $5)
    RETURNING id, uid, plan_id, billing_type, status, start_date, end_date, canceled_at, cancel_reason
    "#,
  )
  .bind(uid)
  .bind(plan_id)
  .bind(billing_type)
  .bind(start_date)
  .bind(end_date)
  .fetch_one(pg_pool)
  .await?;

  Ok(UserSubscriptionRow {
    id: row.get(0),
    uid: row.get(1),
    plan_id: row.get(2),
    billing_type: row.get(3),
    status: row.get(4),
    start_date: row.get(5),
    end_date: row.get(6),
    canceled_at: row.get(7),
    cancel_reason: row.get(8),
  })
}

#[instrument(skip_all, err)]
pub async fn cancel_user_subscription(
  pg_pool: &PgPool,
  uid: i64,
  reason: Option<String>,
) -> Result<UserSubscriptionRow, AppError> {
  let row = sqlx::query(
    r#"
    UPDATE af_user_subscriptions
    SET status = 'canceled', canceled_at = NOW(), cancel_reason = $2
    WHERE uid = $1 AND status = 'active'
    RETURNING id, uid, plan_id, billing_type, status, start_date, end_date, canceled_at, cancel_reason
    "#,
  )
  .bind(uid)
  .bind(reason)
  .fetch_one(pg_pool)
  .await?;

  Ok(UserSubscriptionRow {
    id: row.get(0),
    uid: row.get(1),
    plan_id: row.get(2),
    billing_type: row.get(3),
    status: row.get(4),
    start_date: row.get(5),
    end_date: row.get(6),
    canceled_at: row.get(7),
    cancel_reason: row.get(8),
  })
}

// Subscription Addons
#[instrument(skip_all, err)]
pub async fn list_subscription_addons(
  pg_pool: &PgPool,
  addon_type: Option<&str>,
) -> Result<Vec<SubscriptionAddonRow>, AppError> {
  let query = if let Some(ty) = addon_type {
    sqlx::query(
      r#"
      SELECT id, addon_code, addon_name, addon_name_cn, addon_type,
             price_yuan, storage_gb, ai_chat_count, ai_image_count, is_active
      FROM af_subscription_addons
      WHERE addon_type = $1
      ORDER BY id
      "#,
    )
    .bind(ty)
  } else {
    sqlx::query(
      r#"
      SELECT id, addon_code, addon_name, addon_name_cn, addon_type,
             price_yuan, storage_gb, ai_chat_count, ai_image_count, is_active
      FROM af_subscription_addons
      ORDER BY id
      "#,
    )
  };

  let rows = query.fetch_all(pg_pool).await?;

  Ok(rows
    .into_iter()
    .map(|row| SubscriptionAddonRow {
      id: row.get(0),
      addon_code: row.get(1),
      addon_name: row.get(2),
      addon_name_cn: row.get(3),
      addon_type: row.get(4),
      price_yuan: row.get(5),
      storage_gb: row.get(6),
      ai_chat_count: row.get(7),
      ai_image_count: row.get(8),
      is_active: row.get(9),
    })
    .collect())
}

#[instrument(skip_all, err)]
pub async fn get_subscription_addon(
  pg_pool: &PgPool,
  addon_id: i64,
) -> Result<SubscriptionAddonRow, AppError> {
  let row = sqlx::query(
    r#"
    SELECT id, addon_code, addon_name, addon_name_cn, addon_type,
           price_yuan, storage_gb, ai_chat_count, ai_image_count, is_active
    FROM af_subscription_addons
    WHERE id = $1
    "#,
  )
  .bind(addon_id)
  .fetch_one(pg_pool)
  .await?;

  Ok(SubscriptionAddonRow {
    id: row.get(0),
    addon_code: row.get(1),
    addon_name: row.get(2),
    addon_name_cn: row.get(3),
    addon_type: row.get(4),
    price_yuan: row.get(5),
    storage_gb: row.get(6),
    ai_chat_count: row.get(7),
    ai_image_count: row.get(8),
    is_active: row.get(9),
  })
}

#[instrument(skip_all, err)]
pub async fn insert_user_addon(
  pg_pool: &PgPool,
  uid: i64,
  addon: &SubscriptionAddonRow,
  quantity: i32,
  start_date: DateTime<Utc>,
  end_date: DateTime<Utc>,
) -> Result<UserAddonRow, AppError> {
  let row = sqlx::query(
    r#"
    INSERT INTO af_user_addons (uid, addon_id, quantity, start_date, end_date, status)
    VALUES ($1, $2, $3, $4, $5, 'active')
    RETURNING id, uid, addon_id, quantity, start_date, end_date, status
    "#,
  )
  .bind(uid)
  .bind(addon.id)
  .bind(quantity)
  .bind(start_date)
  .bind(end_date)
  .fetch_one(pg_pool)
  .await?;

  Ok(UserAddonRow {
    id: row.get(0),
    uid: row.get(1),
    addon_id: row.get(2),
    addon_code: addon.addon_code.clone(),
    addon_name_cn: addon.addon_name_cn.clone(),
    addon_type: addon.addon_type.clone(),
    quantity: row.get(3),
    price_yuan: addon.price_yuan,
    storage_gb: addon.storage_gb,
    ai_chat_count: addon.ai_chat_count,
    ai_image_count: addon.ai_image_count,
    start_date: row.get(4),
    end_date: row.get(5),
    status: row.get(6),
  })
}

#[instrument(skip_all, err)]
pub async fn list_user_addons(
  pg_pool: &PgPool,
  uid: i64,
  status: Option<&str>,
) -> Result<Vec<UserAddonRow>, AppError> {
  let query = if let Some(s) = status {
    sqlx::query(
      r#"
      SELECT ua.id, ua.uid, ua.addon_id, sa.addon_code, sa.addon_name_cn, sa.addon_type,
             ua.quantity, sa.price_yuan, sa.storage_gb, sa.ai_chat_count, sa.ai_image_count,
             ua.start_date, ua.end_date, ua.status
      FROM af_user_addons ua
      JOIN af_subscription_addons sa ON ua.addon_id = sa.id
      WHERE ua.uid = $1 AND ua.status = $2
      ORDER BY ua.id DESC
      "#,
    )
    .bind(uid)
    .bind(s)
  } else {
    sqlx::query(
      r#"
      SELECT ua.id, ua.uid, ua.addon_id, sa.addon_code, sa.addon_name_cn, sa.addon_type,
             ua.quantity, sa.price_yuan, sa.storage_gb, sa.ai_chat_count, sa.ai_image_count,
             ua.start_date, ua.end_date, ua.status
      FROM af_user_addons ua
      JOIN af_subscription_addons sa ON ua.addon_id = sa.id
      WHERE ua.uid = $1
      ORDER BY ua.id DESC
      "#,
    )
    .bind(uid)
  };

  let rows = query.fetch_all(pg_pool).await?;

  Ok(rows
    .into_iter()
    .map(|row| UserAddonRow {
      id: row.get(0),
      uid: row.get(1),
      addon_id: row.get(2),
      addon_code: row.get(3),
      addon_name_cn: row.get(4),
      addon_type: row.get(5),
      quantity: row.get(6),
      price_yuan: row.get(7),
      storage_gb: row.get(8),
      ai_chat_count: row.get(9),
      ai_image_count: row.get(10),
      start_date: row.get(11),
      end_date: row.get(12),
      status: row.get(13),
    })
    .collect())
}

// Usage
#[instrument(skip_all, err)]
pub async fn aggregate_user_usage(
  pg_pool: &PgPool,
  uid: i64,
  start_date: NaiveDate,
  end_date: NaiveDate,
) -> Result<Vec<UsageAggregateRow>, AppError> {
  let rows = sqlx::query(
    r#"
    SELECT usage_type, SUM(usage_count)::BIGINT as total
    FROM af_user_subscription_usage
    WHERE uid = $1 AND usage_date >= $2 AND usage_date <= $3
    GROUP BY usage_type
    "#,
  )
  .bind(uid)
  .bind(start_date)
  .bind(end_date)
  .fetch_all(pg_pool)
  .await?;

  Ok(rows
    .into_iter()
    .map(|row| UsageAggregateRow {
      usage_type: row.get(0),
      total: row.get(1),
    })
    .collect())
}

#[instrument(skip_all, err)]
pub async fn list_daily_usage(
  pg_pool: &PgPool,
  uid: i64,
  start_date: NaiveDate,
  end_date: NaiveDate,
) -> Result<Vec<DailyUsageRow>, AppError> {
  let rows = sqlx::query(
    r#"
    SELECT
      usage_date,
      COALESCE(SUM(CASE WHEN usage_type = 'ai_chat' THEN usage_count ELSE 0 END), 0) as ai_chat_count,
      COALESCE(SUM(CASE WHEN usage_type = 'ai_image' THEN usage_count ELSE 0 END), 0) as ai_image_count,
      COALESCE(SUM(CASE WHEN usage_type = 'storage_bytes' THEN usage_count ELSE 0 END), 0) as storage_bytes
    FROM af_user_subscription_usage
    WHERE uid = $1 AND usage_date >= $2 AND usage_date <= $3
    GROUP BY usage_date
    ORDER BY usage_date DESC
    "#,
  )
  .bind(uid)
  .bind(start_date)
  .bind(end_date)
  .fetch_all(pg_pool)
  .await?;

  Ok(rows
    .into_iter()
    .map(|row| DailyUsageRow {
      usage_date: row.get(0),
      ai_chat_count: row.get(1),
      ai_image_count: row.get(2),
      storage_bytes: row.get(3),
    })
    .collect())
}

#[instrument(skip_all, err)]
pub async fn upsert_usage_record(
  pg_pool: &PgPool,
  uid: i64,
  usage_type: &str,
  usage_date: NaiveDate,
  usage_count: i64,
) -> Result<(), AppError> {
  sqlx::query(
    r#"
    INSERT INTO af_user_subscription_usage (uid, usage_date, usage_type, usage_count)
    VALUES ($1, $2, $3, $4)
    ON CONFLICT (uid, usage_date, usage_type)
    DO UPDATE SET usage_count = af_user_subscription_usage.usage_count + $4
    "#,
  )
  .bind(uid)
  .bind(usage_date)
  .bind(usage_type)
  .bind(usage_count)
  .execute(pg_pool)
  .await?;

  Ok(())
}

#[instrument(skip_all, err)]
pub async fn get_user_total_storage_usage(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<i64, AppError> {
  let row: (Option<i64>,) = sqlx::query_as(
    r#"
    SELECT SUM(file_size)::BIGINT
    FROM af_blob_metadata
    WHERE workspace_id IN (
      SELECT workspace_id FROM af_workspace WHERE owner_uid = $1
    )
    "#,
  )
  .bind(uid)
  .fetch_one(pg_pool)
  .await?;

  Ok(row.0.unwrap_or(0))
}

#[instrument(skip_all, err)]
pub async fn get_user_owned_workspace_count(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<i64, AppError> {
  let count: (i64,) = sqlx::query_as(
    r#"
    SELECT COUNT(*)::BIGINT
    FROM af_workspace
    WHERE owner_uid = $1
    "#,
  )
  .bind(uid)
  .fetch_one(pg_pool)
  .await?;

  Ok(count.0)
}

#[instrument(skip_all)]
pub fn calculate_addon_period_end(start_date: DateTime<Utc>) -> DateTime<Utc> {
  // Addons are valid for 1 year from start date
  start_date + chrono::Duration::days(365)
}

