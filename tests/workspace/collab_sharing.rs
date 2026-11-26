use app_error::ErrorCode;
use client_api::entity::QueryCollabParams;
use client_api::Client;
use client_api_test::generate_unique_registered_user_client;
use collab_entity::CollabType;
use reqwest::Method;
use serde_json::{json, Value};
use shared_entity::dto::workspace_dto::{
  CreatePageDatabaseViewParams, CreatePageParams, CreateSpaceParams, SpacePermission, ViewLayout,
};
use uuid::Uuid;

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

async fn private_page(owner: &Client, workspace_id: Uuid, layout: ViewLayout) -> Uuid {
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
  owner
    .create_workspace_page_view(
      workspace_id,
      &CreatePageParams {
        parent_view_id: space.view_id,
        layout,
        name: Some("Shared note".to_string()),
        page_data: None,
        view_id: None,
        collab_id: None,
      },
    )
    .await
    .unwrap()
    .view_id
}

#[tokio::test]
async fn only_owner_can_stop_all_sharing_and_invalidate_links() {
  let (owner, _) = generate_unique_registered_user_client().await;
  let (guest, _) = generate_unique_registered_user_client().await;
  let (second_guest, _) = generate_unique_registered_user_client().await;
  let workspace_id = owner.get_workspaces().await.unwrap()[0].workspace_id;
  let owner_uid = owner.get_profile().await.unwrap().uid;
  let view_id = private_page(&owner, workspace_id, ViewLayout::Document).await;
  let path = format!("/api/workspace/{workspace_id}/collab/{view_id}");
  let sharing_path = format!("{path}/sharing");
  let invite_path = format!("{path}/invite-link");

  assert_eq!(request(&owner, Method::POST, &invite_path).await["code"], 0);
  for recipient in [&guest, &second_guest] {
    let uuid = recipient.get_profile().await.unwrap().uuid;
    let result = request(recipient, Method::POST, &format!("{path}/members/{uuid}")).await;
    assert_eq!(result["code"], 0);
    recipient
      .get_collab(QueryCollabParams::new(
        view_id,
        CollabType::Document,
        workspace_id,
      ))
      .await
      .unwrap();
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
    assert_eq!(
      recipient
        .get_collab(QueryCollabParams::new(
          view_id,
          CollabType::Document,
          workspace_id
        ))
        .await
        .unwrap_err()
        .code,
      ErrorCode::NotEnoughPermissions
    );
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
  let member_path = format!("{path}/members/{}", guest.get_profile().await.unwrap().uuid);
  let (accepted, stopped) = tokio::join!(
    request(&guest, Method::POST, &member_path),
    request(&owner, Method::DELETE, &sharing_path),
  );
  assert_eq!(accepted["code"], 0);
  assert_eq!(stopped["code"], 0);
  assert_eq!(
    guest
      .get_collab(QueryCollabParams::new(
        view_id,
        CollabType::Document,
        workspace_id
      ))
      .await
      .unwrap_err()
      .code,
    ErrorCode::NotEnoughPermissions
  );
  let sent = request(&owner, Method::GET, "/api/collab/me/sent").await;
  assert!(sent["data"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn database_access_survives_until_the_last_shared_view_is_revoked() {
  let (owner, _) = generate_unique_registered_user_client().await;
  let (guest, _) = generate_unique_registered_user_client().await;
  let workspace = owner.get_workspaces().await.unwrap()[0].workspace_id;
  let grid = private_page(&owner, workspace, ViewLayout::Grid).await;
  owner
    .create_database_view(
      workspace,
      &grid,
      &CreatePageDatabaseViewParams {
        layout: ViewLayout::Board,
        name: Some("Linked board".into()),
      },
    )
    .await
    .unwrap();
  let folder = owner
    .get_workspace_folder(&workspace, Some(1), Some(grid))
    .await
    .unwrap();
  let board = folder
    .children
    .iter()
    .find(|v| v.name == "Linked board")
    .unwrap()
    .view_id;
  let resolved = request(
    &owner,
    Method::GET,
    &format!("/api/workspace/{workspace}/database/view/{grid}/resolve"),
  )
  .await;
  let database = Uuid::parse_str(resolved["data"]["database_id"].as_str().unwrap()).unwrap();
  assert_ne!(grid, database);
  let member = guest.get_profile().await.unwrap().uuid;
  let path = |view| format!("/api/workspace/{workspace}/collab/{view}");
  let read = || {
    guest.get_collab(QueryCollabParams::new(
      database,
      CollabType::Database,
      workspace,
    ))
  };

  // Exercise both direct invitations and link acceptance for the same database.
  assert_eq!(
    request(
      &owner,
      Method::POST,
      &format!("{}/members/{member}", path(grid))
    )
    .await["code"],
    0
  );
  assert_eq!(
    request(
      &owner,
      Method::POST,
      &format!("{}/invite-link", path(board))
    )
    .await["code"],
    0
  );
  assert_eq!(
    request(
      &guest,
      Method::POST,
      &format!("{}/members/{member}", path(board))
    )
    .await["code"],
    0
  );
  read().await.unwrap();
  assert_eq!(
    request(&owner, Method::DELETE, &format!("{}/sharing", path(grid))).await["code"],
    0
  );
  read().await.unwrap();
  assert_eq!(
    request(&owner, Method::DELETE, &format!("{}/sharing", path(board))).await["code"],
    0
  );
  assert_eq!(
    read().await.unwrap_err().code,
    ErrorCode::NotEnoughPermissions
  );
  owner
    .get_collab(QueryCollabParams::new(
      database,
      CollabType::Database,
      workspace,
    ))
    .await
    .unwrap();
}
