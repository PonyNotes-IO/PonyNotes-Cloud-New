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

async fn has_collab_read_policy(
  access_control: &AccessControl,
  uid: &i64,
  oid: &Uuid,
) -> Result<bool, AppError> {
  access_control
    .enforce_immediately(uid, ObjectType::Collab(oid.to_string()), Action::Read)
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
    let workspace_action = match action {
      Action::Read => Action::Read,
      Action::Write => Action::Write,
      Action::Delete => Action::Write,
    };

    let collab_result = self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Collab(oid.to_string()),
        workspace_action.clone(),
      )
      .await;
    match collab_result {
      Ok(true) => return Ok(()),
      Err(e) => return Err(e),
      _ => {},
    }

    if !matches!(workspace_action, Action::Read)
      && has_collab_read_policy(&self.access_control, uid, oid).await?
    {
      return Err(AppError::NotEnoughPermissions);
    }

    let workspace_result = self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Workspace(workspace_id.to_string()),
        workspace_action,
      )
      .await;
    match workspace_result {
      Ok(true) => Ok(()),
      Ok(false) => Err(AppError::NotEnoughPermissions),
      Err(e) => Err(e),
    }
  }

  async fn enforce_access_level(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
    access_level: AFAccessLevel,
  ) -> Result<(), AppError> {
    let workspace_action = match access_level {
      AFAccessLevel::ReadOnly => Action::Read,
      AFAccessLevel::ReadAndComment => Action::Read,
      AFAccessLevel::ReadAndWrite => Action::Write,
      AFAccessLevel::FullAccess => Action::Write,
    };

    let collab_result = self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Collab(oid.to_string()),
        workspace_action.clone(),
      )
      .await;
    match collab_result {
      Ok(true) => return Ok(()),
      Err(e) => return Err(e),
      _ => {},
    }

    if !matches!(workspace_action, Action::Read)
      && has_collab_read_policy(&self.access_control, uid, oid).await?
    {
      return Err(AppError::NotEnoughPermissions);
    }

    let workspace_result = self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Workspace(workspace_id.to_string()),
        workspace_action,
      )
      .await;
    match workspace_result {
      Ok(true) => Ok(()),
      Ok(false) => Err(AppError::NotEnoughPermissions),
      Err(e) => Err(e),
    }
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

  async fn can_perform_action(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
    required_action: Action,
  ) -> Result<bool, AppError> {
    let workspace_action = match required_action {
      Action::Read => Action::Read,
      Action::Write => Action::Write,
      Action::Delete => Action::Write,
    };

    // 【共享协作修复 2026-07-02】文档级显式授权优先(Google Docs/Notion 模型)。
    // 旧逻辑对写操作先做 workspace 写权限短路(!can_write_workspace → return false),
    // 被分享者(非文档所属 workspace 成员)即使持有 collab 级编辑授权也永远过不了,
    // 其协作更新在 WS 层被静默丢弃——表现为"被分享者的编辑拥有者永远看不到"。
    // 改为:① collab 级显式策略命中即放行;② 持有 collab 级策略但不足以执行写
    // (被显式降权为只读)则拒绝,不得凭 workspace 角色重新升权(下方原有防御保留);
    // ③ 无 collab 级策略者回退 workspace 角色判定(对普通成员与旧行为完全等价)。
    let collab_result = self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Collab(oid.to_string()),
        workspace_action.clone(),
      )
      .await;
    match collab_result {
      Ok(true) => return Ok(true),
      Err(e) => return Err(e),
      _ => {},
    }

    if !matches!(workspace_action, Action::Read)
      && has_collab_read_policy(&self.access_control, uid, oid).await?
    {
      return Ok(false);
    }

    self
      .access_control
      .enforce_immediately(
        uid,
        ObjectType::Workspace(workspace_id.to_string()),
        workspace_action,
      )
      .await
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
    self
      .can_perform_action(workspace_id, uid, oid, Action::Write)
      .await
  }

  async fn can_read_collab(
    &self,
    workspace_id: &Uuid,
    uid: &i64,
    oid: &Uuid,
  ) -> Result<bool, AppError> {
    self
      .can_perform_action(workspace_id, uid, oid, Action::Read)
      .await
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
  pub async fn test_realtime_workspace_guest_cannot_write_even_with_collab_write() {
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
    assert!(!realtime_access_control
      .can_write_collab(&workspace_id, &uid, &oid)
      .await
      .unwrap());
  }
}
