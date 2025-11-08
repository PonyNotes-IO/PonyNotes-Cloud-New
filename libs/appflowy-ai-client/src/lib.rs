#[cfg(feature = "client-api")]
pub mod client;

#[cfg(feature = "client-api")]
pub mod chat_client;

#[cfg(feature = "dto")]
pub mod dto;

pub mod error;

// Re-exports
#[cfg(feature = "client-api")]
pub use chat_client::ChatClient;

#[cfg(feature = "dto")]
pub use dto::{AIModel, AIModelInfo, AvailableModelsResponse, ChatMessage, ChatRequestParams};
