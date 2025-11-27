#![deny(clippy::duplicate_mod)]

#[cfg(not(any(feature = "legacy_collab_api", feature = "modern_collab_api")))]
compile_error!("必须启用 legacy_collab_api 或 modern_collab_api 之一");

#[cfg(feature = "modern_collab_api")]
pub(crate) use yrs_modern as yrs_impl;
#[cfg(all(not(feature = "modern_collab_api"), feature = "legacy_collab_api"))]
pub(crate) use yrs_legacy as yrs_impl;

pub(crate) use yrs_impl as yrs;

mod data_validation;
mod message;
mod protocol;

pub use data_validation::*;
pub use message::*;
pub use protocol::*;
