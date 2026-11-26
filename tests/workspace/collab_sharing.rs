use client_api::Client;
use client_api_test::generate_unique_registered_user_client;
use reqwest::Method;
use serde_json::{json, Value};
use shared_entity::dto::workspace_dto::{
  CreatePageParams, CreateSpaceParams, SpacePermission, ViewLayout,
};

async fn request(client: &Client, method: Method, path: &str) -> Value {
  client
    .http_client_with_auth(method, &format!("{}{}", client.base_url(), path))
    .await
    .unwrap()
    .json(&json!({}))
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap()
}

#[tokio::test]
async fn only_owner_can_stop_all_sharing_and_invalidate_links() {
  let (owner, _) = generate_unique_registered_user_client().await;
  let (guest, _) = generate_unique_registered_user_client().await;
  let (second_guest, _) = generate_unique_registered_user_client().await;
  let workspace_id = owner.get_workspaces().await.unwrap()[0].workspace_id;
  let owner_uid = owner.get_profile().await.unwrap().uid;
  let space = owner
    .create_space(
      workspace_id,
      &CreateSpaceParams {
        space_permission: SpacePermission::Private,
        name: "Stop sharing test".to_string(),
        space_icon: String::new(),
        space_icon_color: String::new(),
        view_id: None,
      },
    )
    .await
    .unwrap();
  let view_id = owner
    .create_workspace_page_view(
      workspace_id,
      &CreatePageParams {
        parent_view_id: space.view_id,
        layout: ViewLayout::Document,
        name: Some("Shared note".to_string()),
        page_data: None,
        view_id: None,
        collab_id: None,
      },
    )
    .await
    .unwrap()
    .view_id;
  let path = format!("/api/workspace/{workspace_id}/collab/{view_id}");
  let sharing_path = format!("{path}/sharing");
  let invite_path = format!("{path}/invite-link");

  assert_eq!(request(&owner, Method::POST, &invite_path).await["code"], 0);
  for recipient in [&guest, &second_guest] {
    let uuid = recipient.get_profile().await.unwrap().uuid;
    let result = request(recipient, Method::POST, &format!("{path}/members/{uuid}")).await;
    assert_eq!(result["code"], 0);
  }

  let rejected = request(&guest, Method::DELETE, &sharing_path).await;
  assert_ne!(rejected["code"], 0);
  let members = request(&owner, Method::GET, &format!("{path}/members")).await;
  assert_eq!(members["data"].as_array().unwrap().len(), 3);

  assert_eq!(
    request(&owner, Method::DELETE, &sharing_path).await["code"],
    0
  );
  let members = request(&owner, Method::GET, &format!("{path}/members")).await;
  let members = members["data"].as_array().unwrap();
  assert_eq!(members.len(), 1);
  assert_eq!(members[0]["uid"], owner_uid);

  let sent = request(&owner, Method::GET, "/api/collab/me/sent").await;
  assert!(sent["data"].as_array().unwrap().is_empty());
  let share_info = request(
    &guest,
    Method::GET,
    &format!("/api/collab/share-info?view_id={view_id}"),
  )
  .await;
  assert_ne!(share_info["code"], 0);

  for recipient in [&guest, &second_guest] {
    // Retrying the old link must not recreate access.
    let uuid = recipient.get_profile().await.unwrap().uuid;
    request(recipient, Method::POST, &format!("{path}/members/{uuid}")).await;
    let received = request(recipient, Method::GET, "/api/collab/me/received").await;
    assert!(received["data"].as_array().unwrap().is_empty());
  }
  owner
    .get_workspace_page_view(workspace_id, &view_id)
    .await
    .unwrap();

  // A retry and a share with no recipients also succeed.
  assert_eq!(
    request(&owner, Method::DELETE, &sharing_path).await["code"],
    0
  );
  assert_eq!(request(&owner, Method::POST, &invite_path).await["code"], 0);
  assert_eq!(
    request(&owner, Method::DELETE, &sharing_path).await["code"],
    0
  );
  let sent = request(&owner, Method::GET, "/api/collab/me/sent").await;
  assert!(sent["data"].as_array().unwrap().is_empty());
}
