mod application;
mod config;
pub mod error;
pub mod import_worker;
pub(crate) mod s3_client;

mod metric;

mod mailer;
use crate::application::run_server;
use crate::config::Config;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  dotenvy::dotenv().ok();

  let config = Config::from_env()?;
  if std::env::args().nth(1).as_deref() == Some("--weknora-sync-once") {
    let pool = application::get_connection_pool(&config.db_settings).await?;
    let client = ponynotes_weknora::Client::from_env()?;
    let report = ponynotes_weknora::sync::run_once(&pool, &client).await?;
    println!("{report}");
    if report["errors"].as_array().is_none_or(|errors| !errors.is_empty()) {
      return Err("WeKnora sync failed; inspect the last_run summary".into());
    }
    return Ok(());
  }
  let listener = TcpListener::bind("[::]:4001").await.unwrap();
  run_server(listener, config).await
}
