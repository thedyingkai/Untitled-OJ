//! Closed validation for declared service-scoped log sources.
use crate::{LogView, OrchestratorError, Result, validate_endpoint_service_name};

pub fn validate_log_view(log_view: &LogView) -> Result<()> {
    if log_view.source_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "log source_id is required".to_string(),
        ));
    }
    if log_view.service_id.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "log service_id is required".to_string(),
        ));
    }
    if log_view.endpoint.trim().is_empty() {
        return Err(OrchestratorError::InvalidManifest(
            "log endpoint is required".to_string(),
        ));
    }
    validate_endpoint_service_name(&log_view.endpoint, &log_view.service_id)?;
    if log_view.path.trim().is_empty()
        || log_view.path.contains("..")
        || log_view.path.contains('\\')
        || log_view.path.contains('\n')
        || log_view.path.contains('\r')
    {
        return Err(OrchestratorError::UnsafePath(
            "log view path must be service-scoped".to_string(),
        ));
    }
    if !matches!(
        log_view.read_policy.as_str(),
        "service-scoped" | "operation-scoped" | "endpoint-scoped"
    ) {
        return Err(OrchestratorError::InvalidManifest(
            "log read_policy must be scoped".to_string(),
        ));
    }
    Ok(())
}
