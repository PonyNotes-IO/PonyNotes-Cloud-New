use anyhow::Error;
use database::listener::PostgresDBListener;
use database::pg_row::{AFNotificationRow, AFSystemNotification, AFUserNotification};
use sqlx::PgPool;
use tracing::trace;

pub struct PgListeners {
  user_listener: UserListener,
  notification_listener: NotificationListener,
}

impl PgListeners {
  pub async fn new(pg_pool: &PgPool) -> Result<Self, Error> {
    let user_listener = UserListener::new(pg_pool, "af_user_channel").await?;
    let notification_listener =
      NotificationListener::new(pg_pool, "af_notification_channel").await?;
    Ok(Self {
      user_listener,
      notification_listener,
    })
  }

  pub fn subscribe_user_change(&self, uid: i64) -> tokio::sync::mpsc::Receiver<AFUserNotification> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    let mut user_notify = self.user_listener.notify.subscribe();
    tokio::spawn(async move {
      while let Ok(notification) = user_notify.recv().await {
        if let Some(row) = notification.payload.as_ref() {
          if row.uid == uid {
            let _ = tx.send(notification).await;
          }
        }
      }
    });
    rx
  }

  /// 订阅系统通知
  /// 仅推送给指定接收者（recipient_uid）或广播通知（recipient_uid 为 None）
  pub fn subscribe_system_notification(
    &self,
    uid: i64,
  ) -> tokio::sync::mpsc::Receiver<AFNotificationRow> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    let mut notify = self.notification_listener.notify.subscribe();
    tokio::spawn(async move {
      while let Ok(notification) = notify.recv().await {
        if let Some(row) = notification.payload.as_ref() {
          // 推送给指定接收者或广播通知（recipient_uid 为 None）
          let should_send = row.recipient_uid.is_none() || row.recipient_uid == Some(uid);
          if should_send {
            trace!(
              "Sending system notification to uid={}: type={}, id={}",
              uid,
              row.notification_type,
              row.id
            );
            if tx.send(row.clone()).await.is_err() {
              break;
            }
          }
        }
      }
    });
    rx
  }
}

pub type UserListener = PostgresDBListener<AFUserNotification>;
pub type NotificationListener = PostgresDBListener<AFSystemNotification>;