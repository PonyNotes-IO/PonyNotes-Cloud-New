use actix_web::{
  web::{self, Data, Json, Path},
  Result, Scope,
};
use chrono::Utc;
use uuid::Uuid;

use crate::biz::authentication::jwt::UserUuid;
use crate::state::AppState;
use shared_entity::dto::billing_dto::{
  RecurringInterval, SubscriptionPlan, SubscriptionStatus, WorkspaceSubscriptionStatus,
};
use shared_entity::response::{AppResponse, JsonAppResponse};

/// Billing API scope for AppFlowy Cloud compatibility
/// Implements billing endpoints required by the client
pub fn billing_scope() -> Scope {
  web::scope("/billing/api/v1")
    // List all subscription statuses for the current user
    .service(web::resource("/subscription-status").route(web::get().to(list_subscription_status_handler)))
    // Get subscription status for a specific workspace
    .service(web::resource("/subscription-status/{workspace_id}").route(web::get().to(get_workspace_subscription_status_handler)))
}

/// List all subscription statuses for the current user
async fn list_subscription_status_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
) -> Result<JsonAppResponse<Vec<WorkspaceSubscriptionStatus>>> {
  // Get all workspaces for the user
  let workspaces = database::workspace::select_all_user_workspaces(&state.pg_pool, &user_uuid).await?;
  
  let mut statuses = Vec::new();
  for workspace in workspaces {
    let plan = SubscriptionPlan::try_from(workspace.workspace_type as i16)
      .unwrap_or(SubscriptionPlan::Free);
    
    // Calculate current period end (30 days from now for simplicity)
    let current_period_end = Utc::now().timestamp() + 30 * 24 * 60 * 60;
    
    statuses.push(WorkspaceSubscriptionStatus {
      workspace_id: workspace.workspace_id.to_string(),
      workspace_plan: plan,
      recurring_interval: RecurringInterval::Month,
      subscription_status: SubscriptionStatus::Active,
      subscription_quantity: 1,
      cancel_at: None,
      current_period_end,
    });
  }
  
  Ok(Json(AppResponse::Ok().with_data(statuses)))
}

/// Get subscription status for a specific workspace
async fn get_workspace_subscription_status_handler(
  _user_uuid: UserUuid,
  state: Data<AppState>,
  path: Path<String>,
) -> Result<JsonAppResponse<Vec<WorkspaceSubscriptionStatus>>> {
  let workspace_id_str = path.into_inner();
  let workspace_id = Uuid::parse_str(&workspace_id_str)
    .map_err(|_| app_error::AppError::InvalidRequest(format!("Invalid workspace_id: {}", workspace_id_str)))?;
  
  // Get workspace info
  let workspace = database::workspace::select_workspace(&state.pg_pool, &workspace_id).await?;
  
  // Convert workspace type to subscription plan
  let plan = SubscriptionPlan::try_from(workspace.workspace_type as i16)
    .unwrap_or(SubscriptionPlan::Free);
  
  // Calculate current period end (30 days from now for simplicity)
  let current_period_end = Utc::now().timestamp() + 30 * 24 * 60 * 60;
  
  let status = WorkspaceSubscriptionStatus {
    workspace_id: workspace_id_str,
    workspace_plan: plan,
    recurring_interval: RecurringInterval::Month,
    subscription_status: SubscriptionStatus::Active,
    subscription_quantity: 1,
    cancel_at: None,
    current_period_end,
  };
  
  Ok(Json(AppResponse::Ok().with_data(vec![status])))
}

