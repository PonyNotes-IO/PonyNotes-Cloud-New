use app_error::AppError;
use fancy_regex::Regex;
use lazy_static::lazy_static;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub struct UserPassword(pub String);

impl UserPassword {
  pub fn parse(s: String) -> Result<UserPassword, AppError> {
    if s.trim().is_empty() {
      return Err(AppError::InvalidRequest("Password is empty".to_string()));
    }

    if s.graphemes(true).count() > 100 {
      return Err(AppError::InvalidRequest(
        "Password is too long".to_string(),
      ));
    }

    let forbidden_characters = ['/', '(', ')', '"', '<', '>', '\\', '{', '}'];
    let contains_forbidden_characters = s.chars().any(|g| forbidden_characters.contains(&g));
    if contains_forbidden_characters {
      return Err(AppError::InvalidRequest(
        "Password contains forbidden characters".to_string(),
      ));
    }

    if !validate_password(&s) {
      return Err(AppError::InvalidRequest(
        "Password format is invalid".to_string(),
      ));
    }

    Ok(Self(s))
  }
}

impl AsRef<str> for UserPassword {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

lazy_static! {
    // Test it in https://regex101.com/
    // https://stackoverflow.com/questions/2370015/regular-expression-for-password-validation/2370045
    // Hell1!
    // [invalid, greater or equal to 6]
    // Hel1!
    //
    // Hello1!
    // [invalid, must include number]
    // Hello!
    //
    // Hello12!
    // [invalid must include upper case]
    // hello12!
    static ref PASSWORD: Regex = Regex::new("((?=.*\\d)(?=.*[a-z])(?=.*[A-Z])(?=.*[\\W]).{6,20})").unwrap();
}

pub fn validate_password(password: &str) -> bool {
  match PASSWORD.is_match(password) {
    Ok(is_match) => is_match,
    Err(e) => {
      tracing::error!("validate_password fail: {:?}", e);
      false
    },
  }
}

/// 检查用户是否真正设置了密码
/// 
/// 使用 GoTrue 的 password_is_set 字段判断
/// - true: 用户主动设置了密码，可以使用密码登录
/// - false: 系统自动生成的密码（OTP/Magic Link 登录），用户不知道密码
#[instrument(skip_all, err)]
pub async fn check_user_has_password(
  user_uuid: &Uuid,
  pg_pool: &PgPool,
) -> Result<bool, AppError> {
  // 使用 query_scalar (不带!) 以避免编译时SQL验证，适配Docker离线构建
  let password_is_set: Option<bool> = sqlx::query_scalar(
    "SELECT password_is_set FROM auth.users WHERE id = $1"
  )
  .bind(user_uuid)
  .fetch_one(pg_pool)
  .await?;
  
  Ok(password_is_set.unwrap_or(false))
}
