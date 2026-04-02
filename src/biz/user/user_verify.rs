use anyhow::{Context, Result};
use sqlx::types::uuid;
use std::ops::DerefMut;
use std::time::Instant;
use tracing::{event, instrument, trace};

use app_error::AppError;
use database::user::{create_user, is_user_exist, select_email_from_user_uuid};
use database::workspace::select_workspace;
use database_entity::dto::AFRole;
use workspace_template::document::getting_started::GettingStartedTemplate;

use crate::biz::user::user_init::initialize_workspace_for_user;
use crate::biz::user::user_search::get_uid_by_email_or_phone;
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
    let email = if user.email.is_empty() {
      None
    } else {
      Some(user.email.as_str())
    };
    
    // Determine if this is an SSO user (no email) or email-registered user
    let is_sso_user = user.email.is_empty();
    
    // Only generate temporary phone number for SSO users (e.g., WeChat login without phone binding)
    // Email-registered users can have no phone number, they can bind it later if needed
    // Format: +86temp{uuid前12位数字} to follow E.164-like format
    let temp_phone: Option<String> = if is_sso_user && user.phone.is_empty() {
      event!(
        tracing::Level::INFO,
        "SSO user {} has no phone, generating temporary phone number",
        user_uuid
      );
      // Generate a temporary phone number: +86temp + first 12 digits of UUID (without dashes)
      let uuid_str = user_uuid.to_string().replace("-", "");
      Some(format!("+86temp{}", &uuid_str[..uuid_str.len().min(12)]))
    } else {
      None
    };
    
    // Use actual phone if available, otherwise use temporary phone (only for SSO users)
    let phone = if !user.phone.is_empty() {
      Some(user.phone.as_str())
    } else if let Some(ref temp) = temp_phone {
      // Only use temp phone for SSO users
      Some(temp.as_str())
    } else {
      // Email-registered users can have no phone
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

    // Extract and store social login openid (wechat/douyin) from GoTrue identities
    if let Some(identities) = &user.identities {
      for identity in identities {
        let openid = identity.id.as_str();
        if !openid.is_empty() {
          match identity.provider.as_str() {
            "wechat" => {
              if let Err(e) = database::user::update_wechat_openid(&state.pg_pool, &user_uuid, openid).await {
                event!(tracing::Level::WARN, "Failed to store wechat_openid for user {}: {}", user_uuid, e);
              }
            }
            "douyin" => {
              if let Err(e) = database::user::update_douyin_openid(&state.pg_pool, &user_uuid, openid).await {
                event!(tracing::Level::WARN, "Failed to store douyin_openid for user {}: {}", user_uuid, e);
              }
            }
            _ => {}
          }
        }
      }
    }
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

    // Extract and store social login openid (wechat/douyin) from GoTrue identities
    if let Some(identities) = &user.identities {
      for identity in identities {
        let openid = identity.id.as_str();
        if !openid.is_empty() {
          match identity.provider.as_str() {
            "wechat" => {
              if let Err(e) = database::user::update_wechat_openid(&state.pg_pool, &user_uuid, openid).await {
                event!(tracing::Level::WARN, "Failed to store wechat_openid for user {}: {}", user_uuid, e);
              }
            }
            "douyin" => {
              if let Err(e) = database::user::update_douyin_openid(&state.pg_pool, &user_uuid, openid).await {
                event!(tracing::Level::WARN, "Failed to store douyin_openid for user {}: {}", user_uuid, e);
              }
            }
            _ => {}
          }
        }
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

/// Verify phone OTP and complete phone number change or first-time binding
/// 
/// This function handles two scenarios:
/// 1. First-time phone binding for SSO users (uses /otp endpoint, verify with type=sms)
/// 2. Phone number change for existing users (uses update_user, verify with type=PhoneChange)
/// 
/// Flow for first-time binding:
/// 1. User calls send_phone_otp_for_sso_user which uses /otp endpoint
/// 2. User receives OTP and calls this function to verify
/// 3. This function verifies with type=sms
/// 4. GoTrue automatically updates auth.users.phone upon successful verification
/// 5. We sync the change to our business database (af_user table)
/// 
/// Flow for phone change:
/// 1. User calls send_phone_otp which triggers GoTrue's update_user (sends OTP)
/// 2. User receives OTP and calls this function to verify
/// 3. This function verifies with type=PhoneChange
/// 4. GoTrue automatically updates auth.users.phone upon successful verification
/// 5. We sync the change to our business database (af_user table)
#[instrument(skip(state), err)]
pub async fn verify_and_bind_phone(
  user_uuid: &uuid::Uuid,
  phone: &str,
  otp: &str,
  state: &AppState,
) -> Result<shared_entity::dto::auth_dto::BindPhoneResponse, AppError> {
  use database::user::update_user;
  use gotrue::params::VerifyParams;
  use shared_entity::dto::auth_dto::BindPhoneResponse;

  // Check if this is an old phone (belongs to another user)
  let is_old_phone = database::user::phone_exists_for_another_user(&state.pg_pool, phone, user_uuid)
    .await
    .unwrap_or(false);

  let verify_type = if is_old_phone {
    // OTP was sent via /otp endpoint, verify with type=sms
    gotrue::params::VerifyType::Sms
  } else {
    // OTP was sent via update_user phone change, verify with type=phone_change
    gotrue::params::VerifyType::PhoneChange
  };

  let verify_type_str = match verify_type {
    gotrue::params::VerifyType::Sms => "sms",
    gotrue::params::VerifyType::PhoneChange => "phone_change",
    gotrue::params::VerifyType::MagicLink => "magiclink",
    gotrue::params::VerifyType::Recovery => "recovery",
  };
  event!(
    tracing::Level::INFO,
    "Verifying phone OTP for user: {}, phone: {}, is_old_phone: {}, verification_type: {}",
    user_uuid,
    phone,
    is_old_phone,
    verify_type_str
  );

  // Note: GoTrue's validatePhone will normalize the phone format (remove + prefix)
  // Frontend should ensure consistent format, but GoTrue will handle normalization
  let verify_params = VerifyParams {
    type_: verify_type,
    phone: phone.to_string(),
    token: otp.to_string(),
    email: String::new(),
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

      if is_old_phone {
        // Old phone: only update bind_mobile, NOT phone (can't - UNIQUE constraint, phone belongs to another user)
        event!(
          tracing::Level::INFO,
          "Old phone binding: only updating bind_mobile for user {}, phone: {}",
          user_uuid,
          phone
        );
        database::user::update_bind_mobile(&state.pg_pool, user_uuid, phone).await?;
        event!(
          tracing::Level::INFO,
          "bind_mobile updated successfully for user: {}, phone: {} (two separate accounts remain)",
          user_uuid,
          phone
        );
        Ok(BindPhoneResponse {
          phone_updated: false,
          bind_mobile_updated: true,
        })
      } else {
        // New phone: update both phone and bind_mobile
        event!(
          tracing::Level::INFO,
          "New phone binding: updating both phone and bind_mobile for user {}, phone: {}",
          user_uuid,
          phone
        );
        update_user(&state.pg_pool, user_uuid, None, None, Some(phone.to_string()), None).await?;
        database::user::update_bind_mobile(&state.pg_pool, user_uuid, phone).await?;
        event!(
          tracing::Level::INFO,
          "Phone and bind_mobile updated for user: {}, phone: {} (unified account)",
          user_uuid,
          phone
        );
        Ok(BindPhoneResponse {
          phone_updated: true,
          bind_mobile_updated: true,
        })
      }
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

/// Initiate phone number OTP by calling GoTrue's update_user (new phone) or /otp (old phone).
///
/// For new phones (not yet in af_user.phone for any user): follows GoTrue's standard phone change flow:
/// 1. Calls GoTrue's update_user API with the new phone number and channel=sms
/// 2. GoTrue sends an OTP to the new phone number
/// 3. GoTrue stores the new phone in a pending state (new_phone field)
/// 4. User must verify the OTP using verify_and_bind_phone with type=phone_change
///
/// For old phones (already in af_user.phone for another user): uses GoTrue's /otp endpoint:
/// 1. Calls GoTrue's magic_link (phone /otp) with the phone number
/// 2. GoTrue sends an OTP to the phone number (no uniqueness check needed)
/// 3. User must verify the OTP using verify_and_bind_phone with type=sms
///
/// IMPORTANT: This requires the user's access_token to authenticate the request.
#[instrument(skip(state), err)]
pub async fn send_phone_otp(
  access_token: &str,
  user_uuid: &uuid::Uuid,
  phone: &str,
  state: &AppState,
) -> Result<(), AppError> {
  use gotrue_entity::dto::UpdateGotrueUserParams;

  // Check if this phone belongs to another user (old phone scenario)
  let is_old_phone = database::user::phone_exists_for_another_user(&state.pg_pool, phone, user_uuid)
    .await
    .unwrap_or(false);

  if is_old_phone {
    // Old phone: use GoTrue's /otp endpoint (phone sign-in flow, no uniqueness check)
    // OTP will be verified with type=sms (not phone_change) at verify step
    event!(
      tracing::Level::INFO,
      "Phone {} belongs to another user - using /otp endpoint for OTP (old phone binding)",
      phone
    );

    use gotrue::params::MagicLinkParams;
    let mut otp_params = MagicLinkParams::default();
    otp_params.phone = phone.to_string();

    return state
      .gotrue_client
      .magic_link(&otp_params, None)
      .await
      .map(|_| ())
      .map_err(|e| AppError::InvalidRequest(format!("发送验证码失败: {}", e)));
  }

  event!(
    tracing::Level::INFO,
    "Initiating phone change to: {}",
    phone
  );

  // Call GoTrue's update_user API to initiate phone change
  // IMPORTANT: Must set channel to "sms" to trigger SMS sending
  // Note: GoTrue's validatePhone will normalize the phone format (remove + prefix)
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

      // Check if the error is about phone number already being registered
      let error_msg = e.to_string();
      let friendly_msg = if error_msg.contains("already been registered")
        || error_msg.contains("phone number has already")
        || error_msg.contains("phone exists") {
        format!("该手机号 {} 已被其他用户注册，请使用其他手机号", phone)
      } else {
        format!("发送验证码失败: {}", error_msg)
      };

      Err(AppError::InvalidRequest(friendly_msg))
    }
  }
}

/// Send phone OTP for SSO users (e.g., WeChat login) who need to bind phone for the first time
/// 
/// This function is specifically designed for SSO users who don't have a phone number yet.
/// It uses GoTrue's /otp endpoint or Admin API to send verification code.
/// 
/// IMPORTANT: This requires the user's access_token to authenticate the request.
#[instrument(skip(state), err)]
pub async fn send_phone_otp_for_sso_user(
  access_token: &str,
  phone: &str,
  state: &AppState,
) -> Result<(), AppError> {
  use gotrue::params::{AdminUserParams, MagicLinkParams};
  
  event!(
    tracing::Level::INFO,
    "Sending phone OTP for SSO user (first-time binding) to: {}",
    phone
  );
  
  // Get user info to get user UUID
  let user_info = state
    .gotrue_client
    .user_info(access_token)
    .await
    .map_err(|err| AppError::InvalidRequest(format!(
      "Failed to get user info: {}",
      err
    )))?;
  
  let admin_token = state.gotrue_admin.token().await?;
  
  // Get current user details first
  let current_user = state
    .gotrue_client
    .admin_user_details(&admin_token, &user_info.id)
    .await
    .map_err(|err| AppError::InvalidRequest(format!(
      "Failed to get user details: {}",
      err
    )))?;
  
  // For SSO users, try using /otp endpoint first
  let mut otp_params = MagicLinkParams::default();
  otp_params.phone = phone.to_string();
  
  event!(
    tracing::Level::INFO,
    "Using /otp endpoint to send OTP for SSO user phone: {}",
    phone
  );
  
  let otp_result = state
    .gotrue_client
    .magic_link(&otp_params, None)
    .await;
  
  match otp_result {
    Ok(_) => {
      event!(
        tracing::Level::INFO,
        "OTP sent successfully via /otp endpoint for SSO user phone: {}",
        phone
      );
      Ok(())
    }
    Err(otp_err) => {
      event!(
        tracing::Level::WARN,
        "/otp endpoint failed for SSO user phone: {}, error: {}, trying Admin API",
        phone,
        otp_err
      );
      // Fallback: Try Admin API to set phone (this might trigger OTP sending)
      let mut admin_params = AdminUserParams::default();
      admin_params.aud = current_user.aud;
      admin_params.role = current_user.role;
      admin_params.email = current_user.email;
      admin_params.phone = phone.to_string();
      admin_params.phone_confirm = false;
      admin_params.email_confirm = current_user.email_confirmed_at.is_some();
      
      let admin_result = state
        .gotrue_client
        .admin_update_user(&admin_token, &user_info.id, &admin_params)
        .await;
      
      match admin_result {
        Ok(_) => {
          event!(
            tracing::Level::INFO,
            "Admin API phone update successful for SSO user, OTP should be sent to: {}",
            phone
          );
          Ok(())
        }
        Err(admin_err) => {
          event!(
            tracing::Level::WARN,
            "Both /otp and Admin API failed for SSO user phone: {}, errors: {} / {}",
            phone,
            otp_err,
            admin_err
          );
          Err(AppError::InvalidRequest(format!(
            "发送验证码失败: {} (SSO账户)",
            otp_err
          )))
        }
      }
    }
  }
}

/// Check if an email is already registered by another user
/// Used for email binding/change pre-check before sending verification code
#[instrument(skip(state), err)]
pub async fn check_email_registered(
  user_uuid: &uuid::Uuid,
  email: &str,
  state: &AppState,
) -> Result<CheckEmailRegisteredResult, AppError> {
  // Get current user's email
  let current_email = select_email_from_user_uuid(&state.pg_pool, user_uuid)
    .await?
    .unwrap_or_default();

  // Check if the email belongs to the current user
  if current_email == email {
    return Ok(CheckEmailRegisteredResult {
      email_exists: false,
      is_own_email: true,
      existing_uid: None,
      message: None,
    });
  }

  // Check if email is registered by another user by querying auth.users
  let existing_result = get_uid_by_email_or_phone(
    &state.pg_pool,
    email,
  )
  .await;

  match existing_result {
    Ok((uid, _)) => {
      event!(
        tracing::Level::INFO,
        "CheckEmailRegistered: email {} is registered by uid {}",
        email,
        uid
      );
      Ok(CheckEmailRegisteredResult {
        email_exists: true,
        is_own_email: false,
        existing_uid: Some(uid),
        message: Some("该邮箱已被其他账号注册".to_string()),
      })
    }
    Err(_) => {
      // No user found with this email → email is available
      Ok(CheckEmailRegisteredResult {
        email_exists: false,
        is_own_email: false,
        existing_uid: None,
        message: None,
      })
    }
  }
}

#[derive(Debug)]
pub struct CheckEmailRegisteredResult {
  pub email_exists: bool,
  pub is_own_email: bool,
  pub existing_uid: Option<i64>,
  pub message: Option<String>,
}
