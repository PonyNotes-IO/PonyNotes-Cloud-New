use anyhow::Error;
use serde::de::DeserializeOwned;
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tracing::{error, trace};

pub struct PostgresDBListener<T: Clone> {
  pub notify: broadcast::Sender<T>,
}

impl<T> PostgresDBListener<T>
where
  T: Clone + DeserializeOwned + Send + 'static,
{
  pub async fn new(pg_pool: &PgPool, channel: &str) -> Result<Self, Error> {
    let mut listener = PgListener::connect_with(pg_pool).await?;
    // TODO(nathan): using listen_all
    listener.listen(channel).await?;

    let (tx, _) = broadcast::channel(1000);
    let notify = tx.clone();
    tokio::spawn(async move {
      while let Ok(notification) = listener.recv().await {
        trace!(
          "Received notification: channel: {}, payload: {}",
          notification.channel(),
          notification.payload()
        );
        let raw = notification.payload();
        match serde_json::from_str::<T>(raw) {
          Ok(change) => {
            trace!("Deserialized pg_notify payload OK (channel={})", notification.channel());
            let _ = tx.send(change);
          },
          Err(err) => {
            error!(
              "[pg_listener] Failed to deserialize payload on channel '{}': {:?}\nraw={}",
              notification.channel(),
              err,
              &raw[..raw.len().min(500)]
            );
          },
        }
      }
    });
    Ok(Self { notify })
  }
}
