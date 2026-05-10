use app_error::AppError;
use redis::AsyncCommands;

use crate::state::RedisConnectionManager;

// 同一手机或邮箱，60 秒内最多发送 1 次 OTP
const OTP_WINDOW_SECS: i64 = 60;
const OTP_MAX_PER_WINDOW: u64 = 1;

pub async fn check_phone_otp_rate_limit(
  phone: &str,
  redis: &mut RedisConnectionManager,
) -> Result<(), AppError> {
  let key = format!("otp:phone:{}", phone);
  check_otp_rate_limit(&key, redis, OTP_MAX_PER_WINDOW, OTP_WINDOW_SECS).await
}

pub async fn check_email_otp_rate_limit(
  email: &str,
  redis: &mut RedisConnectionManager,
) -> Result<(), AppError> {
  let key = format!("otp:email:{}", email);
  check_otp_rate_limit(&key, redis, OTP_MAX_PER_WINDOW, OTP_WINDOW_SECS).await
}

async fn check_otp_rate_limit(
  key: &str,
  redis: &mut RedisConnectionManager,
  max_count: u64,
  window_secs: i64,
) -> Result<(), AppError> {
  let count: u64 = redis
    .incr(key, 1u64)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("Redis incr error: {}", e)))?;

  if count == 1 {
    // 第一次请求时设置过期时间，保证 key 不会永久存在
    let _: () = redis
      .expire(key, window_secs)
      .await
      .map_err(|e| AppError::Internal(anyhow::anyhow!("Redis expire error: {}", e)))?;
  }

  if count > max_count {
    return Err(AppError::TooManyRequests(
      "发送过于频繁，请 60 秒后再试".to_string(),
    ));
  }

  Ok(())
}
