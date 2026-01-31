use database_entity::dto::AFWorkspaceMember;
use serde::{Deserialize, Serialize};
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub enum UserMessage {
  ProfileChange(AFUserChange),
  WorkspaceMemberChange(AFWorkspaceMemberChange),
  /// 系统通知：用于工作空间级别的通知（成员加入、权限变更、@提及等）
  SystemNotification(AFSystemNotification),
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct AFUserChange {
  pub uid: i64,
  pub name: Option<String>,
  pub email: Option<String>,
  pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct AFWorkspaceMemberChange {
  added: Vec<AFWorkspaceMember>,
  updated: Vec<AFWorkspaceMember>,
  removed: Vec<AFWorkspaceMember>,
}

/// 系统通知结构体
#[derive(Debug, Clone, Serialize, Deserialize, Hash, Eq, PartialEq)]
pub struct AFSystemNotification {
  /// 通知唯一ID
  pub id: String,
  /// 工作空间ID
  pub workspace_id: String,
  /// 通知类型：workspace_member_joined, permission_changed, mention, etc.
  pub notification_type: String,
  /// 通知标题
  pub title: String,
  /// 通知内容
  pub message: String,
  /// 额外的JSON载荷
  pub payload_json: String,
  /// 创建时间戳（秒）
  pub created_at: i64,
  /// 接收者用户ID（0表示广播）
  pub recipient_uid: i64,
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct UserDevice {
  device_id: String,
  uid: i64,
}

impl UserDevice {
  pub fn new(device_id: &str, uid: i64) -> Self {
    Self {
      device_id: device_id.to_string(),
      uid,
    }
  }
}

impl From<&RealtimeUser> for UserDevice {
  fn from(user: &RealtimeUser) -> Self {
    Self {
      device_id: user.device_id.to_string(),
      uid: user.uid,
    }
  }
}

/// A `RealtimeUser` represents an individual user's connection within a realtime collaboration environment.
///
/// Each instance uniquely identifies a user's connection through a combination of user ID, device ID, and session ID.
/// This struct is crucial for managing user states, such as their active connections and interactions with the realtime server.
///
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RealtimeUser {
  pub uid: i64,
  /// `device_id`: A `String` representing the identifier of the device through which the user is connected.
  pub device_id: String,
  /// - `connect_at`: The time, in milliseconds since the Unix epoch, when the user established the connection to
  ///   the realtime server. For users connecting multiple times from the same device, this represents the most
  ///   recent connection time.
  pub connect_at: i64,
  /// - `session_id`: A `String` that uniquely identifies the current websocket connection session. This ID is
  ///   generated anew for each connection established, providing a mechanism to uniquely identify and manage
  ///   individual connection sessions. The session ID is used when cleanly handling user disconnections.
  pub session_id: String,
  /// - `app_version`: A `String` representing the version of the application that the user is using.
  pub app_version: String,
}

impl RealtimeUser {
  pub fn new(
    uid: i64,
    device_id: String,
    session_id: String,
    connect_at: i64,
    app_version: String,
  ) -> Self {
    Self {
      uid,
      device_id,
      connect_at,
      session_id,
      app_version,
    }
  }

  pub fn user_device(&self) -> String {
    format!("{}:{}", self.uid, self.device_id)
  }
}

impl Display for RealtimeUser {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.write_fmt(format_args!(
      "uid:{}|device_id:{}|connected_at:{}",
      self.uid, self.device_id, self.connect_at,
    ))
  }
}
