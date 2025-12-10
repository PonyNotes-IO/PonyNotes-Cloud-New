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
    
    // Prepare email and phone for user creation
    // Business requirement: phone number cannot be empty, but email can be empty
    let email = if user.email.is_empty() {
      None
    } else {
      Some(user.email.as_str())
    };
    
    // If phone is empty (e.g., WeChat login without phone binding),
    // generate a temporary phone number based on user UUID to satisfy the requirement
    // Format: +86temp{uuid前12位数字} to follow E.164-like format
    let temp_phone: Option<String> = if user.phone.is_empty() {
      event!(
        tracing::Level::INFO,
        "Phone is empty for user {}, generating temporary phone number",
        user_uuid
      );
      // Generate a temporary phone number: +86temp + first 12 digits of UUID (without dashes)
      let uuid_str = user_uuid.to_string().replace("-", "");
      Some(format!("+86temp{}", &uuid_str[..uuid_str.len().min(12)]))
    } else {
      None
    };
    
    // Use actual phone if available, otherwise use temporary phone
    let phone = if !user.phone.is_empty() {
      Some(user.phone.as_str())
    } else if let Some(ref temp) = temp_phone {
      Some(temp.as_str())
    } else {
      None
    };
    
    let workspace_id =
      create_user(txn.deref_mut(), new_uid, &user_uuid, email, phone, &name).await?;
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

/// Verify phone OTP and complete phone number change
/// 
/// This function follows GoTrue's standard phone change flow:
/// 1. User calls send_phone_otp which triggers GoTrue's update_user (sends OTP)
/// 2. User receives OTP and calls this function to verify
/// 3. This function verifies with type=PhoneChange
/// 4. GoTrue automatically updates auth.users.phone upon successful verification
/// 5. We sync the change to our business database (af_user table)
/// 
/// IMPORTANT: GoTrue handles the phone update automatically when verifying with PhoneChange type.
/// We don't need to manually call update_user after verification.
#[instrument(skip(state), err)]
pub async fn verify_and_bind_phone(
  user_uuid: &uuid::Uuid,
  phone: &str,
  otp: &str,
  state: &AppState,
) -> Result<(), AppError> {
  use database::user::update_user;
  use gotrue::params::VerifyParams;
  
  // Use PhoneChange type - this tells GoTrue to complete the phone number change
  // that was initiated by the update_user call in send_phone_otp
  let verify_params = VerifyParams {
    type_: gotrue::params::VerifyType::PhoneChange,
    phone: phone.to_string(),
    token: otp.to_string(),
    email: String::new(),
  };
  
  event!(
    tracing::Level::INFO,
    "Verifying phone change OTP for user: {}, phone: {}",
    user_uuid,
    phone
  );
  
  let verify_result = state
    .gotrue_client
    .verify(&verify_params)
    .await;
  
  match verify_result {
    Ok(_token_response) => {
      event!(
        tracing::Level::INFO,
        "Phone OTP verified successfully, GoTrue has updated auth.users.phone for user: {}, phone: {}",
        user_uuid,
        phone
      );
      
      // Step 2: Sync the phone number to our business database
      // GoTrue has already updated auth.users.phone, now we update af_user.phone
      update_user(&state.pg_pool, user_uuid, None, None, Some(phone.to_string()), None).await?;
      
      event!(
        tracing::Level::INFO,
        "Phone number change completed successfully for user: {}, phone: {}",
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

/// Initiate phone number change by calling GoTrue's update_user
/// 
/// This function follows GoTrue's standard phone change flow:
/// 1. Calls GoTrue's update_user API with the new phone number and channel=sms
/// 2. GoTrue sends an OTP to the new phone number
/// 3. GoTrue stores the new phone in a pending state (new_phone field)
/// 4. User must verify the OTP using verify_and_bind_phone to complete the change
/// 
/// IMPORTANT: This requires the user's access_token to authenticate the request.
#[instrument(skip(state), err)]
pub async fn send_phone_otp(
  access_token: &str,
  phone: &str,
  state: &AppState,
) -> Result<(), AppError> {
  use gotrue_entity::dto::UpdateGotrueUserParams;
  
  event!(
    tracing::Level::INFO,
    "Initiating phone change to: {}",
    phone
  );
  
  // Call GoTrue's update_user API to initiate phone change
  // IMPORTANT: Must set channel to "sms" to trigger SMS sending
  let mut update_params = UpdateGotrueUserParams::new();
  update_params.phone = phone.to_string();
  update_params.channel = "sms".to_string(); // This is critical!
  
  event!(
    tracing::Level::INFO,
    "Calling GoTrue update_user with phone: {}, channel: sms",
    phone
  );
  
  let result = state
    .gotrue_client
    .update_user(access_token, &update_params)
    .await;
  
  match result {
    Ok(_user) => {
      event!(
        tracing::Level::INFO,
        "Phone change initiated successfully, OTP sent to: {}",
        phone
      );
      Ok(())
    }
    Err(e) => {
      event!(
        tracing::Level::WARN,
        "Failed to initiate phone change to: {}, error: {}",
        phone,
        e
      );
      Err(AppError::InvalidRequest(format!(
        "发送验证码失败: {}",
        e
      )))
    }
  }
}
