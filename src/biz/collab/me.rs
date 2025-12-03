use sqlx::PgPool;
use app_error::AppError;
use database::collab::{select_received_collab_list, select_send_collab_list};
use database::pg_row::AFCollabMemberInvite;

pub async fn get_send_collab_list(
  pg_pool: &PgPool,
  uid: i64,
) -> Result<Vec<AFCollabMemberInvite>, AppError> {
    select_send_collab_list(pg_pool, uid).await
}

pub async fn get_received_collab_list(
    pg_pool: &PgPool,
    uid: i64,
) -> Result<Vec<AFCollabMemberInvite>, AppError> {
    select_received_collab_list(pg_pool, uid).await
}