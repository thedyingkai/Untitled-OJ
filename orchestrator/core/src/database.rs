use crate::{OrchestratorError, OrchestratorStore, Result};
use regex::Regex;
use std::collections::HashSet;

pub const ORCHESTRATOR_TABLES: &[&str] = &[
    "services",
    "service_sets",
    "service_endpoints",
    "service_links",
    "orchestrator_operations",
    "orchestrator_operation_logs",
    "orchestrator_operation_locks",
    "topology_snapshots",
    "log_sources",
    "diagnostic_reports",
];

pub const ORCHESTRATOR_DATABASE_STATEMENTS: &[DatabaseStatement] = &[
    DatabaseStatement {
        name: "services.upsert",
        sql: r#"
INSERT INTO services (service_id, set_id, name, version, status, kind, description, manifest, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())
ON CONFLICT (service_id) DO UPDATE SET
    set_id = EXCLUDED.set_id,
    name = EXCLUDED.name,
    version = EXCLUDED.version,
    status = EXCLUDED.status,
    kind = EXCLUDED.kind,
    description = EXCLUDED.description,
    manifest = EXCLUDED.manifest,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "services.list",
        sql: "SELECT service_id, set_id, name, version, status, kind, description, manifest, created_at, updated_at FROM services ORDER BY service_id",
    },
    DatabaseStatement {
        name: "service_sets.upsert",
        sql: r#"
INSERT INTO service_sets (set_id, name, description, sort_order, manifest, updated_at)
VALUES ($1, $2, $3, $4, $5, NOW())
ON CONFLICT (set_id) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    sort_order = EXCLUDED.sort_order,
    manifest = EXCLUDED.manifest,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_endpoints.upsert",
        sql: r#"
INSERT INTO service_endpoints (endpoint, service_id, protocol, health_path, health, reachable, display_name, note, config, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW())
ON CONFLICT (endpoint) DO UPDATE SET
    service_id = EXCLUDED.service_id,
    protocol = EXCLUDED.protocol,
    health_path = EXCLUDED.health_path,
    health = EXCLUDED.health,
    reachable = EXCLUDED.reachable,
    display_name = EXCLUDED.display_name,
    note = EXCLUDED.note,
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "service_links.upsert",
        sql: r#"
INSERT INTO service_links (source_endpoint, target_endpoint, protocol, auth_mode, scope, health, latency_ms, config_ref, secret_ref, policy, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
ON CONFLICT (source_endpoint, target_endpoint) DO UPDATE SET
    protocol = EXCLUDED.protocol,
    auth_mode = EXCLUDED.auth_mode,
    scope = EXCLUDED.scope,
    health = EXCLUDED.health,
    latency_ms = EXCLUDED.latency_ms,
    config_ref = EXCLUDED.config_ref,
    secret_ref = EXCLUDED.secret_ref,
    policy = EXCLUDED.policy,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operations.insert",
        sql: r#"
INSERT INTO orchestrator_operations (operation_id, action, target_type, target_id, status, actor_user_id, actor_username, request, plan, rollback_plan, result, error_message, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW())
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operations.update_status",
        sql: r#"
UPDATE orchestrator_operations
SET status = $2, result = $3, error_message = $4, updated_at = NOW()
WHERE operation_id = $1
"#,
    },
    DatabaseStatement {
        name: "orchestrator_operation_logs.append",
        sql: "INSERT INTO orchestrator_operation_logs (operation_id, level, message, metadata) VALUES ($1, $2, $3, $4)",
    },
    DatabaseStatement {
        name: "orchestrator_operation_locks.acquire",
        sql: r#"
INSERT INTO orchestrator_operation_locks (lock_key, owner, expires_at)
VALUES ($1, $2, $3)
ON CONFLICT (lock_key) DO UPDATE SET
    owner = EXCLUDED.owner,
    acquired_at = NOW(),
    expires_at = EXCLUDED.expires_at
WHERE orchestrator_operation_locks.expires_at < NOW()
"#,
    },
    DatabaseStatement {
        name: "topology_snapshots.insert",
        sql: "INSERT INTO topology_snapshots (snapshot_id, root_host, root_endpoint, authority, exposure_policy, snapshot) VALUES ($1, $2, $3, $4, $5, $6)",
    },
    DatabaseStatement {
        name: "log_sources.upsert",
        sql: r#"
INSERT INTO log_sources (source_id, endpoint, service_id, kind, location, config, updated_at)
VALUES ($1, $2, $3, $4, $5, $6, NOW())
ON CONFLICT (source_id) DO UPDATE SET
    endpoint = EXCLUDED.endpoint,
    service_id = EXCLUDED.service_id,
    kind = EXCLUDED.kind,
    location = EXCLUDED.location,
    config = EXCLUDED.config,
    updated_at = NOW()
"#,
    },
    DatabaseStatement {
        name: "diagnostic_reports.insert",
        sql: "INSERT INTO diagnostic_reports (report_id, target_type, target_id, status, summary, report) VALUES ($1, $2, $3, $4, $5, $6)",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseSchemaReport {
    pub tables: Vec<String>,
    pub missing_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatabaseStatement {
    pub name: &'static str,
    pub sql: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseAccessReport {
    pub statement_count: usize,
    pub touched_tables: Vec<String>,
    pub missing_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseWritePlan {
    pub writes: Vec<DatabaseWrite>,
    pub touched_tables: Vec<String>,
    pub non_formal_tables: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseWrite {
    pub object_type: String,
    pub object_id: String,
    pub table: String,
    pub statement: String,
}

pub fn inspect_orchestrator_schema(sql: &str) -> Result<DatabaseSchemaReport> {
    let table_re =
        Regex::new(r"(?i)CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-zA-Z_][a-zA-Z0-9_]*)")
            .expect("valid table regex");
    let mut tables = table_re
        .captures_iter(sql)
        .filter_map(|captures| captures.get(1).map(|item| item.as_str().to_string()))
        .collect::<Vec<_>>();
    tables.sort();
    tables.dedup();

    let table_set = tables.iter().map(String::as_str).collect::<HashSet<_>>();
    let formal_table_set = ORCHESTRATOR_TABLES.iter().copied().collect::<HashSet<_>>();
    let missing_tables = ORCHESTRATOR_TABLES
        .iter()
        .copied()
        .filter(|table| !table_set.contains(*table))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let non_formal_tables = tables
        .iter()
        .filter(|table| !formal_table_set.contains(table.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    if !missing_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator schema is missing tables: {}",
            missing_tables.join(", ")
        )));
    }
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator schema contains non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }

    Ok(DatabaseSchemaReport {
        tables,
        missing_tables,
        non_formal_tables,
    })
}

pub fn plan_database_writes<S: OrchestratorStore>(store: &S) -> Result<DatabaseWritePlan> {
    let mut writes = Vec::new();
    for service in store.services() {
        writes.push(database_write(
            "Service",
            service.id,
            "services",
            "services.upsert",
        ));
    }
    for set in store.sets() {
        writes.push(database_write(
            "Set",
            set.id,
            "service_sets",
            "service_sets.upsert",
        ));
    }
    for endpoint in store.endpoints() {
        writes.push(database_write(
            "Endpoint",
            endpoint.endpoint,
            "service_endpoints",
            "service_endpoints.upsert",
        ));
    }
    for link in store.links() {
        writes.push(database_write(
            "Link",
            format!("{} -> {}", link.source_endpoint, link.target_endpoint),
            "service_links",
            "service_links.upsert",
        ));
    }
    for operation in store.operations() {
        writes.push(database_write(
            "Operation",
            operation.operation_id.clone(),
            "orchestrator_operations",
            "orchestrator_operations.insert",
        ));
        for (index, _record) in store
            .operation_logs(&operation.operation_id)
            .iter()
            .enumerate()
        {
            writes.push(database_write(
                "OperationLog",
                format!("{}#{}", operation.operation_id, index + 1),
                "orchestrator_operation_logs",
                "orchestrator_operation_logs.append",
            ));
        }
    }
    for topology in store.topologies() {
        writes.push(database_write(
            "Topology",
            topology.root_endpoint,
            "topology_snapshots",
            "topology_snapshots.insert",
        ));
    }
    for log_view in store.log_views() {
        writes.push(database_write(
            "LogView",
            log_view.source_id,
            "log_sources",
            "log_sources.upsert",
        ));
    }
    for report in store.diagnostic_reports() {
        writes.push(database_write(
            "DiagnosticReport",
            report.report_id,
            "diagnostic_reports",
            "diagnostic_reports.insert",
        ));
    }

    let mut touched_tables = writes
        .iter()
        .map(|write| write.table.clone())
        .collect::<Vec<_>>();
    touched_tables.push("orchestrator_operation_locks".to_string());
    touched_tables.sort();
    touched_tables.dedup();

    let non_formal_tables = touched_tables
        .iter()
        .filter(|table| !is_formal_table(table))
        .cloned()
        .collect::<Vec<_>>();
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "database write plan touches non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }
    let statement_names = ORCHESTRATOR_DATABASE_STATEMENTS
        .iter()
        .map(|statement| statement.name)
        .collect::<HashSet<_>>();
    let missing_statements = writes
        .iter()
        .filter(|write| !statement_names.contains(write.statement.as_str()))
        .map(|write| write.statement.clone())
        .collect::<Vec<_>>();
    if !missing_statements.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "database write plan references missing statements: {}",
            missing_statements.join(", ")
        )));
    }

    Ok(DatabaseWritePlan {
        writes,
        touched_tables,
        non_formal_tables,
    })
}

pub fn inspect_database_access(statements: &[DatabaseStatement]) -> Result<DatabaseAccessReport> {
    let mut touched_tables = statements
        .iter()
        .flat_map(|statement| sql_table_references(statement.sql))
        .collect::<Vec<_>>();
    touched_tables.sort();
    touched_tables.dedup();

    let table_set = touched_tables
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let missing_tables = ORCHESTRATOR_TABLES
        .iter()
        .copied()
        .filter(|table| !table_set.contains(*table))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let non_formal_tables = touched_tables
        .iter()
        .filter(|table| !is_formal_table(table))
        .cloned()
        .collect::<Vec<_>>();

    if !missing_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator database access is missing tables: {}",
            missing_tables.join(", ")
        )));
    }
    if !non_formal_tables.is_empty() {
        return Err(OrchestratorError::Dependency(format!(
            "orchestrator database access touches non-formal tables: {}",
            non_formal_tables.join(", ")
        )));
    }

    Ok(DatabaseAccessReport {
        statement_count: statements.len(),
        touched_tables,
        missing_tables,
        non_formal_tables,
    })
}

fn sql_table_references(sql: &str) -> Vec<String> {
    let table_re = Regex::new(r"(?i)\b(?:FROM|INTO|UPDATE|JOIN)\s+([a-zA-Z_][a-zA-Z0-9_]*)")
        .expect("valid table reference regex");
    table_re
        .captures_iter(sql)
        .filter_map(|captures| captures.get(1).map(|item| item.as_str().to_string()))
        .filter(|table| !matches!(table.to_ascii_lowercase().as_str(), "set"))
        .collect()
}

fn is_formal_table(table: &str) -> bool {
    ORCHESTRATOR_TABLES.contains(&table)
}

fn database_write(
    object_type: impl Into<String>,
    object_id: impl Into<String>,
    table: impl Into<String>,
    statement: impl Into<String>,
) -> DatabaseWrite {
    DatabaseWrite {
        object_type: object_type.into(),
        object_id: object_id.into(),
        table: table.into(),
        statement: statement.into(),
    }
}
