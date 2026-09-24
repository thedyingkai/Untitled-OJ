//! Stable metadata-operation response values; no execution capability.
use crate::OperationLogRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionCapabilityStatus {
    Real,
    RuntimePipeline,
    StoreBacked,
    Unsupported,
    Readonly,
}

impl ActionCapabilityStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Real => "REAL",
            Self::RuntimePipeline => "RUNTIME_PIPELINE",
            Self::StoreBacked => "STORE_BACKED",
            Self::Unsupported => "UNSUPPORTED",
            Self::Readonly => "READONLY",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionDispatchResult {
    pub action_id: String,
    pub status: String,
    pub message: String,
    pub operation_id: String,
    pub result: Value,
    pub error: String,
    pub warnings: Vec<String>,
    pub changed_objects: Vec<String>,
    pub capability_status: ActionCapabilityStatus,
    pub logs: Vec<OperationLogRecord>,
}
