#![deny(clippy::duplicate_mod)]

#[cfg(not(any(feature = "legacy_collab_api", feature = "modern_collab_api")))]
compile_error!("必须启用 legacy_collab_api 或 modern_collab_api 之一");

#[cfg(feature = "modern_collab_api")]
pub(crate) use yrs_modern as yrs_impl;
#[cfg(all(not(feature = "modern_collab_api"), feature = "legacy_collab_api"))]
pub(crate) use yrs_legacy as yrs_impl;

pub(crate) use yrs_impl as yrs;

mod http;
mod http_ai;
mod http_billing;

mod http_access_request;
mod http_blob;
mod http_collab;
mod http_guest;
mod http_member;
mod http_person;
pub mod http_publish;
mod http_quick_note;
mod http_search;
mod http_template;
mod http_view;
pub use http::*;

pub mod collab_sync;

mod http_chat;
mod http_file;
mod http_settings;
pub mod notify;
mod ping;
mod retry;

pub mod log;
pub mod v2;
pub mod ws;

pub mod error {
  pub use shared_entity::response::AppResponseError;
  pub use shared_entity::response::ErrorCode;
}

// Export all dto entities that will be used in the frontend application
pub mod entity {
  #[cfg(not(target_arch = "wasm32"))]
  pub use crate::http_chat::*;
  pub use appflowy_proto::WorkspaceNotification;
  pub use client_api_entity::*;
  pub use gotrue_entity::dto::CheckPasswordStatusResponse;
}

#[cfg(feature = "template")]
pub mod template {
  pub use workspace_template;
}
