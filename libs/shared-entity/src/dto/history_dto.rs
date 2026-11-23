use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersionMeta {
  pub snapshot_id: String,
  pub object_id: String,
  /// Unix milliseconds, assigned by the server.
  pub created_at: i64,
  /// True when saving reused an existing, still-readable version.
  #[serde(default)]
  pub reused: bool,
  #[serde(default)]
  pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentHistorySummary {
  pub workspace_id: Uuid,
  pub document_id: Uuid,
  pub name: String,
  pub workspace_name: String,
  pub path: Vec<String>,
  pub latest_version_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentHistoryIndex {
  pub items: Vec<DocumentHistorySummary>,
  pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersionList {
  pub items: Vec<DocumentVersionMeta>,
  pub has_more: bool,
  pub retention_days: i32,
  #[serde(default)]
  pub storage_limited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentVersionData {
  pub document: serde_json::Value,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotMeta {
  pub oid: String,
  /// Using yrs::Snapshot to deserialize the snapshot
  pub snapshot: Vec<u8>,
  /// Specifies the version of the snapshot
  pub snapshot_version: i32,
  pub created_at: i64,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct RepeatedSnapshotMeta {
  pub items: Vec<SnapshotMeta>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryState {
  pub object_id: String,
  pub doc_state: Vec<u8>,
  pub doc_state_version: i32,
}

/// [HistoryState] contains all the necessary information that can be used to restore the full state
/// collab. This collab object can restore given [SnapshotMeta]. The snapshot represents the state of
/// the collab at a certain point in time.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotInfo {
  pub history: HistoryState,
  pub snapshot_meta: SnapshotMeta,
}
