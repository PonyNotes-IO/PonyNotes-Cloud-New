use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingType {
  Monthly,
  Yearly,
}

impl BillingType {
  pub fn as_str(&self) -> &str {
    match self {
      BillingType::Monthly => "monthly",
      BillingType::Yearly => "yearly",
    }
  }

  pub fn months(&self) -> u32 {
    match self {
      BillingType::Monthly => 1,
      BillingType::Yearly => 12,
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
  Active,
  Canceled,
  Expired,
  Pending,
}

impl SubscriptionStatus {
  pub fn as_str(&self) -> &str {
    match self {
      SubscriptionStatus::Active => "active",
      SubscriptionStatus::Canceled => "canceled",
      SubscriptionStatus::Expired => "expired",
      SubscriptionStatus::Pending => "pending",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonType {
  Storage,
  AiToken,
}

impl AddonType {
  pub fn as_str(&self) -> &str {
    match self {
      AddonType::Storage => "storage",
      AddonType::AiToken => "ai_token",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonStatus {
  Active,
  Expired,
  Used,
}

impl AddonStatus {
  pub fn as_str(&self) -> &str {
    match self {
      AddonStatus::Active => "active",
      AddonStatus::Expired => "expired",
      AddonStatus::Used => "used",
    }
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageType {
  AiChat,
  AiImage,
  StorageBytes,
}

// Request DTOs
#[derive(Debug, Serialize, Deserialize)]
pub struct SubscribeRequest {
  pub plan_id: i64,
  pub billing_type: BillingType,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelSubscriptionRequest {
  pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PurchaseAddonRequest {
  pub addon_id: i64,
  pub quantity: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UsageRecordRequest {
  pub usage_type: UsageType,
  pub usage_count: i64,
  pub usage_date: Option<NaiveDate>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionUsageQuery {
  pub start_date: Option<NaiveDate>,
  pub end_date: Option<NaiveDate>,
}

// Plan and Addon Info DTOs
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscriptionPlanInfo {
  pub id: i64,
  pub plan_code: String,
  pub plan_name: String,
  pub plan_name_cn: String,
  pub monthly_price_yuan: f64,
  pub yearly_price_yuan: f64,
  pub cloud_storage_gb: i32,
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscriptionAddonInfo {
  pub id: i64,
  pub addon_code: String,
  pub addon_name: String,
  pub addon_name_cn: String,
  pub addon_type: AddonType,
  pub price_yuan: f64,
  pub storage_gb: Option<i32>,
  pub ai_chat_count: Option<i32>,
  pub ai_image_count: Option<i32>,
  pub is_active: bool,
}

// User Subscription and Addon Record DTOs
#[derive(Debug, Serialize, Deserialize)]
pub struct UserSubscriptionRecord {
  pub id: i64,
  pub plan_id: i64,
  pub plan_code: String,
  pub plan_name_cn: String,
  pub billing_type: BillingType,
  pub status: SubscriptionStatus,
  pub start_date: DateTime<Utc>,
  pub end_date: DateTime<Utc>,
  pub canceled_at: Option<DateTime<Utc>>,
  pub cancel_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserAddonRecord {
  pub id: i64,
  pub addon_id: i64,
  pub addon_code: String,
  pub addon_name_cn: String,
  pub addon_type: AddonType,
  pub quantity: i32,
  pub price_yuan: f64,
  pub storage_gb: Option<i32>,
  pub ai_chat_count: Option<i32>,
  pub ai_image_count: Option<i32>,
  pub start_date: DateTime<Utc>,
  pub end_date: DateTime<Utc>,
  pub status: AddonStatus,
}

// Current Subscription Response
#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionCurrentResponse {
  pub subscription: UserSubscriptionRecord,
  pub plan_details: SubscriptionPlanInfo,
  pub usage: SubscriptionCurrentUsage,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionCurrentUsage {
  pub ai_chat_used_this_month: i64,
  pub ai_chat_remaining_this_month: Option<i64>,
  pub ai_image_used_this_month: i64,
  pub ai_image_remaining_this_month: Option<i64>,
  pub storage_used_gb: f64,
  pub storage_total_gb: Option<f64>,
}

// Usage Response DTOs
#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionUsageResponse {
  pub subscription_limits: SubscriptionUsageLimits,
  pub current_usage: SubscriptionUsageMetrics,
  pub remaining: SubscriptionUsageRemaining,
  pub addon_usage: SubscriptionAddonUsage,
  pub daily_usage: Vec<SubscriptionDailyUsage>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionUsageLimits {
  pub ai_chat_count_per_month: Option<i64>,
  pub ai_image_generation_per_month: Option<i64>,
  pub cloud_storage_gb: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionUsageMetrics {
  pub ai_chat_used_this_month: i64,
  pub ai_image_used_this_month: i64,
  pub storage_used_gb: f64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionUsageRemaining {
  pub ai_chat_remaining_this_month: Option<i64>,
  pub ai_image_remaining_this_month: Option<i64>,
  pub storage_remaining_gb: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionAddonUsage {
  pub storage_addon_total_gb: f64,
  pub ai_token_addon_chat_count: i64,
  pub ai_token_addon_image_count: i64,
  pub ai_token_addon_chat_used: i64,
  pub ai_token_addon_image_used: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SubscriptionDailyUsage {
  pub date: NaiveDate,
  pub ai_chat_count: i64,
  pub ai_image_count: i64,
  pub storage_bytes: i64,
}

