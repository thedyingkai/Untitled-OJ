//! Registry-backed diagnostic transport. Never enters the 0.2 action router.
use crate::auth::{ORCHESTRATOR_INTERNAL_TOKEN_HEADER, internal_token_check};
use crate::http::{ApiRequest, ApiResponse, StatusError, path_segments, query_value};
use crate::registry::RegistryContext;
use crate::routes::status_for_error;
use anyhow::Result;
use serde_json::json;

pub(crate) fn handle_registry_request(
    registry: &mut RegistryContext,
    request: ApiRequest,
    expected_internal_token: Option<&str>,
) -> ApiResponse {
    route(registry, request, expected_internal_token)
        .unwrap_or_else(|error| ApiResponse::error(status_for_error(&error), error.to_string()))
}

fn route(
    registry: &mut RegistryContext,
    request: ApiRequest,
    expected_internal_token: Option<&str>,
) -> Result<ApiResponse> {
    let path = request.path.split('?').next().unwrap_or("/");
    let query = request
        .path
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("");
    let segments = path_segments(path).map_err(|error| StatusError::new(400, error.to_string()))?;
    let parts = segments.iter().map(String::as_str).collect::<Vec<_>>();
    internal_token_check(
        &request.method,
        &parts,
        request
            .headers
            .get(ORCHESTRATOR_INTERNAL_TOKEN_HEADER)
            .map(String::as_str),
        expected_internal_token,
    )?;
    match (request.method.as_str(), parts.as_slice()) {
        ("GET", ["health"]) => Ok(ApiResponse::ok(json!({
            "status": "ok",
            "service": "ojos-orchestrator-daemon",
            "store": if registry.uses_persistent_store() { "persistent" } else { "memory" },
            "orchestrator_database_url": std::env::var("ORCHESTRATOR_DATABASE_URL").is_ok(),
            "warnings": registry.warnings(),
        }))),
        ("POST", ["diagnostics"]) => Ok(ApiResponse::created(json!({
            "action_result": registry.create_diagnostic()?,
        }))),
        ("GET", ["diagnostics"]) => {
            let cursor = query_value(query, "cursor")
                .map_err(|error| StatusError::new(400, error.to_string()))?
                .unwrap_or_default();
            let limit = query_value(query, "limit")
                .map_err(|error| StatusError::new(400, error.to_string()))?
                .map(|value| value.parse::<usize>())
                .transpose()
                .map_err(|_| StatusError::new(400, "limit must be an integer"))?
                .unwrap_or(50);
            if !(1..=200).contains(&limit) {
                return Err(StatusError::new(400, "limit must be between 1 and 200").into());
            }
            let mut reports = registry.diagnostic_reports()?;
            reports.sort_by(|left, right| left.report_id.cmp(&right.report_id));
            let mut reports = reports
                .into_iter()
                .filter(|report| report.report_id.as_str() > cursor.as_str())
                .take(limit + 1)
                .collect::<Vec<_>>();
            let next_cursor = if reports.len() > limit {
                reports.truncate(limit);
                reports.last().map(|report| report.report_id.clone())
            } else {
                None
            };
            Ok(ApiResponse::ok(
                json!({"items": reports, "next_cursor": next_cursor}),
            ))
        }
        ("GET", ["diagnostics", report_file])
            if report_file.ends_with(".json") || report_file.ends_with(".md") =>
        {
            let (report_id, format) = if report_file.ends_with(".json") {
                (report_file.trim_end_matches(".json"), "json")
            } else {
                (report_file.trim_end_matches(".md"), "markdown")
            };
            require_report(registry, report_id)?;
            let export = registry.diagnostic_export(report_id, format)?;
            Ok(ApiResponse::ok(
                json!({"report_id": export.report_id, "format": export.format, "content": export.content}),
            ))
        }
        ("GET", ["diagnostics", report_id]) => {
            let report = registry.diagnostic_report(report_id)?.ok_or_else(|| {
                StatusError::new(404, format!("diagnostic report {report_id} not found"))
            })?;
            Ok(ApiResponse::ok(json!({"diagnostic_report": report})))
        }
        _ => Ok(ApiResponse::error(
            404,
            format!(
                "unsupported Orchestrator API route {} {}",
                request.method, path
            ),
        )),
    }
}

fn require_report(registry: &RegistryContext, report_id: &str) -> Result<()> {
    if registry.diagnostic_report(report_id)?.is_none() {
        return Err(
            StatusError::new(404, format!("diagnostic report {report_id} not found")).into(),
        );
    }
    Ok(())
}
