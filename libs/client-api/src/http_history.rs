use crate::Client;
use reqwest::Method;
use shared_entity::dto::history_dto::{
  DocumentHistoryIndex, DocumentVersionData, DocumentVersionList, DocumentVersionMeta,
};
use shared_entity::response::{AppResponse, AppResponseError};
use uuid::Uuid;

impl Client {
  pub async fn list_documents_with_history(
    &self,
    search: &str,
    workspace: &str,
    before: i64,
    before_document: &str,
  ) -> Result<DocumentHistoryIndex, AppResponseError> {
    let url = format!("{}/api/workspace/history", self.base_url);
    let response = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .query(&[
        ("search", search),
        ("workspace", workspace),
        ("before", &before.to_string()),
        ("before_document", before_document),
      ])
      .send()
      .await?;
    response
      .json::<AppResponse<DocumentHistoryIndex>>()
      .await?
      .into_data()
  }

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
    self
      .write_document_version(workspace, document, data, false)
      .await
  }

  pub async fn write_document_version(
    &self,
    workspace: &Uuid,
    document: &Uuid,
    data: DocumentVersionData,
    restore_backup: bool,
  ) -> Result<DocumentVersionMeta, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{workspace}/collab/{document}/history",
      self.base_url
    );
    let response = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .query(&[("restore_backup", restore_backup)])
      .json(&data)
      .send()
      .await?;
    response
      .json::<AppResponse<DocumentVersionMeta>>()
      .await?
      .into_data()
  }
}
