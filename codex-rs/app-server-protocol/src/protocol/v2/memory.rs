//! Readiness for selecting v2 memory context after background consolidation.

use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct MemoryStatusParams {
    /// Required distinct consolidated threads. Defaults to 20; supported range is 1..=4096.
    #[ts(optional = nullable)]
    pub min_consolidated_threads: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct MemoryStatusResponse {
    pub v2_consolidated_threads: u32,
    pub v2_ready: bool,
}

/// Host read-only inspection of the native memory library; never invokes a model.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct MemoryInspectParams {
    pub operation: MemoryInspectOperation,
    #[ts(optional = nullable)]
    pub path: Option<String>,
    #[ts(optional = nullable)]
    pub query: Option<String>,
    #[ts(optional = nullable)]
    pub cursor: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum MemoryInspectOperation {
    List,
    Read,
    Search,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct MemoryInspectEntry {
    pub path: String,
    pub directory: bool,
    pub content: Option<String>,
    pub line: Option<u32>,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct MemoryInspectResponse {
    pub entries: Vec<MemoryInspectEntry>,
    pub next_cursor: Option<String>,
    pub truncated: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct MemoryMaintenanceParams {
    pub operation: MemoryMaintenanceOperation,
    #[ts(optional = nullable)]
    pub thread_id: Option<String>,
    #[ts(optional = nullable)]
    pub force: Option<bool>,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub enum MemoryMaintenanceOperation {
    Start,
    Status,
    Stop,
}
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct MemoryMaintenanceResponse {
    pub active: bool,
    pub phase: String,
    pub stop_requested: bool,
    pub force_stop: bool,
    pub tokens_used: i64,
    pub child_thread_id: Option<String>,
    pub error: Option<String>,
}
