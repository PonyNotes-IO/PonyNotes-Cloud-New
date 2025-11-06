use anyhow::Error;
use collab::core::collab::default_client_id;
use collab_database::database::{Database, DatabaseContext};
use collab_database::database_trait::NoPersistenceDatabaseCollabService;
use collab_database::entity::{CreateDatabaseParams, EncodedDatabase};
use std::sync::Arc;

pub async fn create_database_collab(
  params: CreateDatabaseParams,
) -> Result<EncodedDatabase, Error> {
  let client_id = default_client_id();
  let collab_service = Arc::new(NoPersistenceDatabaseCollabService::new(client_id));
  let row_collab_service = Arc::new(NoPersistenceDatabaseCollabService::new(client_id));
  let context = DatabaseContext::new(collab_service, row_collab_service);
  let database = Database::create_with_view(params, context).await?;
  database
    .encode_database_collabs()
    .await
    .map_err(|e| anyhow::anyhow!("Failed to encode database collabs: {:?}", e))
}
