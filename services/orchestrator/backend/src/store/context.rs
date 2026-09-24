//! Store context responsibilities.
use crate::store::error::StoreError;
use sha2::Digest;
use sha2::Sha256;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(crate) const CONTROL_PLANE_NODE_ID: &str = "control-plane";

pub(crate) fn now_marker() -> String {
    format!("unix-ms:{}", now_ms())
}

pub(crate) fn operation_id(
    prefix: &str,
    target_id: &str,
    request: &MutationContext,
) -> Result<String, StoreError> {
    let key = request
        .idempotency_key
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            StoreError::new(
                400,
                "IDEMPOTENCY_KEY_REQUIRED",
                "Store mutations require an Idempotency-Key header",
            )
        })?;
    let digest = Sha256::digest(format!("{prefix}\0{target_id}\0{key}").as_bytes());
    Ok(format!("op-{prefix}-{digest:x}"))
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Caller identity needed for mutation replay. Capturing a key does not validate it early:
/// the established operation-planning step remains responsible for rejecting an absent key.
pub(crate) struct MutationContext {
    pub(crate) idempotency_key: Option<String>,
}
