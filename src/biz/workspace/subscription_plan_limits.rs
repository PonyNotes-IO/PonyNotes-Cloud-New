use shared_entity::dto::billing_dto::SubscriptionPlan;

/// Plan limits configuration for each subscription tier
#[derive(Debug, Clone)]
pub struct PlanLimits {
  /// Maximum number of members allowed in the workspace
  pub member_limit: i64,
  /// Maximum storage in bytes
  pub storage_bytes_limit: i64,
  /// Maximum AI responses per month
  pub ai_responses_limit: i64,
  /// Maximum single file upload size in bytes
  pub single_upload_limit: i64,
  /// Whether storage is unlimited
  pub storage_unlimited: bool,
  /// Whether AI responses are unlimited
  pub ai_unlimited: bool,
}

impl PlanLimits {
  /// Get the limits for a given subscription plan
  pub fn from_plan(plan: &SubscriptionPlan) -> Self {
    match plan {
      SubscriptionPlan::Free => PlanLimits {
        member_limit: i64::MAX, // Local use: unlimited members
        storage_bytes_limit: 0,
        ai_responses_limit: 10, // Free users: 10 AI responses per month
        single_upload_limit: 0,
        storage_unlimited: true,
        ai_unlimited: false,
      },
      SubscriptionPlan::Basic => PlanLimits {
        member_limit: 2,
        storage_bytes_limit: 2 * 1024 * 1024 * 1024, // 2GB
        ai_responses_limit: 10,
        single_upload_limit: 5 * 1024 * 1024, // 5MB
        storage_unlimited: false,
        ai_unlimited: false,
      },
      SubscriptionPlan::Pro => PlanLimits {
        member_limit: 5,
        storage_bytes_limit: 10 * 1024 * 1024 * 1024, // 10GB
        ai_responses_limit: 40,
        single_upload_limit: 10 * 1024 * 1024, // 10MB
        storage_unlimited: false,
        ai_unlimited: false,
      },
      SubscriptionPlan::Team => PlanLimits {
        member_limit: 10,
        storage_bytes_limit: 20 * 1024 * 1024 * 1024, // 20GB
        ai_responses_limit: 120,
        single_upload_limit: 20 * 1024 * 1024, // 20MB
        storage_unlimited: false,
        ai_unlimited: false,
      },
      SubscriptionPlan::AiMax => {
        // AI Max: unlimited AI, but inherits storage/member limits from Pro
        let mut limits = PlanLimits::from_plan(&SubscriptionPlan::Pro);
        limits.ai_responses_limit = i64::MAX;
        limits.ai_unlimited = true;
        limits
      },
      SubscriptionPlan::AiLocal => {
        // AI Local: uses local AI, inherits other limits from Pro
        let mut limits = PlanLimits::from_plan(&SubscriptionPlan::Pro);
        limits.ai_responses_limit = i64::MAX;
        limits.ai_unlimited = true;
        limits
      },
    }
  }

  /// Check if adding new members would exceed the limit
  pub fn can_add_members(&self, current_count: i64, members_to_add: i64) -> bool {
    current_count + members_to_add <= self.member_limit
  }

  /// Check if adding storage would exceed the limit
  pub fn can_add_storage(&self, current_bytes: i64, bytes_to_add: i64) -> bool {
    if self.storage_unlimited {
      return true;
    }
    current_bytes + bytes_to_add <= self.storage_bytes_limit
  }

  /// Check if a single file upload is within the limit
  pub fn can_upload_file(&self, file_size: i64) -> bool {
    file_size <= self.single_upload_limit
  }

  /// Check if AI responses are available
  pub fn can_use_ai(&self, current_count: i64) -> bool {
    if self.ai_unlimited {
      return true;
    }
    current_count < self.ai_responses_limit
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_free_plan_limits() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Free);
    assert_eq!(limits.member_limit, i64::MAX);
    assert!(limits.storage_unlimited);
    assert_eq!(limits.ai_responses_limit, 0);
  }

  #[test]
  fn test_basic_plan_limits() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Basic);
    assert_eq!(limits.member_limit, 2);
    assert_eq!(limits.storage_bytes_limit, 2 * 1024 * 1024 * 1024);
    assert_eq!(limits.ai_responses_limit, 10);
    assert_eq!(limits.single_upload_limit, 5 * 1024 * 1024);
  }

  #[test]
  fn test_pro_plan_limits() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Pro);
    assert_eq!(limits.member_limit, 5);
    assert_eq!(limits.storage_bytes_limit, 10 * 1024 * 1024 * 1024);
    assert_eq!(limits.ai_responses_limit, 40);
    assert_eq!(limits.single_upload_limit, 10 * 1024 * 1024);
  }

  #[test]
  fn test_team_plan_limits() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Team);
    assert_eq!(limits.member_limit, 10);
    assert_eq!(limits.storage_bytes_limit, 20 * 1024 * 1024 * 1024);
    assert_eq!(limits.ai_responses_limit, 120);
    assert_eq!(limits.single_upload_limit, 20 * 1024 * 1024);
  }

  #[test]
  fn test_ai_max_unlimited() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::AiMax);
    assert!(limits.ai_unlimited);
    assert_eq!(limits.ai_responses_limit, i64::MAX);
    // Should inherit Pro's other limits
    assert_eq!(limits.member_limit, 5);
  }

  #[test]
  fn test_can_add_members() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Basic);
    assert!(limits.can_add_members(1, 1)); // 1 + 1 = 2, within limit
    assert!(!limits.can_add_members(2, 1)); // 2 + 1 = 3, exceeds limit
  }

  #[test]
  fn test_can_add_storage() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Basic);
    let gb = 1024 * 1024 * 1024;
    assert!(limits.can_add_storage(gb, gb)); // 1GB + 1GB = 2GB, within limit
    assert!(!limits.can_add_storage(gb, 2 * gb)); // 1GB + 2GB = 3GB, exceeds limit
  }

  #[test]
  fn test_can_upload_file() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Basic);
    let mb = 1024 * 1024;
    assert!(limits.can_upload_file(4 * mb)); // 4MB, within 5MB limit
    assert!(!limits.can_upload_file(6 * mb)); // 6MB, exceeds 5MB limit
  }

  #[test]
  fn test_can_use_ai() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::Basic);
    assert!(limits.can_use_ai(5)); // 5 < 10, within limit
    assert!(!limits.can_use_ai(10)); // 10 >= 10, at limit
  }

  #[test]
  fn test_ai_max_unlimited_ai() {
    let limits = PlanLimits::from_plan(&SubscriptionPlan::AiMax);
    assert!(limits.can_use_ai(1000000)); // Always true for unlimited AI
  }
}

