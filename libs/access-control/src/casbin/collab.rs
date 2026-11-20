use super::access::AccessControl;
use crate::{
  act::Action,
  collab::{CollabAccessControl, RealtimeAccessControl},
  entity::{ObjectType, SubjectType},
};
use app_error::AppError;
use async_trait::async_trait;
use database_entity::dto::AFAccessLevel;
use tracing::instrument;
use uuid::Uuid;

async fn can_access_collab(
  access_control: &AccessControl,
  workspace_id: &Uuid,
  uid: &i64,
  oid: &Uuid,
  requested_action: Action,
) -> Result<bool, AppError> {
  let workspace_action = match requested_action {
    Action::Read => Action::Read,
    Action::Write | Action::Delete => Action::Write,
  };
  let object = ObjectType::Collab(oid.to_string());
  if access_control
    .enforce_immediately(uid, object.clone(), workspace_action.clone())
    .await?
  {
    return Ok(true);
  }
  if (workspace_action != Action::Read
    && access_control
      .enforce_immediately(uid, object, Action::Read)
      .await?)
    || match &access_control.pg_pool {
      Some(pool) => database::collab::requires_explicit_collab_access(pool, oid).await?,
      None => false,
    }
  {
    return Ok(false);
  }
  access_control
    .enforce_immediately(
      uid,
      ObjectType::Workspace(workspace_id.to_string()),
      workspace_action,
    )
    .await
}

#[derive(Clone)]
pub struct CollabAccessControlImpl {
  access_control: AccessControl,
}

impl CollabAccessControlImpl {
  pub fn new(access_control: AccessControl) -> Self {
    Self { access_control }
  }
}

#[async_trait]
impl CollabAccessControl for CollabAccessControlImpl {
  async fn enforce_action(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
    action: Action,
  ) -> Result<(), AppError> {
    if can_access_collab(&self.access_control, workspace_id, uid, oid, action).await? {
      return Ok(());
    }
    Err(AppError::NotEnoughPermissions)
  }

  async fn enforce_access_level(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
    access_level: AFAccessLevel,
  ) -> Result<(), AppError> {
    let action = match access_level {
      AFAccessLevel::ReadOnly | AFAccessLevel::ReadAndComment => Action::Read,
      AFAccessLevel::ReadAndWrite | AFAccessLevel::FullAccess => Action::Write,
    };
    self.enforce_action(workspace_id, uid, oid, action).await
  }

  #[instrument(level = "info", skip_all)]
  async fn update_access_level_policy(
    &self,
    uid: &i64,
    oid: &Uuid,
    level: AFAccessLevel,
  ) -> Result<(), AppError> {
    self
      .access_control
      .remove_policy(SubjectType::User(*uid), ObjectType::Collab(oid.to_string()))
      .await?;
    self
      .access_control
      .update_policy(
        SubjectType::User(*uid),
        ObjectType::Collab(oid.to_string()),
        level,
      )
      .await
  }

  #[instrument(level = "info", skip_all)]
  async fn remove_access_level(&self, uid: &i64, oid: &Uuid) -> Result<(), AppError> {
    self
      .access_control
      .remove_policy(SubjectType::User(*uid), ObjectType::Collab(oid.to_string()))
      .await
  }
}

#[derive(Clone)]
pub struct RealtimeCollabAccessControlImpl {
  access_control: AccessControl,
}

impl RealtimeCollabAccessControlImpl {
  pub fn new(access_control: AccessControl) -> Self {
    Self { access_control }
  }
}

#[async_trait]
impl RealtimeAccessControl for RealtimeCollabAccessControlImpl {
  async fn can_write_collab(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
  ) -> Result<bool, AppError> {
    can_access_collab(&self.access_control, workspace_id, uid, oid, Action::Write).await
  }

  async fn can_read_collab(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
  ) -> Result<bool, AppError> {
    can_access_collab(&self.access_control, workspace_id, uid, oid, Action::Read).await
  }
}

#[cfg(test)]
mod tests {
  use database_entity::dto::{AFAccessLevel, AFRole};
  use uuid::Uuid;

  use crate::casbin::util::tests::test_enforcer_v2;
  use crate::{
    act::Action,
    casbin::access::AccessControl,
    collab::{CollabAccessControl, RealtimeAccessControl},
    entity::{ObjectType, SubjectType},
  };

  #[tokio::test]
  pub async fn test_collab_access_control() {
    let enforcer = test_enforcer_v2().await;
    let uid = 1;
    let workspace_id = Uuid::new_v4();
    let oid = Uuid::new_v4();
    enforcer
      .update_policy(
        SubjectType::User(uid),
        ObjectType::Workspace(workspace_id.to_string()),
        AFRole::Member,
      )
      .await
      .unwrap();
    let access_control = AccessControl::with_enforcer(enforcer);
    let collab_access_control = super::CollabAccessControlImpl::new(access_control);
    for action in [Action::Read, Action::Write, Action::Delete] {
      collab_access_control
        .enforce_action(&workspace_id, &uid, &oid, action.clone())
        .await
        .unwrap_or_else(|_| panic!("Failed to enforce action: {:?}", action));
    }
  }

  #[tokio::test]
  #[ignore = "requires WORKSPACE_SYNC_TEST_DATABASE_URL"]
  async fn synced_document_requires_an_explicit_grant() {
    use sqlx::postgres::PgPoolOptions;
    let pool = PgPoolOptions::new()
      .max_connections(1)
      .connect(&std::env::var("WORKSPACE_SYNC_TEST_DATABASE_URL").unwrap())
      .await
      .unwrap();
    sqlx::query("CREATE TEMP TABLE af_collab (oid uuid PRIMARY KEY)")
      .execute(&pool)
      .await
      .unwrap();
    sqlx::query(include_str!(
      "../../../../migrations/20261119000001_restrict_workspace_sync_collabs.sql"
    ))
    .execute(&pool)
    .await
    .unwrap();
    let workspace = Uuid::new_v4();
    let oid = "50c290ef-2b37-5750-8cfb-60dc5ccd427c"
      .parse::<Uuid>()
      .unwrap();
    let regular = "42b83f76-aecc-5b03-9736-449bee5bf49e"
      .parse::<Uuid>()
      .unwrap();
    sqlx::query(
      "INSERT INTO af_collab (oid, requires_explicit_access) VALUES ($1, true), ($2, false)",
    )
    .bind(oid)
    .bind(regular)
    .execute(&pool)
    .await
    .unwrap();
    let enforcer = test_enforcer_v2().await;
    let owner = 1;
    let teammate = 2;
    enforcer
      .update_policy(
        SubjectType::User(teammate),
        ObjectType::Workspace(workspace.to_string()),
        AFRole::Member,
      )
      .await
      .unwrap();
    let mut access = AccessControl::with_enforcer(enforcer);
    access.pg_pool = Some(pool);
    let http = super::CollabAccessControlImpl::new(access.clone());
    let realtime = super::RealtimeCollabAccessControlImpl::new(access);
    http
      .update_access_level_policy(&owner, &oid, AFAccessLevel::FullAccess)
      .await
      .unwrap();
    assert!(http
      .enforce_action(&workspace, &owner, &oid, Action::Read)
      .await
      .is_ok());
    assert!(realtime
      .can_write_collab(&workspace, &owner, &oid)
      .await
      .unwrap());
    assert!(http
      .enforce_action(&workspace, &teammate, &regular, Action::Read)
      .await
      .is_ok());
    assert!(http
      .enforce_action(&workspace, &teammate, &oid, Action::Read)
      .await
      .is_err());
    assert!(!realtime
      .can_read_collab(&workspace, &teammate, &oid)
      .await
      .unwrap());
    assert!(!realtime
      .can_write_collab(&workspace, &teammate, &oid)
      .await
      .unwrap());
    http
      .update_access_level_policy(&teammate, &oid, AFAccessLevel::ReadOnly)
      .await
      .unwrap();
    assert!(http
      .enforce_action(&workspace, &teammate, &oid, Action::Read)
      .await
      .is_ok());
    assert!(realtime
      .can_read_collab(&workspace, &teammate, &oid)
      .await
      .unwrap());
    assert!(!realtime
      .can_write_collab(&workspace, &teammate, &oid)
      .await
      .unwrap());
    http.remove_access_level(&teammate, &oid).await.unwrap();
    assert!(!realtime
      .can_read_collab(&workspace, &teammate, &oid)
      .await
      .unwrap());
  }

  #[tokio::test]
  pub async fn test_collab_read_only_overrides_workspace_write() {
    let enforcer = test_enforcer_v2().await;
    let uid = 1;
    let workspace_id = Uuid::new_v4();
    let oid = Uuid::new_v4();
    enforcer
      .update_policy(
        SubjectType::User(uid),
        ObjectType::Workspace(workspace_id.to_string()),
        AFRole::Member,
      )
      .await
      .unwrap();

    let access_control = AccessControl::with_enforcer(enforcer);
    let collab_access_control = super::CollabAccessControlImpl::new(access_control.clone());
    let realtime_access_control = super::RealtimeCollabAccessControlImpl::new(access_control);

    collab_access_control
      .enforce_action(&workspace_id, &uid, &oid, Action::Write)
      .await
      .unwrap();

    collab_access_control
      .update_access_level_policy(&uid, &oid, AFAccessLevel::ReadOnly)
      .await
      .unwrap();

    collab_access_control
      .enforce_action(&workspace_id, &uid, &oid, Action::Read)
      .await
      .unwrap();
    assert!(collab_access_control
      .enforce_action(&workspace_id, &uid, &oid, Action::Write)
      .await
      .is_err());
    assert!(realtime_access_control
      .can_read_collab(&workspace_id, &uid, &oid)
      .await
      .unwrap());
    assert!(!realtime_access_control
      .can_write_collab(&workspace_id, &uid, &oid)
      .await
      .unwrap());
  }

  #[tokio::test]
  pub async fn test_realtime_explicit_collab_write_applies_to_workspace_guests() {
    let enforcer = test_enforcer_v2().await;
    let uid = 1;
    let workspace_id = Uuid::new_v4();
    let oid = Uuid::new_v4();
    enforcer
      .update_policy(
        SubjectType::User(uid),
        ObjectType::Workspace(workspace_id.to_string()),
        AFRole::Guest,
      )
      .await
      .unwrap();
    enforcer
      .update_policy(
        SubjectType::User(uid),
        ObjectType::Collab(oid.to_string()),
        AFAccessLevel::ReadAndWrite,
      )
      .await
      .unwrap();

    let access_control = AccessControl::with_enforcer(enforcer);
    let realtime_access_control = super::RealtimeCollabAccessControlImpl::new(access_control);

    assert!(realtime_access_control
      .can_read_collab(&workspace_id, &uid, &oid)
      .await
      .unwrap());
    assert!(realtime_access_control
      .can_write_collab(&workspace_id, &uid, &oid)
      .await
      .unwrap());
  }
}
