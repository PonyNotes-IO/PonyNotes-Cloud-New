use anyhow::Error;
#[cfg(all(not(feature = "modern_collab_api"), feature = "legacy_collab_api"))]
use collab::core::collab::{default_client_id, CollabOptions};
#[cfg(feature = "modern_collab_api")]
use collab::core::collab_plugin::CollabPlugin;
use collab::core::collab::DataSource;
use collab::core::origin::CollabOrigin;
use collab::entity::EncodedCollab;
use collab::preclude::Collab;
use collab_entity::CollabType;
use tracing::instrument;
use uuid::Uuid;

#[inline]
pub async fn collab_from_encode_collab(object_id: &Uuid, data: &[u8]) -> Result<Collab, Error> {
  let object_id = object_id.to_string();
  let data = data.to_vec();

  tokio::task::spawn_blocking(move || {
    let encoded_collab = EncodedCollab::decode_from_bytes(&data)?;
    create_collab_from_doc_state(&object_id, encoded_collab.doc_state.to_vec())
  })
  .await?
}

#[instrument(level = "trace", skip(data), fields(len = %data.len()))]
#[inline]
pub async fn validate_encode_collab(
  object_id: &Uuid,
  data: &[u8],
  collab_type: &CollabType,
) -> Result<(), Error> {
  let collab = collab_from_encode_collab(object_id, data).await?;
  collab_type.validate_require_data(&collab)?;
  Ok::<(), Error>(())
}

#[cfg(all(not(feature = "modern_collab_api"), feature = "legacy_collab_api"))]
fn create_collab_from_doc_state(object_id: &str, doc_state: Vec<u8>) -> Result<Collab, Error> {
  let data_source = DataSource::DocStateV1(doc_state);
  let options = CollabOptions::new(object_id.to_string(), default_client_id()).with_data_source(data_source);
  let collab = Collab::new_with_options(CollabOrigin::Empty, options)?;
  Ok(collab)
}

#[cfg(feature = "modern_collab_api")]
fn create_collab_from_doc_state(object_id: &str, doc_state: Vec<u8>) -> Result<Collab, Error> {
  let data_source = DataSource::DocStateV1(doc_state);
  let collab = Collab::new_with_source(
    CollabOrigin::Empty,
    object_id,
    data_source,
    Vec::<Box<dyn CollabPlugin>>::new(),
    false,
  )?;
  Ok(collab)
}
