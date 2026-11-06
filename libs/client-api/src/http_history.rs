use crate::Client;
use reqwest::Method;
use shared_entity::dto::history_dto::{
  DocumentVersionData, DocumentVersionList, DocumentVersionMeta,
};
use shared_entity::response::{AppResponse, AppResponseError};
use uuid::Uuid;

impl Client {
  pub async fn list_document_versions(
    &self,
    workspace: &Uuid,
    document: &Uuid,
    offset: u32,
  ) -> Result<DocumentVersionList, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{workspace}/collab/{document}/history?offset={offset}",
      self.base_url
    );
    let response = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .send()
      .await?;
    response
      .json::<AppResponse<DocumentVersionList>>()
      .await?
      .into_data()
  }

  pub async fn get_document_version(
    &self,
    workspace: &Uuid,
    document: &Uuid,
    snapshot: &Uuid,
  ) -> Result<DocumentVersionData, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{workspace}/collab/{document}/history/{snapshot}",
      self.base_url
    );
    let response = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .send()
      .await?;
    response
      .json::<AppResponse<DocumentVersionData>>()
      .await?
      .into_data()
  }

  pub async fn save_document_version(
    &self,
    workspace: &Uuid,
    document: &Uuid,
    data: DocumentVersionData,
  ) -> Result<DocumentVersionMeta, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{workspace}/collab/{document}/history",
      self.base_url
    );
    let response = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .json(&data)
      .send()
      .await?;
    response
      .json::<AppResponse<DocumentVersionMeta>>()
      .await?
      .into_data()
  }
}
