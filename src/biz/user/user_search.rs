use app_error::AppError;
use database::pg_row::AFUserRow;

pub async fn search_users_by_email(
  pg_pool: &sqlx::PgPool,
  q: &str,
  page_no: usize,
) -> Result<Vec<AFUserRow>, AppError> {
  const PAGE_SIZE: usize = 10;
  // 计算偏移量
  let offset = (page_no.saturating_sub(1)) * PAGE_SIZE;

  // 查询
  let users: Vec<AFUserRow> = sqlx::query_as::<_, AFUserRow>(
    "SELECT * FROM af_user WHERE email LIKE $1 LIMIT $2 OFFSET $3",
  )
  .bind(format!("{}%", q))
  .bind(PAGE_SIZE as i64)
  .bind(offset as i64)
  .fetch_all(pg_pool)
  .await
  .map_err(|e| AppError::DBError(e.to_string()))?;

  Ok(users)
}

/// 判断输入是邮箱还是手机号
/// 返回 "email" 或 "phone"
fn detect_identifier_type(identifier: &str) -> Result<&'static str, AppError> {
  // 简单判断：包含@符号的是邮箱
  if identifier.contains('@') {
    // 进一步验证邮箱格式
    if is_valid_email_format(identifier) {
      return Ok("email");
    }
    return Err(AppError::InvalidRequest("Invalid email format".to_string()));
  }

  // 否则认为是手机号（可以进一步验证格式）
  if is_valid_phone_format(identifier) {
    return Ok("phone");
  }
  Err(AppError::InvalidRequest("Invalid phone format".to_string()))
}

/// 简单的邮箱格式验证
fn is_valid_email_format(email: &str) -> bool {
  let parts: Vec<&str> = email.split('@').collect();
  if parts.len() != 2 {
    return false;
  }
  let domain = parts[1];
  domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// 简单的手机号格式验证（支持E.164格式，如+8613800138000）
fn is_valid_phone_format(phone: &str) -> bool {
  let cleaned = phone.trim().trim_start_matches('+');
  // 基本验证：至少10位数字
  cleaned.len() >= 10 && cleaned.chars().all(|c| c.is_ascii_digit())
}

/// 通过邮箱或手机号查询用户uid
pub async fn get_uid_by_email_or_phone(
  pg_pool: &sqlx::PgPool,
  identifier: &str,
) -> Result<(i64, String), AppError> {
  let identifier_type = detect_identifier_type(identifier)?;

  let uid = match identifier_type {
    "email" => database::user::select_uid_from_email(pg_pool, identifier).await?,
    "phone" => database::user::select_uid_from_phone(pg_pool, identifier).await?,
    _ => {
      return Err(AppError::InvalidRequest(
        "Invalid identifier type".to_string(),
      ));
    },
  };

  Ok((uid, identifier_type.to_string()))
}
