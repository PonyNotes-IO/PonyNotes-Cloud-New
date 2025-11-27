#![deny(clippy::duplicate_mod)]

#[cfg(not(any(feature = "legacy_collab_api", feature = "modern_collab_api")))]
compile_error!("必须启用 legacy_collab_api 或 modern_collab_api 之一");

#[cfg(all(not(feature = "modern_collab_api"), feature = "legacy_collab_api"))]
pub(crate) use yrs_legacy as yrs_impl;
#[cfg(feature = "modern_collab_api")]
pub(crate) use yrs_modern as yrs_impl;

pub(crate) use yrs_impl as yrs;

mod message;
pub mod user;

mod client_message;
// If the realtime_proto not exist, the following code will be generated:
// ```shell
//  cd libs/collab-rt-entity
//  cargo clean
//  cargo build
// ```
pub mod collab_proto;
pub mod realtime_proto;
mod server_message;

pub use client_message::*;
pub use message::*;
pub use realtime_proto::*;
pub use server_message::*;
