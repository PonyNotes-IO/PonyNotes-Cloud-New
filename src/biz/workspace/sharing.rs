use super::collab_member::notify_collab_permission_changed;
use crate::biz::collab::ops::get_latest_workspace_database;
use crate::state::AppState;
use app_error::AppError;
use appflowy_collaborate::ws2::{PermissionType, PermissionUpdate, UpdateUserPermissions};
use database::collab::GetCollabOrigin;
use database::workspace::select_collab_owner;
use database_entity::dto::AFAccessLevel;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgAdvisoryLock, PgAdvisoryLockGuard};
use sqlx::{Connection, PgConnection, PgPool, Postgres};
use uuid::Uuid;

/// All sharing mutations of linked views serialize on their underlying database.
/// The session lock also covers policy updates on either side of a SQL commit.
pub struct ShareTarget {
  pub workspace_id: Uuid,
  pub view_id: Uuid,
  pub object_id: Uuid,
  pub owner: i64,
  lock: PgAdvisoryLock,
  linked_views: Vec<String>,
}

impl ShareTarget {
  pub async fn load(state: &AppState, workspace_id: Uuid, view_id: Uuid) -> Result<Self, AppError> {
    let view_key = view_id.to_string();
    let (object_id, linked_views) = match get_latest_workspace_database(
      &state.collab_storage,
      &state.pg_pool,
      GetCollabOrigin::Server,
      workspace_id,
    )
    .await
    {
      Ok((_, index)) => match index.get_database_meta_with_view_id(&view_key) {
        Some(meta) => (Uuid::parse_str(&meta.database_id)?, meta.linked_views),
        None => (view_id, Vec::new()),
      },
      // Older workspaces can have documents without a workspace database.
      Err(AppError::RecordNotFound(_)) => (view_id, Vec::new()),
      Err(err) => return Err(err),
    };
    let owner = select_collab_owner(&state.pg_pool, &workspace_id, &object_id).await?;
    Ok(Self {
      workspace_id,
      view_id,
      object_id,
      owner,
      linked_views,
      lock: PgAdvisoryLock::new(format!("collab-sharing/{object_id}")),
    })
  }

  pub async fn lock<'a>(
    &'a self,
    pool: &PgPool,
  ) -> Result<PgAdvisoryLockGuard<'a, PoolConnection<Postgres>>, AppError> {
    Ok(self.lock.acquire(pool.acquire().await?).await?)
  }

  pub async fn require_invite_access(
    &self,
    conn: &mut PgConnection,
    uid: i64,
  ) -> Result<(), AppError> {
    if uid == self.owner {
      return Ok(());
    }
    let allowed: bool = sqlx::query_scalar(
      "SELECT EXISTS(
      SELECT FROM af_collab_member WHERE uid = $1 AND oid = $2 AND permission_id = 4
      UNION ALL SELECT FROM af_workspace WHERE owner_uid = $1 AND workspace_id = $3)",
    )
    .bind(uid)
    .bind(self.view_id.to_string())
    .bind(self.workspace_id)
    .fetch_one(conn)
    .await?;
    if !allowed {
      return Err(AppError::NotEnoughPermissions);
    }
    Ok(())
  }

  pub fn database_id(&self) -> Option<Uuid> {
    (self.view_id != self.object_id).then_some(self.object_id)
  }

  /// A single removal keeps sent-share metadata. Stopping the share also
  /// invalidates its link and removes all invitations. Both keep the recipient
  /// list recoverable until policy, realtime and notification updates succeed.
  pub async fn revoke(&self, state: &AppState, recipient: Option<i64>) -> Result<(), AppError> {
    self
      .revoke_with(&state.pg_pool, recipient, |uid, policies| {
        self.publish(state, uid, policies)
      })
      .await
  }

  async fn revoke_with<Fut>(
    &self,
    pool: &PgPool,
    recipient: Option<i64>,
    mut publish: impl FnMut(i64, Vec<(Uuid, Option<i32>)>) -> Fut,
  ) -> Result<(), AppError>
  where
    Fut: std::future::Future<Output = Result<(), AppError>>,
  {
    let mut conn = self.lock(pool).await?;
    let mut tx = conn.begin().await?;
    let oid = self.view_id.to_string();
    let members: Vec<(i64, Option<i32>, Option<String>)> = sqlx::query_as(
      "WITH recipients AS (
         SELECT uid FROM af_collab_member WHERE oid = $1
         UNION SELECT received_uid FROM af_collab_member_invite WHERE oid = $1 AND received_uid IS NOT NULL
       )
       SELECT r.uid, m.permission_id, (SELECT i.name FROM af_collab_member_invite i
         WHERE i.oid = $1 AND i.received_uid = r.uid ORDER BY i.created_at DESC LIMIT 1)
       FROM recipients r LEFT JOIN af_collab_member m ON m.oid = $1 AND m.uid = r.uid
       WHERE r.uid <> $2 AND ($3::bigint IS NULL OR r.uid = $3) ORDER BY r.uid"
    )
    .bind(&oid)
    .bind(self.owner)
    .bind(recipient)
    .fetch_all(&mut *tx)
    .await?;
    for (uid, old_permission, name) in members {
      let policies = self.set_member(&mut tx, uid, None).await?;
      publish(uid, policies).await?;
      if old_permission.is_some() {
        notify_collab_permission_changed(
          &mut tx,
          &self.workspace_id,
          &self.view_id,
          uid,
          name.as_deref().unwrap_or("未知文章"),
          old_permission,
          None,
          true,
        )
        .await?;
      }
    }
    if recipient.is_none() {
      sqlx::query("DELETE FROM af_collab_member_invite WHERE oid = $1")
        .bind(oid)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
  }

  pub fn require_owner(&self, uid: i64) -> Result<(), AppError> {
    if self.owner != uid {
      return Err(AppError::NotEnoughPermissions);
    }
    Ok(())
  }

  /// Update durable grants first, returning the effective policies to publish.
  /// For old invitations, linked_views supplies the mapping absent from the row.
  pub async fn set_member(
    &self,
    conn: &mut PgConnection,
    uid: i64,
    permission: Option<i32>,
  ) -> Result<Vec<(Uuid, Option<i32>)>, AppError> {
    if uid == self.owner {
      return Err(AppError::InvalidRequest("不能修改笔记所有者的权限".into()));
    }
    write_grant(conn, uid, self.view_id, permission).await?;
    let mut policies = vec![(self.view_id, permission)];
    if self.object_id != self.view_id {
      let remaining: Option<i32> = sqlx::query_scalar(
        "SELECT MAX(m.permission_id) FROM af_collab_member m
         WHERE m.uid = $1 AND EXISTS (
           SELECT 1 FROM af_collab_member_invite i
           WHERE i.received_uid = m.uid AND i.oid = m.oid
             AND (i.database_oid = $2 OR (i.database_oid IS NULL AND i.oid = ANY($3))))",
      )
      .bind(uid)
      .bind(self.object_id)
      .bind(&self.linked_views)
      .fetch_one(&mut *conn)
      .await?;
      write_grant(conn, uid, self.object_id, remaining).await?;
      policies.push((self.object_id, remaining));
    }
    Ok(policies)
  }

  pub async fn publish(
    &self,
    state: &AppState,
    uid: i64,
    policies: Vec<(Uuid, Option<i32>)>,
  ) -> Result<(), AppError> {
    let mut updates = Vec::new();
    for (object_id, permission) in policies {
      let permission_type = if let Some(id) = permission {
        let level = match id {
          1 => AFAccessLevel::ReadOnly,
          2 => AFAccessLevel::ReadAndComment,
          3 => AFAccessLevel::ReadAndWrite,
          4 => AFAccessLevel::FullAccess,
          _ => return Err(AppError::InvalidRequest("无效的权限 ID".into())),
        };
        state
          .collab_access_control
          .update_access_level_policy(&uid, &object_id, level)
          .await?;
        if level.can_write() {
          PermissionType::Write
        } else {
          PermissionType::Read
        }
      } else {
        state
          .collab_access_control
          .remove_access_level(&uid, &object_id)
          .await?;
        PermissionType::NoAccess
      };
      updates.push(PermissionUpdate {
        object_id,
        permission_type,
      });
    }
    state
      .ws_server
      .send(UpdateUserPermissions {
        workspace_id: self.workspace_id,
        uid,
        updates,
      })
      .await
      .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
  }
}

async fn write_grant(
  conn: &mut PgConnection,
  uid: i64,
  oid: Uuid,
  permission: Option<i32>,
) -> Result<(), AppError> {
  if let Some(id) = permission {
    sqlx::query(
      "INSERT INTO af_collab_member (uid, oid, permission_id) VALUES ($1,$2,$3)
      ON CONFLICT (uid, oid) DO UPDATE SET permission_id = EXCLUDED.permission_id",
    )
    .bind(uid)
    .bind(oid.to_string())
    .bind(id)
    .execute(conn)
    .await?;
  } else {
    sqlx::query("DELETE FROM af_collab_member WHERE uid = $1 AND oid = $2")
      .bind(uid)
      .bind(oid.to_string())
      .execute(conn)
      .await?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use sqlx::PgPool;

  async fn setup(pool: &PgPool) -> Result<(), AppError> {
    let mut conn = pool.acquire().await?;
    sqlx::raw_sql(
      "CREATE TABLE af_collab_member (
      uid bigint, oid text, permission_id integer, PRIMARY KEY (uid, oid));
      CREATE TABLE af_collab_member_invite (oid text, received_uid bigint, name text, created_at timestamptz);
      CREATE TABLE af_notification (workspace_id uuid, notification_type text, payload jsonb, recipient_uid bigint);",
    )
    .execute(&mut *conn)
    .await?;
    sqlx::raw_sql(include_str!(
      "../../../migrations/20261126000000_collab_share_database_oid.sql"
    ))
    .execute(&mut *conn)
    .await?;
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn retry_recomputes_linked_grants_after_rollback(pool: PgPool) -> Result<(), AppError> {
    setup(&pool).await?;
    let mut conn = pool.acquire().await?;
    let database = Uuid::new_v4();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let target = ShareTarget {
      workspace_id: Uuid::new_v4(),
      view_id: first,
      object_id: database,
      owner: 1,
      lock: PgAdvisoryLock::new(database.to_string()),
      linked_views: vec![first.to_string(), second.to_string()],
    };
    let uid = 2;
    for (view, permission, source) in [(first, 3, Some(database)), (second, 1, None)] {
      sqlx::query(
        "INSERT INTO af_collab_member_invite (oid, received_uid, database_oid) VALUES ($1, $2, $3)",
      )
      .bind(view.to_string())
      .bind(uid)
      .bind(source)
      .execute(&mut *conn)
      .await?;
      write_grant(&mut conn, uid, view, Some(permission)).await?;
    }
    write_grant(&mut conn, uid, database, Some(3)).await?;
    drop(conn);
    let first_attempt = target
      .revoke_with(&pool, None, |member, policies| {
        assert_eq!(member, uid);
        assert_eq!(policies, vec![(first, None), (database, Some(1))]);
        std::future::ready(Err(AppError::Unhandled("injected realtime failure".into())))
      })
      .await;
    assert!(matches!(first_attempt, Err(AppError::Unhandled(_))));
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM af_collab_member WHERE uid = $1")
      .bind(uid)
      .fetch_one(&pool)
      .await?;
    assert_eq!(rows, 3);
    let mut retried = Vec::new();
    target
      .revoke_with(&pool, None, |member, policies| {
        retried.push((member, policies));
        std::future::ready(Ok(()))
      })
      .await?;
    assert_eq!(
      retried,
      vec![(uid, vec![(first, None), (database, Some(1))])]
    );
    let last = ShareTarget {
      view_id: second,
      ..target
    };
    last
      .revoke_with(&pool, None, |_, policies| {
        assert_eq!(policies, vec![(second, None), (database, None)]);
        std::future::ready(Ok(()))
      })
      .await?;
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM af_collab_member WHERE uid = $1")
      .bind(uid)
      .fetch_one(&pool)
      .await?;
    assert_eq!(remaining, 0);
    let notifications: i64 =
      sqlx::query_scalar("SELECT COUNT(*) FROM af_notification WHERE recipient_uid = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await?;
    assert_eq!(notifications, 2);
    Ok(())
  }

  #[sqlx::test(migrations = false)]
  async fn stop_waits_for_an_in_flight_accept(pool: PgPool) -> Result<(), AppError> {
    setup(&pool).await?;
    let doc = Uuid::new_v4();
    let target = ShareTarget {
      workspace_id: Uuid::new_v4(),
      view_id: doc,
      object_id: doc,
      owner: 1,
      linked_views: Vec::new(),
      lock: PgAdvisoryLock::new(doc.to_string()),
    };
    let mut accepting = target.lock(&pool).await?;
    sqlx::query("INSERT INTO af_collab_member_invite (oid) VALUES ($1)")
      .bind(doc.to_string())
      .execute(&mut *accepting)
      .await?;
    let stopping = target.revoke_with(&pool, None, |uid, policies| {
      assert_eq!(uid, 2);
      assert_eq!(policies, vec![(doc, None)]);
      std::future::ready(Ok(()))
    });
    tokio::pin!(stopping);
    assert!(
      tokio::time::timeout(std::time::Duration::from_millis(100), &mut stopping)
        .await
        .is_err()
    );
    target.set_member(&mut accepting, 2, Some(1)).await?;
    accepting.release_now().await?;
    stopping.await?;
    let remaining: (i64, i64) = sqlx::query_as(
      "SELECT
      (SELECT COUNT(*) FROM af_collab_member), (SELECT COUNT(*) FROM af_collab_member_invite)",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(remaining, (0, 0));
    Ok(())
  }
}
