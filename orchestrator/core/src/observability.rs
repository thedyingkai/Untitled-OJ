use crate::{
    DiagnosticFinding, DiagnosticReport, LogView, OperationLogRecord, OperationStatus,
    OrchestratorError, OrchestratorStore, Result, Topology, redact_secret_text,
    validate_endpoint_id,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogQuery {
    pub service_id: Option<String>,
    pub endpoint: Option<String>,
    pub operation_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogQueryResult {
    pub sources: Vec<LogView>,
    pub operation_logs: Vec<OperationLogRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticExport {
    pub report_id: String,
    pub format: String,
    pub content: String,
}

pub fn query_logs<S: OrchestratorStore>(store: &S, query: &LogQuery) -> Result<LogQueryResult> {
    let mut sources = store
        .list_log_sources()?
        .into_iter()
        .filter(|source| log_source_matches(source, query))
        .collect::<Vec<_>>();
    sources.sort_by(|left, right| left.source_id.cmp(&right.source_id));

    let mut operation_logs = if let Some(operation_id) = query.operation_id.as_deref() {
        store.list_operation_logs(operation_id)?
    } else {
        Vec::new()
    };
    sort_recent_operation_logs(&mut operation_logs);

    Ok(LogQueryResult {
        sources,
        operation_logs,
    })
}

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
    validate_endpoint_id(&log_view.endpoint)?;
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

pub fn build_diagnostic_report<S: OrchestratorStore>(
    store: &S,
    report_id: impl Into<String>,
) -> Result<DiagnosticReport> {
    let topology = store.build_topology_view()?;
    let operation_logs = recent_operation_logs(store, &topology, 20)?;
    let failed_operations = topology
        .operations
        .iter()
        .filter(|operation| matches!(operation.status, OperationStatus::Failed))
        .map(|operation| operation.operation_id.clone())
        .collect::<Vec<_>>();
    let unhealthy_endpoints = topology
        .endpoints
        .iter()
        .filter(|endpoint| {
            matches!(
                endpoint.health.as_str(),
                "degraded" | "blocked" | "unreachable"
            ) || !endpoint.reachable
        })
        .map(|endpoint| endpoint.endpoint.clone())
        .collect::<Vec<_>>();
    let unhealthy_links = topology
        .links
        .iter()
        .filter(|link| matches!(link.health.as_str(), "degraded" | "blocked" | "unreachable"))
        .map(|link| format!("{} -> {}", link.source_endpoint, link.target_endpoint))
        .collect::<Vec<_>>();
    let findings = diagnostic_findings(
        failed_operations.len(),
        unhealthy_endpoints.len(),
        unhealthy_links.len(),
    );
    let action_matrix = crate::action_matrix();
    let unsupported_capabilities = action_matrix
        .iter()
        .filter(|entry| {
            matches!(
                entry.capability_status,
                crate::ActionCapabilityStatus::Unsupported
            )
        })
        .map(|entry| entry.action_id.clone())
        .collect::<Vec<_>>();

    Ok(DiagnosticReport {
        report_id: report_id.into(),
        target_type: "Topology".to_string(),
        target_id: topology.root_endpoint.clone(),
        status: if findings.is_empty() {
            "ok".to_string()
        } else {
            "degraded".to_string()
        },
        summary: format!(
            "{} services, {} endpoints, {} links, {} operations",
            topology.services.len(),
            topology.endpoints.len(),
            topology.links.len(),
            topology.operations.len()
        ),
        operation_id: String::new(),
        data: serde_json::json!({
            "services_summary": {
                "count": topology.services.len(),
                "services": topology.services,
            },
            "endpoints_summary": {
                "count": topology.endpoints.len(),
                "unhealthy": unhealthy_endpoints,
            },
            "links_summary": {
                "count": topology.links.len(),
                "unhealthy": unhealthy_links,
            },
            "operations_summary": {
                "count": topology.operations.len(),
                "failed": failed_operations,
            },
            "recent_operation_logs": operation_logs,
            "database_schema_check": {
                "formal_tables": crate::ORCHESTRATOR_TABLES,
            },
            "action_matrix": action_matrix,
            "unsupported_capabilities": unsupported_capabilities,
            "forbidden_concept_scan_summary": {
                "formal_core_objects": [
                    "Service",
                    "Set",
                    "Endpoint",
                    "Link",
                    "Operation",
                    "Topology",
                    "LogView",
                    "DiagnosticReport"
                ]
            }
        }),
        findings,
        created_at: String::new(),
    })
}

pub fn export_diagnostic_report(
    report: &DiagnosticReport,
    format: &str,
) -> Result<DiagnosticExport> {
    match format {
        "json" => Ok(DiagnosticExport {
            report_id: report.report_id.clone(),
            format: "json".to_string(),
            content: serde_json::to_string_pretty(report)?,
        }),
        "markdown" => Ok(DiagnosticExport {
            report_id: report.report_id.clone(),
            format: "markdown".to_string(),
            content: diagnostic_markdown(report),
        }),
        _ => Err(OrchestratorError::InvalidManifest(
            "diagnostic export format must be json or markdown".to_string(),
        )),
    }
}

fn log_source_matches(source: &LogView, query: &LogQuery) -> bool {
    query
        .service_id
        .as_ref()
        .is_none_or(|service_id| &source.service_id == service_id)
        && query
            .endpoint
            .as_ref()
            .is_none_or(|endpoint| &source.endpoint == endpoint)
        && query.operation_id.as_ref().is_none_or(|operation_id| {
            source.operation_id.is_empty() || &source.operation_id == operation_id
        })
        && query
            .source_id
            .as_ref()
            .is_none_or(|source_id| &source.source_id == source_id)
}

fn recent_operation_logs<S: OrchestratorStore>(
    store: &S,
    topology: &Topology,
    limit: usize,
) -> Result<Vec<OperationLogRecord>> {
    let mut logs = Vec::new();
    for operation in &topology.operations {
        logs.extend(store.list_operation_logs(&operation.operation_id)?);
    }
    sort_recent_operation_logs(&mut logs);
    logs.truncate(limit);
    Ok(logs)
}

fn sort_recent_operation_logs(logs: &mut [OperationLogRecord]) {
    logs.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.operation_id.cmp(&left.operation_id))
            .then_with(|| right.step_id.cmp(&left.step_id))
    });
}

fn diagnostic_findings(
    failed_operation_count: usize,
    unhealthy_endpoint_count: usize,
    unhealthy_link_count: usize,
) -> Vec<DiagnosticFinding> {
    let mut findings = Vec::new();
    if failed_operation_count > 0 {
        findings.push(DiagnosticFinding {
            code: "operations.failed".to_string(),
            severity: "error".to_string(),
            message: format!("{failed_operation_count} failed operations"),
            redacted: false,
        });
    }
    if unhealthy_endpoint_count > 0 {
        findings.push(DiagnosticFinding {
            code: "endpoints.unhealthy".to_string(),
            severity: "warn".to_string(),
            message: format!("{unhealthy_endpoint_count} unhealthy endpoints"),
            redacted: false,
        });
    }
    if unhealthy_link_count > 0 {
        findings.push(DiagnosticFinding {
            code: "links.unhealthy".to_string(),
            severity: "warn".to_string(),
            message: format!("{unhealthy_link_count} unhealthy links"),
            redacted: false,
        });
    }
    findings
}

fn diagnostic_markdown(report: &DiagnosticReport) -> String {
    let mut lines = vec![
        format!("# DiagnosticReport {}", report.report_id),
        String::new(),
        format!("- status: {}", report.status),
        format!("- target: {} {}", report.target_type, report.target_id),
        format!("- summary: {}", redact_secret_text(&report.summary)),
    ];
    if !report.findings.is_empty() {
        lines.push(String::new());
        lines.push("## Findings".to_string());
        for finding in &report.findings {
            lines.push(format!(
                "- [{}] {}: {}",
                finding.severity,
                finding.code,
                redact_secret_text(&finding.message)
            ));
        }
    }
    lines.join("\n")
}
