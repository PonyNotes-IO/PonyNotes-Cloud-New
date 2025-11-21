use anyhow::{Context, Result};
use sqlx::types::uuid;
use std::ops::DerefMut;
use std::time::Instant;
use tracing::{event, instrument, trace};

use app_error::AppError;
use database::user::{create_user, is_user_exist};
use database::workspace::select_workspace;
use database_entity::dto::AFRole;
use workspace_template::document::getting_started::GettingStartedTemplate;

use crate::biz::user::user_init::initialize_workspace_for_user;
use crate::state::AppState;

/// Verify the token from the gotrue server and create the user if it is a new user
/// Return true if the user is a new user
///
#[instrument(skip_all, err)]
pub async fn verify_token(access_token: &str, state: &AppState) -> Result<bool, AppError> {
  let user = state.gotrue_client.user_info(access_token).await?;
  let user_uuid = uuid::Uuid::parse_str(&user.id)?;
  let name = name_from_user_metadata(&user.user_metadata);

  // Create new user if it doesn't exist
  let mut txn = state
    .pg_pool
    .begin()
    .await
    .context("acquire transaction to verify token")?;

  let is_new = !is_user_exist(txn.deref_mut(), &user_uuid).await?;
  if is_new {
    let new_uid = state.id_gen.write().await.next_id();
    event!(tracing::Level::INFO, "create new user:{}", new_uid);
    
    // Use phone number if email is empty (for phone-based authentication)
    let email_or_phone = if user.email.is_empty() {
      &user.phone
    } else {
      &user.email
    };
    
    let workspace_id =
      create_user(txn.deref_mut(), new_uid, &user_uuid, email_or_phone, &name).await?;
    let workspace_row = select_workspace(txn.deref_mut(), &workspace_id).await?;

    // It's essential to cache the user's role because subsequent actions will rely on this cached information.
    state
      .workspace_access_control
      .insert_role(&new_uid, &workspace_id, AFRole::Owner)
      .await?;
    // Need to commit the transaction for the record in `af_user` to be inserted
    // so that `initialize_workspace_for_user` will be able to find the user
    txn
      .commit()
      .await
      .context("fail to commit transaction to verify token")?;

    // Create a workspace with the GetStarted template
    let mut txn2 = state.pg_pool.begin().await?;
    let start = Instant::now();
    initialize_workspace_for_user(
      new_uid,
      &user_uuid,
      &workspace_row,
      &mut txn2,
      vec![GettingStartedTemplate],
      &state.collab_storage,
    )
    .await?;
    txn2
      .commit()
      .await
      .context("fail to commit transaction to initialize workspace")?;
    state.metrics.collab_metrics.observe_pg_tx(start.elapsed());
  } else {
    trace!("user already exists:{},{}", user.id, user.email);
    // For existing users, ensure their workspace roles are cached in Casbin
    // This is important after server restarts or when Casbin cache is cleared
    use database::user::select_uid_from_uuid;
    use database::workspace::select_user_workspace_ids;
    
    let uid = select_uid_from_uuid(txn.deref_mut(), &user_uuid).await?;
    let workspace_ids = select_user_workspace_ids(txn.deref_mut(), &user_uuid).await?;
    
    // Commit the transaction before caching roles
    txn
      .commit()
      .await
      .context("fail to commit transaction to verify token")?;
    
    // Cache all workspace roles for this user
    for (workspace_id, role) in workspace_ids {
      if let Err(e) = state
        .workspace_access_control
        .insert_role(&uid, &workspace_id, role)
        .await
      {
        // Log error but don't fail the login process
        event!(
          tracing::Level::WARN,
          "Failed to cache role for user {} in workspace {}: {}",
          uid,
          workspace_id,
          e
        );
      }
    }
  }

  Ok(is_new)
}

// Best effort to get user's name after oauth
fn name_from_user_metadata(value: &serde_json::Value) -> String {
  value
    .get("name")
    .or(value.get("full_name"))
    .or(value.get("nickname"))
    .and_then(serde_json::Value::as_str)
    .map(str::to_string)
    .unwrap_or_default()
}

/// Verify phone OTP and bind phone number to user
/// 
/// This function:
/// 1. Calls GoTrue's verify endpoint to validate the OTP
/// 2. If validation succeeds, updates the user's phone number in the database
#[instrument(skip(state), err)]
pub async fn verify_and_bind_phone(
  user_uuid: &uuid::Uuid,
  phone: &str,
  otp: &str,
  state: &AppState,
) -> Result<(), AppError> {
  use database::user::update_user;
  use gotrue::params::VerifyParams;
  
  // Step 1: Verify the OTP using GoTrue's verify endpoint
  // This validates that the user has access to this phone number
  let verify_params = VerifyParams {
    phone: phone.to_string(),
    token: otp.to_string(),
    ..Default::default()
  };
  
  let verify_result = state
    .gotrue_client
    .verify(&verify_params)
    .await;
  
  match verify_result {
    Ok(_token_response) => {
      event!(
        tracing::Level::INFO,
        "Phone OTP verified successfully for user: {}, phone: {}",
        user_uuid,
        phone
      );
      
      // Step 2: Update the user's phone number in the database
      update_user(&state.pg_pool, user_uuid, None, None, Some(phone.to_string()), None).await?;
      
      event!(
        tracing::Level::INFO,
        "Phone number bound successfully for user: {}, phone: {}",
        user_uuid,
        phone
      );
      
      Ok(())
    }
    Err(e) => {
      event!(
        tracing::Level::WARN,
        "Phone OTP verification failed for user: {}, phone: {}, error: {}",
        user_uuid,
        phone,
        e
      );
      Err(AppError::InvalidRequest(format!(
        "验证码错误或已过期: {}",
        e
      )))
    }
  }
}
