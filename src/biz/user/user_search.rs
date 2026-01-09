use app_error::AppError;
use database::pg_row::AFUserRow;

/// 搜索用户（支持邮箱和手机号）
/// 根据输入自动判断搜索类型：
/// - 包含 @ 符号：搜索邮箱
/// - 纯数字或以 + 开头：搜索手机号
/// - 其他情况：同时搜索邮箱和手机号
pub async fn search_users_by_email(
  pg_pool: &sqlx::PgPool,
  q: &str,
  page_no: usize,
) -> Result<Vec<AFUserRow>, AppError> {
  const PAGE_SIZE: usize = 10;
  // 计算偏移量
  let offset = (page_no.saturating_sub(1)) * PAGE_SIZE;

  let search_pattern = format!("{}%", q);
  
  // 判断搜索类型
  let is_email = q.contains('@');
  let is_phone = {
    let cleaned = q.trim().trim_start_matches('+');
    !cleaned.is_empty() && cleaned.chars().all(|c| c.is_ascii_digit())
  };

  // 根据输入类型选择不同的查询策略
  let users: Vec<AFUserRow> = if is_email {
    // 包含 @ 符号，只搜索邮箱
    sqlx::query_as::<_, AFUserRow>(
      "SELECT * FROM af_user WHERE email LIKE $1 AND deleted_at IS NULL LIMIT $2 OFFSET $3",
    )
    .bind(&search_pattern)
    .bind(PAGE_SIZE as i64)
    .bind(offset as i64)
    .fetch_all(pg_pool)
    .await
    .map_err(|e| AppError::DBError(e.to_string()))?
  } else if is_phone {
    // 纯数字，搜索手机号（支持多种格式匹配）
    // 尝试匹配原始输入、带+86前缀、不带前缀等
    let phone_patterns = generate_phone_patterns(q);
    
    let mut all_users = Vec::new();
    for pattern in phone_patterns {
      let pattern_with_wildcard = format!("{}%", pattern);
      let found: Vec<AFUserRow> = sqlx::query_as::<_, AFUserRow>(
        "SELECT * FROM af_user WHERE phone LIKE $1 AND deleted_at IS NULL LIMIT $2 OFFSET $3",
      )
      .bind(&pattern_with_wildcard)
      .bind(PAGE_SIZE as i64)
      .bind(offset as i64)
      .fetch_all(pg_pool)
      .await
      .map_err(|e| AppError::DBError(e.to_string()))?;
      
      for user in found {
        // 避免重复添加
        if !all_users.iter().any(|u: &AFUserRow| u.uid == user.uid) {
          all_users.push(user);
        }
      }
      
      // 找到结果就停止
      if !all_users.is_empty() {
        break;
      }
    }
    all_users
  } else {
    // 其他情况，同时搜索邮箱和手机号
    sqlx::query_as::<_, AFUserRow>(
      "SELECT * FROM af_user WHERE (email LIKE $1 OR phone LIKE $1) AND deleted_at IS NULL LIMIT $2 OFFSET $3",
    )
    .bind(&search_pattern)
    .bind(PAGE_SIZE as i64)
    .bind(offset as i64)
    .fetch_all(pg_pool)
    .await
    .map_err(|e| AppError::DBError(e.to_string()))?
  };

  Ok(users)
}

/// 生成手机号搜索模式（处理不同格式）
fn generate_phone_patterns(phone: &str) -> Vec<String> {
  let mut patterns = Vec::new();
  
  // 清理输入：移除所有非数字字符（保留开头的+号信息）
  let has_plus = phone.starts_with('+');
  let digits: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
  
  if digits.is_empty() {
    return patterns;
  }
  
  // 原始数字
  patterns.push(digits.clone());
  
  // 如果是11位国内手机号，尝试添加86前缀
  if digits.len() == 11 && !digits.starts_with("86") {
    patterns.push(format!("86{}", digits));
    patterns.push(format!("+86{}", digits));
  }
  
  // 如果以86开头且长度为13位，也尝试不带86的版本
  if digits.starts_with("86") && digits.len() == 13 {
    patterns.push(digits[2..].to_string());
  }
  
  // 如果原始输入有+号，添加带+的版本
  if has_plus && !patterns.iter().any(|p| p.starts_with('+')) {
    patterns.push(format!("+{}", digits));
  }
  
  patterns
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
