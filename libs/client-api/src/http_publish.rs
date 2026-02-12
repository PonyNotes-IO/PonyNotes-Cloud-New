use crate::{process_response_data, process_response_error, Client};
use bytes::Bytes;
use client_api_entity::publish_dto::DuplicatePublishedPageResponse;
use client_api_entity::workspace_dto::{PublishInfoView, PublishedView};
use client_api_entity::{workspace_dto::PublishedDuplicate, PublishInfo, UpdatePublishNamespace};
use client_api_entity::{
  CreateGlobalCommentParams, CreateReactionParams, DeleteGlobalCommentParams, DeleteReactionParams,
  GetReactionQueryParams, GlobalComments, PatchPublishedCollab, PublishInfoMeta, Reactions,
  UpdateDefaultPublishView,
};
use reqwest::Method;
use shared_entity::response::AppResponseError;
use tracing::instrument;
use uuid::Uuid;

// Add new DTOs for receive published collab
use serde::{Deserialize, Serialize};

/// 用户接收的发布文档请求
#[derive(Debug, Serialize)]
pub struct ReceivePublishedCollabRequest {
  pub published_view_id: Uuid,
  pub dest_workspace_id: Uuid,
  pub dest_view_id: Uuid,
}

/// 用户接收的发布文档响应
#[derive(Debug, Deserialize)]
pub struct ReceivePublishedCollabResponse {
  pub view_id: Uuid,
  pub is_readonly: bool,
}

/// 所有发布的文档列表项（包含发布者和接收者的信息）
#[derive(Debug, Deserialize)]
pub struct AllPublishedCollabItem {
  pub published_view_id: Uuid,
  pub view_id: Uuid,
  pub workspace_id: Uuid,
  pub name: String,
  pub publish_name: String,
  pub publisher_email: Option<String>,
  pub published_at: chrono::DateTime<chrono::Utc>,
  pub is_received: bool,
  pub is_readonly: bool,
}

/// 获取所有发布的文档列表响应
#[derive(Debug, Deserialize)]
pub struct ListAllPublishedCollabResponse {
  pub items: Vec<AllPublishedCollabItem>,
}

// Publisher API
impl Client {
  #[instrument(level = "debug", skip_all)]
  pub async fn list_published_views(
    &self,
    workspace_id: &Uuid,
  ) -> Result<Vec<PublishInfoView>, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/published-info",
      self.base_url, workspace_id,
    );

    let resp = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .send()
      .await?;
    process_response_data::<Vec<PublishInfoView>>(resp).await
  }

  /// 获取所有发布的笔记列表（不限制 workspace_id）
  /// 用于侧边栏发布菜单显示所有发布的笔记
  #[instrument(level = "debug", skip_all)]
  pub async fn list_all_published_views(
    &self,
  ) -> Result<ListAllPublishedCollabResponse, AppResponseError> {
    let url = format!("{}/api/published-info/all", self.base_url);

    let resp = self
      .cloud_client
      .get(&url)
      .send()
      .await?
      .error_for_status()?;
    process_response_data::<ListAllPublishedCollabResponse>(resp).await
  }

  /// 接收发布的文档（复制到自己的工作区）
  /// 发布的文档对接收者默认是只读的，不能协作同步
  #[instrument(level = "debug", skip_all)]
  pub async fn receive_published_collab(
    &self,
    request: &ReceivePublishedCollabRequest,
  ) -> Result<ReceivePublishedCollabResponse, AppResponseError> {
    let url = format!("{}/api/published/receive", self.base_url);

    let resp = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .json(request)
      .send()
      .await?;
    process_response_data::<ReceivePublishedCollabResponse>(resp).await
  }

  /// Changes the namespace for the first non-original publish namespace
  /// or the original publish namespace if not exists.
  pub async fn set_workspace_publish_namespace(
    &self,
    workspace_id: &Uuid,
    new_namespace: String,
  ) -> Result<(), AppResponseError> {
    let old_namespace = self.get_workspace_publish_namespace(workspace_id).await?;

    let url = format!(
      "{}/api/workspace/{}/publish-namespace",
      self.base_url, workspace_id
    );

    let resp = self
      .http_client_with_auth(Method::PUT, &url)
      .await?
      .json(&UpdatePublishNamespace {
        old_namespace,
        new_namespace,
      })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn get_workspace_publish_namespace(
    &self,
    workspace_id: &Uuid,
  ) -> Result<String, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/publish-namespace",
      self.base_url, workspace_id
    );
    let resp = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .send()
      .await?;
    process_response_data::<String>(resp).await
  }

  pub async fn patch_published_collabs(
    &self,
    workspace_id: &Uuid,
    patches: &[PatchPublishedCollab],
  ) -> Result<(), AppResponseError> {
    let url = format!("{}/api/workspace/{}/publish", self.base_url, workspace_id);
    let resp = self
      .http_client_with_auth(Method::PATCH, &url)
      .await?
      .json(patches)
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn unpublish_collabs(
    &self,
    workspace_id: &Uuid,
    view_ids: &[Uuid],
  ) -> Result<(), AppResponseError> {
    let url = format!("{}/api/workspace/{}/publish", self.base_url, workspace_id);
    let resp = self
      .http_client_with_auth(Method::DELETE, &url)
      .await?
      .json(view_ids)
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn create_comment_on_published_view(
    &self,
    view_id: &Uuid,
    comment_content: &str,
    reply_comment_id: &Option<Uuid>,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/comment",
      self.base_url, view_id
    );
    let resp = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .json(&CreateGlobalCommentParams {
        content: comment_content.to_string(),
        reply_comment_id: *reply_comment_id,
      })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn delete_comment_on_published_view(
    &self,
    view_id: &Uuid,
    comment_id: &Uuid,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/comment",
      self.base_url, view_id
    );
    let resp = self
      .http_client_with_auth(Method::DELETE, &url)
      .await?
      .json(&DeleteGlobalCommentParams {
        comment_id: *comment_id,
      })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn create_reaction_on_comment(
    &self,
    reaction_type: &str,
    view_id: &Uuid,
    comment_id: &Uuid,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/reaction",
      self.base_url, view_id
    );
    let resp = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .json(&CreateReactionParams {
        reaction_type: reaction_type.to_string(),
        comment_id: *comment_id,
      })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn delete_reaction_on_comment(
    &self,
    reaction_type: &str,
    view_id: &Uuid,
    comment_id: &Uuid,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/reaction",
      self.base_url, view_id
    );
    let resp = self
      .http_client_with_auth(Method::DELETE, &url)
      .await?
      .json(&DeleteReactionParams {
        reaction_type: reaction_type.to_string(),
        comment_id: *comment_id,
      })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn set_default_publish_view(
    &self,
    workspace_id: &Uuid,
    view_id: Uuid,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/publish-default",
      self.base_url, workspace_id
    );
    let resp = self
      .http_client_with_auth(Method::PUT, &url)
      .await?
      .json(&UpdateDefaultPublishView { view_id })
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn delete_default_publish_view(
    &self,
    workspace_id: &Uuid,
  ) -> Result<(), AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/publish-default",
      self.base_url, workspace_id
    );
    let resp = self
      .http_client_with_auth(Method::DELETE, &url)
      .await?
      .send()
      .await?;
    process_response_error(resp).await
  }

  pub async fn get_default_publish_view_info(
    &self,
    workspace_id: &Uuid,
  ) -> Result<PublishInfo, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/publish-default",
      self.base_url, workspace_id
    );
    let resp = self
      .http_client_with_auth(Method::GET, &url)
      .await?
      .send()
      .await?;
    process_response_data::<PublishInfo>(resp).await
  }
}

// Optional login
impl Client {
  pub async fn get_published_view_comments(
    &self,
    view_id: &Uuid,
  ) -> Result<GlobalComments, AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/comment",
      self.base_url, view_id
    );
    let client = if let Ok(client) = self.http_client_with_auth(Method::GET, &url).await {
      client
    } else {
      self.http_client_without_auth(Method::GET, &url).await?
    };

    let resp = client.send().await?;
    process_response_data::<GlobalComments>(resp).await
  }
}

// Guest API (no login required)
impl Client {
  #[instrument(level = "debug", skip_all)]
  pub async fn get_published_collab_info(
    &self,
    view_id: &Uuid,
  ) -> Result<PublishInfo, AppResponseError> {
    let url = format!(
      "{}/api/workspace/v1/published-info/{}",
      self.base_url, view_id
    );

    let resp = self.cloud_client.get(&url).send().await?;
    process_response_data::<PublishInfo>(resp).await
  }

  #[instrument(level = "debug", skip_all)]
  pub async fn get_published_outline(
    &self,
    publish_namespace: &str,
  ) -> Result<PublishedView, AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-outline/{}",
      self.base_url, publish_namespace,
    );

    let resp = self
      .cloud_client
      .get(&url)
      .send()
      .await?
      .error_for_status()?;

    process_response_data::<PublishedView>(resp).await
  }

  #[instrument(level = "debug", skip_all)]
  pub async fn get_default_published_collab<T>(
    &self,
    publish_namespace: &str,
  ) -> Result<PublishInfoMeta<T>, AppResponseError>
  where
    T: serde::de::DeserializeOwned + 'static,
  {
    let url = format!(
      "{}/api/workspace/published/{}",
      self.base_url, publish_namespace,
    );

    let resp = self
      .cloud_client
      .get(&url)
      .send()
      .await?
      .error_for_status()?;

    process_response_data::<PublishInfoMeta<T>>(resp).await
  }

  #[instrument(level = "debug", skip_all)]
  pub async fn get_published_collab<T>(
    &self,
    publish_namespace: &str,
    publish_name: &str,
  ) -> Result<T, AppResponseError>
  where
    T: serde::de::DeserializeOwned + 'static,
  {
    tracing::debug!(
      "get_published_collab: {} {}",
      publish_namespace,
      publish_name
    );
    let url = format!(
      "{}/api/workspace/v1/published/{}/{}",
      self.base_url, publish_namespace, publish_name
    );

    let resp = self
      .cloud_client
      .get(&url)
      .send()
      .await?
      .error_for_status()?;

    process_response_data::<T>(resp).await
  }

  #[instrument(level = "debug", skip_all)]
  pub async fn get_published_collab_blob(
    &self,
    publish_namespace: &str,
    publish_name: &str,
  ) -> Result<Bytes, AppResponseError> {
    tracing::debug!(
      "get_published_collab_blob: {} {}",
      publish_namespace,
      publish_name
    );
    let url = format!(
      "{}/api/workspace/published/{}/{}/blob",
      self.base_url, publish_namespace, publish_name
    );
    let resp = self.cloud_client.get(&url).send().await?;
    let bytes = resp.error_for_status()?.bytes().await?;

    if let Ok(app_err) = serde_json::from_slice::<AppResponseError>(&bytes) {
      return Err(app_err);
    }

    Ok(bytes)
  }

  pub async fn duplicate_published_to_workspace(
    &self,
    workspace_id: Uuid,
    publish_duplicate: &PublishedDuplicate,
  ) -> Result<DuplicatePublishedPageResponse, AppResponseError> {
    let url = format!(
      "{}/api/workspace/{}/published-duplicate",
      self.base_url, workspace_id
    );
    let resp = self
      .http_client_with_auth(Method::POST, &url)
      .await?
      .json(publish_duplicate)
      .send()
      .await?;
    process_response_data::<DuplicatePublishedPageResponse>(resp).await
  }

  pub async fn get_published_view_reactions(
    &self,
    view_id: &Uuid,
    comment_id: &Option<Uuid>,
  ) -> Result<Reactions, AppResponseError> {
    let url = format!(
      "{}/api/workspace/published-info/{}/reaction",
      self.base_url, view_id
    );
    let resp = self
      .cloud_client
      .get(url)
      .query(&GetReactionQueryParams {
        comment_id: *comment_id,
      })
      .send()
      .await?;
    process_response_data::<Reactions>(resp).await
  }
}
