//! PonyNotes conversation identity and visibility; execution stays in WeKnora.
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
  #[default]
  Private,
  Shared,
}

impl Visibility {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Private => "private",
      Self::Shared => "shared",
    }
  }
}

#[derive(Clone, Copy, Debug)]
pub struct Access {
  pub owner_uid: i64,
  pub shared: bool,
}

impl Access {
  pub fn allows(self, uid: i64) -> bool {
    self.shared || self.owner_uid == uid
  }
}

pub async fn list(pool: &PgPool, uid: i64, workspace: Uuid, offset: u32) -> Result<Vec<Value>> {
  Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',c.conversation_id,'title',c.title,'visibility',c.visibility,'creator_name',u.name,'creator_id',u.uuid,'created_at',c.created_at,'updated_at',c.updated_at) FROM af_agent_conversations c JOIN af_user u ON u.uid=c.uid WHERE c.workspace_id=$2 AND (c.uid=$1 OR c.visibility='shared') AND EXISTS(SELECT 1 FROM af_workspace_member m WHERE m.uid=$1 AND m.workspace_id=$2) ORDER BY c.updated_at DESC,c.conversation_id LIMIT 100 OFFSET $3")
    .bind(uid).bind(workspace).bind(i64::from(offset)).fetch_all(pool).await?)
}

pub async fn create(
  pool: &PgPool,
  uid: i64,
  workspace: Uuid,
  id: Uuid,
  title: &str,
  visibility: Visibility,
) -> Result<Option<Value>> {
  sqlx::query("INSERT INTO af_agent_conversations(conversation_id,uid,workspace_id,title,visibility) SELECT $1,$2,$3,$4,$5 WHERE EXISTS(SELECT 1 FROM af_workspace_member m WHERE m.uid=$2 AND m.workspace_id=$3) ON CONFLICT DO NOTHING")
    .bind(id).bind(uid).bind(workspace).bind(title).bind(visibility.as_str()).execute(pool).await?;
  // Retries cannot change an existing conversation's creator, workspace or scope.
  Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',c.conversation_id,'title',c.title,'visibility',c.visibility,'creator_name',u.name,'creator_id',u.uuid,'created_at',c.created_at,'updated_at',c.updated_at) FROM af_agent_conversations c JOIN af_user u ON u.uid=c.uid WHERE c.conversation_id=$1 AND c.uid=$2 AND c.workspace_id=$3 AND c.visibility=$4 AND EXISTS(SELECT 1 FROM af_workspace_member m WHERE m.uid=$2 AND m.workspace_id=$3)")
    .bind(id).bind(uid).bind(workspace).bind(visibility.as_str()).fetch_optional(pool).await?)
}

pub async fn access(pool: &PgPool, workspace: Uuid, id: Uuid, uid: i64) -> Result<Option<Access>> {
  let row: Option<(i64, String)> = sqlx::query_as("SELECT c.uid,c.visibility FROM af_agent_conversations c WHERE c.workspace_id=$1 AND c.conversation_id=$2 AND EXISTS(SELECT 1 FROM af_workspace_member m WHERE m.uid=$3 AND m.workspace_id=$1)")
    .bind(workspace).bind(id).bind(uid).fetch_optional(pool).await?;
  Ok(row.map(|(owner_uid, visibility)| Access {
    owner_uid,
    shared: visibility == "shared",
  }))
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  #[test]
  fn private_is_the_default_and_unknown_scopes_are_rejected() {
    assert_eq!(Visibility::default(), Visibility::Private);
    assert!(serde_json::from_value::<Visibility>(json!("public")).is_err());
    let private = Access {
      owner_uid: 1,
      shared: false,
    };
    assert!(private.allows(1));
    assert!(!private.allows(2));
    assert!(Access {
      owner_uid: 1,
      shared: true
    }
    .allows(2));
  }

  #[tokio::test]
  #[ignore = "requires TEST_DATABASE_URL ending in /agent_visibility_test"]
  async fn visibility_membership_pagination_and_idempotent_creates() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL");
    assert!(url.ends_with("/agent_visibility_test"));
    let pool = sqlx::postgres::PgPoolOptions::new()
      .max_connections(1)
      .connect(&url)
      .await
      .unwrap();
    let schema = format!("conversation_test_{}", Uuid::new_v4().simple());
    sqlx::raw_sql(&format!(
      "CREATE SCHEMA {schema}; SET search_path TO {schema};
      CREATE TABLE af_user(uid bigint PRIMARY KEY, uuid uuid NOT NULL, name text NOT NULL);
      CREATE TABLE af_workspace(workspace_id uuid PRIMARY KEY);
      CREATE TABLE af_workspace_member(uid bigint, workspace_id uuid);
    "
    ))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::raw_sql(include_str!(
      "../../../migrations/20261112000000_agent_conversations.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    // Exercise the actual additive migration against a legacy private row.
    let workspace = Uuid::new_v4();
    let other = Uuid::new_v4();
    let old = Uuid::new_v4();
    sqlx::query("INSERT INTO af_user VALUES(1,$1,'甲'),(2,$2,'乙'),(3,$3,'外部成员')")
      .bind(Uuid::new_v4())
      .bind(Uuid::new_v4())
      .bind(Uuid::new_v4())
      .execute(&pool)
      .await
      .unwrap();
    sqlx::query("INSERT INTO af_workspace VALUES($1),($2)")
      .bind(workspace)
      .bind(other)
      .execute(&pool)
      .await
      .unwrap();
    sqlx::query("INSERT INTO af_workspace_member VALUES(1,$1),(2,$1),(3,$2)")
      .bind(workspace)
      .bind(other)
      .execute(&pool)
      .await
      .unwrap();
    sqlx::query("INSERT INTO af_agent_conversations(conversation_id,uid,workspace_id,title) VALUES($1,1,$2,'旧对话')")
      .bind(old).bind(workspace).execute(&pool).await.unwrap();
    sqlx::raw_sql(include_str!(
      "../../../migrations/20261117000000_agent_conversation_visibility.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let shared = Uuid::new_v4();
    create(&pool, 1, workspace, shared, "共同讨论", Visibility::Shared)
      .await
      .unwrap()
      .unwrap();
    let private = Uuid::new_v4();
    create(
      &pool,
      2,
      workspace,
      private,
      "乙的私有对话",
      Visibility::Private,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(list(&pool, 1, workspace, 0).await.unwrap().len(), 2);
    assert_eq!(list(&pool, 2, workspace, 0).await.unwrap().len(), 2);
    assert!(list(&pool, 3, workspace, 0).await.unwrap().is_empty());
    assert!(!access(&pool, workspace, old, 2)
      .await
      .unwrap()
      .unwrap()
      .allows(2));
    assert!(access(&pool, workspace, shared, 2)
      .await
      .unwrap()
      .unwrap()
      .allows(2));
    assert_eq!(
      access(&pool, workspace, shared, 2)
        .await
        .unwrap()
        .unwrap()
        .owner_uid,
      1
    );
    assert!(access(&pool, other, shared, 3).await.unwrap().is_none());
    assert!(access(&pool, workspace, shared, 3).await.unwrap().is_none());
    assert!(
      create(&pool, 1, workspace, old, "改为共享", Visibility::Shared)
        .await
        .unwrap()
        .is_none()
    );
    assert!(
      create(&pool, 2, workspace, shared, "冒领", Visibility::Shared)
        .await
        .unwrap()
        .is_none()
    );
    assert!(create(
      &pool,
      3,
      workspace,
      Uuid::new_v4(),
      "越权创建",
      Visibility::Shared
    )
    .await
    .unwrap()
    .is_none());
    assert_eq!(
      create(&pool, 1, workspace, shared, "重试", Visibility::Shared)
        .await
        .unwrap()
        .unwrap()["title"],
      "共同讨论"
    );
    assert_eq!(list(&pool, 1, workspace, 1).await.unwrap().len(), 1);
    // Losing membership removes both list and direct access immediately.
    sqlx::query("DELETE FROM af_workspace_member WHERE uid=2 AND workspace_id=$1")
      .bind(workspace)
      .execute(&pool)
      .await
      .unwrap();
    assert!(list(&pool, 2, workspace, 0).await.unwrap().is_empty());
    assert!(access(&pool, workspace, shared, 2).await.unwrap().is_none());
    sqlx::raw_sql(&format!(
      "SET search_path TO public; DROP SCHEMA {schema} CASCADE"
    ))
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;
  }
}
