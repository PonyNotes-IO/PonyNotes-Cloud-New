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
