use crate::{
    PostgresError, PostgresOrchestratorStore, PostgresResult, RuntimeManagementMode,
    SqliteOrchestratorStore, StorageError, StorageResult, StoredRuntimeInstance,
    sqlite::{ENDPOINTS, HOST_SERVICES, NODES, TOPOLOGY_SNAPSHOTS},
};
use orchestrator_legacy::{
    Endpoint, HostService, NodeRecord, TopologyEndpointSpec, TopologyLinkSpec, TopologyRevision,
    TopologySnapshot, TopologySpec, TopologyStatus, parse_endpoint_id,
};
use orchestrator_runtime::{
    RuntimeContract, RuntimeDesiredState, RuntimeInstance, RuntimeObservedState,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const IMPORT_ID: &str = "v0.2-records-to-v1";
const PRIMARY_TOPOLOGY_ID: &str = "primary";
const IMPORT_ACTOR: &str = "migration:v0.2-to-v1";
const IMPORT_TIMESTAMP: &str = "legacy-v0.2-import";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyImportReport {
    pub import_id: String,
    pub source_checksum: String,
    pub topology_snapshot_id: Option<String>,
    pub topology_revision_id: Option<String>,
    pub topology_skipped_reason: Option<String>,
    pub runtime_instances_imported: usize,
    pub runtime_instances_skipped: Vec<String>,
}

#[derive(Debug, Clone)]
struct LegacyRecord {
    kind: String,
    key: String,
    payload: String,
}

#[derive(Debug, Default)]
struct LegacySource {
    records: Vec<LegacyRecord>,
    snapshot: Option<TopologySnapshot>,
    host_services: Vec<HostService>,
    endpoints: Vec<Endpoint>,
    nodes: Vec<NodeRecord>,
}

impl LegacySource {
    fn decode(records: Vec<LegacyRecord>) -> Result<Self, String> {
        let mut source = Self {
            records,
            ..Self::default()
        };
        for record in &source.records {
            match record.kind.as_str() {
                TOPOLOGY_SNAPSHOTS => {
                    // Records are loaded newest first. Only the latest snapshot
                    // represents the legacy desired view.
                    if source.snapshot.is_none() {
                        source.snapshot = Some(decode_record(record)?);
                    }
                }
                HOST_SERVICES => source.host_services.push(decode_record(record)?),
                ENDPOINTS => source.endpoints.push(decode_record(record)?),
                NODES => source.nodes.push(decode_record(record)?),
                _ => {}
            }
        }
        Ok(source)
    }

    fn checksum(&self) -> String {
        let mut hasher = Sha256::new();
        for record in &self.records {
            hasher.update(record.kind.as_bytes());
            hasher.update([0]);
            hasher.update(record.key.as_bytes());
            hasher.update([0]);
            hasher.update(record.payload.as_bytes());
            hasher.update([0xff]);
        }
        format!("{:x}", hasher.finalize())
    }
}

fn decode_record<T: for<'de> Deserialize<'de>>(record: &LegacyRecord) -> Result<T, String> {
    serde_json::from_str(&record.payload).map_err(|error| {
        format!(
            "legacy {} record {} is not valid v0.2 JSON: {error}",
            record.kind, record.key
        )
    })
}

impl SqliteOrchestratorStore {
    pub(crate) fn import_legacy_v0_2(&self) -> StorageResult<LegacyImportReport> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(report) = transaction
            .query_row(
                "SELECT report FROM orchestrator_legacy_imports WHERE import_id = ?1",
                [IMPORT_ID],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            return serde_json::from_str(&report).map_err(StorageError::from);
        }

        let source = load_sqlite_source(&transaction)?;
        let mut report = initial_report(&source);
        import_sqlite_topology(&transaction, &source, &mut report)?;
        import_sqlite_runtime_instances(&transaction, &source, &mut report)?;
        let encoded = serde_json::to_string(&report)?;
        transaction.execute(
            "INSERT INTO orchestrator_legacy_imports(import_id, source_checksum, report) VALUES (?1, ?2, ?3)",
            params![IMPORT_ID, report.source_checksum, encoded],
        )?;
        transaction.commit()?;
        Ok(report)
    }

    pub fn legacy_import_report(&self) -> StorageResult<LegacyImportReport> {
        self.connection()?
            .query_row(
                "SELECT report FROM orchestrator_legacy_imports WHERE import_id = ?1",
                [IMPORT_ID],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|payload| serde_json::from_str(&payload).map_err(StorageError::from))
            .transpose()?
            .ok_or_else(|| StorageError::Invariant("legacy import report is missing".to_string()))
    }
}

fn load_sqlite_source(transaction: &rusqlite::Transaction<'_>) -> StorageResult<LegacySource> {
    let mut statement = transaction.prepare(
        "SELECT kind, record_key, payload FROM orchestrator_records
         WHERE kind IN (?1, ?2, ?3, ?4)
         ORDER BY CASE WHEN kind = ?1 THEN 0 ELSE 1 END, updated_at DESC, record_key DESC",
    )?;
    let records = statement
        .query_map(
            params![TOPOLOGY_SNAPSHOTS, HOST_SERVICES, ENDPOINTS, NODES],
            |row| {
                Ok(LegacyRecord {
                    kind: row.get(0)?,
                    key: row.get(1)?,
                    payload: row.get(2)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()?;
    LegacySource::decode(records).map_err(StorageError::Domain)
}

fn import_sqlite_topology(
    transaction: &rusqlite::Transaction<'_>,
    source: &LegacySource,
    report: &mut LegacyImportReport,
) -> StorageResult<()> {
    let Some(snapshot) = source.snapshot.as_ref() else {
        report.topology_skipped_reason = Some("no legacy topology snapshot".to_string());
        return Ok(());
    };
    report.topology_snapshot_id = Some(snapshot.snapshot_id.clone());
    let exists: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM orchestrator_topology_heads WHERE topology_id = ?1)",
        [PRIMARY_TOPOLOGY_ID],
        |row| row.get(0),
    )?;
    if exists {
        report.topology_skipped_reason = Some(
            "v1 primary topology already exists; legacy snapshot was left untouched".to_string(),
        );
        return Ok(());
    }

    let revision = legacy_topology_revision(snapshot).map_err(StorageError::Domain)?;
    let revision_number = i64::try_from(revision.revision_number()).map_err(|_| {
        StorageError::Invariant("legacy topology revision number overflow".to_string())
    })?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_revisions(topology_id, revision_number, revision_id, parent_revision_id, rollback_of_revision_id, content_sha256, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            revision.topology_id(),
            revision_number,
            revision.revision_id(),
            revision.parent_revision_id(),
            revision.rollback_of_revision_id(),
            revision.content_sha256(),
            serde_json::to_string(&revision)?,
        ],
    )?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_heads(topology_id, draft_revision_id) VALUES (?1, ?2)",
        params![revision.topology_id(), revision.revision_id()],
    )?;
    let status = TopologyStatus::draft(
        revision.topology_id(),
        Some(revision.revision_id().to_string()),
        revision.created_at(),
    )
    .map_err(|error| StorageError::Domain(error.to_string()))?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_status(topology_id, desired_revision_id, observed_revision_id, payload) VALUES (?1, ?2, NULL, ?3)",
        params![
            revision.topology_id(),
            revision.revision_id(),
            serde_json::to_string(&status)?,
        ],
    )?;
    report.topology_revision_id = Some(revision.revision_id().to_string());
    Ok(())
}

fn import_sqlite_runtime_instances(
    transaction: &rusqlite::Transaction<'_>,
    source: &LegacySource,
    report: &mut LegacyImportReport,
) -> StorageResult<()> {
    for host_service in &source.host_services {
        let Some(instance) =
            legacy_runtime_instance(host_service, &source.endpoints, &source.nodes)
                .map_err(StorageError::Domain)?
        else {
            report.runtime_instances_skipped.push(format!(
                "{}:{} has no matching legacy endpoint",
                host_service.host_ip, host_service.service_name
            ));
            continue;
        };
        instance.validate()?;
        let inserted = transaction.execute(
            "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES (?1, ?2, ?3, 'RUNNING', 'UNKNOWN', ?4) ON CONFLICT(deployment_id) DO NOTHING",
            params![
                instance.instance.deployment_id,
                instance.node_id,
                instance.instance.service_id,
                serde_json::to_string(&instance)?,
            ],
        )?;
        report.runtime_instances_imported += inserted;
    }
    Ok(())
}

impl PostgresOrchestratorStore {
    pub(crate) fn import_legacy_v0_2(&self) -> PostgresResult<LegacyImportReport> {
        self.pool().with_client(|client| {
            let mut transaction = client.transaction()?;
            transaction.query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&IMPORT_ID],
            )?;
            if let Some(row) = transaction.query_opt(
                "SELECT report::text FROM orchestrator_legacy_imports WHERE import_id = $1",
                &[&IMPORT_ID],
            )? {
                return serde_json::from_str(&row.get::<_, String>(0)).map_err(Into::into);
            }

            let source = load_postgres_source(&mut transaction)?;
            let mut report = initial_report(&source);
            import_postgres_topology(&mut transaction, &source, &mut report)?;
            import_postgres_runtime_instances(&mut transaction, &source, &mut report)?;
            let encoded = serde_json::to_string(&report)?;
            transaction.execute(
                "INSERT INTO orchestrator_legacy_imports(import_id, source_checksum, report) VALUES ($1, $2, $3::text::jsonb)",
                &[&IMPORT_ID, &report.source_checksum, &encoded],
            )?;
            transaction.commit()?;
            Ok(report)
        })
    }

    pub fn legacy_import_report(&self) -> PostgresResult<LegacyImportReport> {
        self.pool().with_client(|client| {
            client
                .query_opt(
                    "SELECT report::text FROM orchestrator_legacy_imports WHERE import_id = $1",
                    &[&IMPORT_ID],
                )?
                .map(|row| {
                    serde_json::from_str(&row.get::<_, String>(0)).map_err(PostgresError::from)
                })
                .transpose()?
                .ok_or_else(|| {
                    PostgresError::Invariant("legacy import report is missing".to_string())
                })
        })
    }
}

fn load_postgres_source(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
) -> PostgresResult<LegacySource> {
    let rows = transaction.query(
        "SELECT kind, record_key, payload::text FROM orchestrator_records
         WHERE kind IN ($1, $2, $3, $4)
         ORDER BY CASE WHEN kind = $1 THEN 0 ELSE 1 END, updated_at DESC, record_key DESC",
        &[&TOPOLOGY_SNAPSHOTS, &HOST_SERVICES, &ENDPOINTS, &NODES],
    )?;
    let mut records = rows
        .into_iter()
        .map(|row| LegacyRecord {
            kind: row.get(0),
            key: row.get(1),
            payload: row.get(2),
        })
        .collect::<Vec<_>>();
    // 0.2 PostgreSQL deployments used normalized tables rather than the
    // record adapter. Read them only when present, keeping record-adapter rows
    // first so a mixed database deterministically prefers its newer source.
    append_normalized_postgres_sources(transaction, &mut records)?;
    LegacySource::decode(records).map_err(PostgresError::Domain)
}

fn append_normalized_postgres_sources(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    records: &mut Vec<LegacyRecord>,
) -> PostgresResult<()> {
    if postgres_table_exists(transaction, "topology_snapshots")? {
        for row in transaction.query(
            "SELECT snapshot_id, topology::text, created_at::text FROM topology_snapshots ORDER BY created_at DESC, snapshot_id DESC",
            &[],
        )? {
            let snapshot_id = row.get::<_, String>(0);
            let snapshot = TopologySnapshot {
                snapshot_id: snapshot_id.clone(),
                topology: serde_json::from_str(&row.get::<_, String>(1))?,
                created_at: row.get(2),
            };
            records.push(LegacyRecord {
                kind: TOPOLOGY_SNAPSHOTS.to_string(),
                key: snapshot_id,
                payload: serde_json::to_string(&snapshot)?,
            });
        }
    }
    if postgres_table_exists(transaction, "host_services")? {
        for row in transaction.query(
            "SELECT host_ip, service_name, version, status, config::text, labels::text, created_at::text, updated_at::text FROM host_services ORDER BY updated_at DESC, host_ip, service_name",
            &[],
        )? {
            let value = HostService {
                host_ip: row.get(0),
                service_name: row.get(1),
                version: row.get(2),
                status: row.get(3),
                config: serde_json::from_str(&row.get::<_, String>(4))?,
                labels: serde_json::from_str(&row.get::<_, String>(5))?,
                created_at: row.get(6),
                updated_at: row.get(7),
            };
            records.push(LegacyRecord {
                kind: HOST_SERVICES.to_string(),
                key: format!("{}:{}", value.host_ip, value.service_name),
                payload: serde_json::to_string(&value)?,
            });
        }
    }
    if postgres_table_exists(transaction, "service_endpoints")? {
        for row in transaction.query(
            "SELECT endpoint, service_id, protocol, health_path, status, reachable, display_name, note, config::text, created_at::text, updated_at::text FROM service_endpoints ORDER BY updated_at DESC, endpoint",
            &[],
        )? {
            let value = Endpoint {
                endpoint: row.get(0),
                service_id: row.get(1),
                protocol: row.get(2),
                health_path: row.get(3),
                health: row.get(4),
                reachable: row.get(5),
                display_name: row.get(6),
                note: row.get(7),
                config: serde_json::from_str(&row.get::<_, String>(8))?,
                created_at: row.get(9),
                updated_at: row.get(10),
            };
            records.push(LegacyRecord {
                kind: ENDPOINTS.to_string(),
                key: value.endpoint.clone(),
                payload: serde_json::to_string(&value)?,
            });
        }
    }
    if postgres_table_exists(transaction, "nodes")? {
        for row in transaction.query(
            "SELECT node_id, host_ip, parent_node_id, role, labels::text, status, created_at::text, updated_at::text FROM nodes ORDER BY updated_at DESC, node_id",
            &[],
        )? {
            let value = NodeRecord {
                node_id: row.get(0),
                host_ip: row.get(1),
                parent_node_id: row.get(2),
                role: row.get(3),
                labels: serde_json::from_str(&row.get::<_, String>(4))?,
                status: row.get(5),
                created_at: row.get(6),
                updated_at: row.get(7),
            };
            records.push(LegacyRecord {
                kind: NODES.to_string(),
                key: value.node_id.clone(),
                payload: serde_json::to_string(&value)?,
            });
        }
    }
    Ok(())
}

fn postgres_table_exists(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    table_name: &str,
) -> PostgresResult<bool> {
    Ok(transaction
        .query_one("SELECT to_regclass($1) IS NOT NULL", &[&table_name])?
        .get(0))
}

fn import_postgres_topology(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    source: &LegacySource,
    report: &mut LegacyImportReport,
) -> PostgresResult<()> {
    let Some(snapshot) = source.snapshot.as_ref() else {
        report.topology_skipped_reason = Some("no legacy topology snapshot".to_string());
        return Ok(());
    };
    report.topology_snapshot_id = Some(snapshot.snapshot_id.clone());
    let exists: bool = transaction
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM orchestrator_topology_heads WHERE topology_id = $1)",
            &[&PRIMARY_TOPOLOGY_ID],
        )?
        .get(0);
    if exists {
        report.topology_skipped_reason = Some(
            "v1 primary topology already exists; legacy snapshot was left untouched".to_string(),
        );
        return Ok(());
    }

    let revision = legacy_topology_revision(snapshot).map_err(PostgresError::Domain)?;
    let revision_number = i64::try_from(revision.revision_number()).map_err(|_| {
        PostgresError::Invariant("legacy topology revision number overflow".to_string())
    })?;
    let revision_payload = serde_json::to_string(&revision)?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_revisions(topology_id, revision_number, revision_id, parent_revision_id, rollback_of_revision_id, content_sha256, payload) VALUES ($1, $2, $3, $4, $5, $6, $7::text::jsonb)",
        &[
            &revision.topology_id(),
            &revision_number,
            &revision.revision_id(),
            &revision.parent_revision_id(),
            &revision.rollback_of_revision_id(),
            &revision.content_sha256(),
            &revision_payload,
        ],
    )?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_heads(topology_id, draft_revision_id) VALUES ($1, $2)",
        &[&revision.topology_id(), &revision.revision_id()],
    )?;
    let status = TopologyStatus::draft(
        revision.topology_id(),
        Some(revision.revision_id().to_string()),
        revision.created_at(),
    )
    .map_err(|error| PostgresError::Domain(error.to_string()))?;
    let status_payload = serde_json::to_string(&status)?;
    transaction.execute(
        "INSERT INTO orchestrator_topology_status(topology_id, desired_revision_id, observed_revision_id, payload) VALUES ($1, $2, NULL, $3::text::jsonb)",
        &[&revision.topology_id(), &revision.revision_id(), &status_payload],
    )?;
    report.topology_revision_id = Some(revision.revision_id().to_string());
    Ok(())
}

fn import_postgres_runtime_instances(
    transaction: &mut r2d2_postgres::postgres::Transaction<'_>,
    source: &LegacySource,
    report: &mut LegacyImportReport,
) -> PostgresResult<()> {
    for host_service in &source.host_services {
        let Some(instance) =
            legacy_runtime_instance(host_service, &source.endpoints, &source.nodes)
                .map_err(PostgresError::Domain)?
        else {
            report.runtime_instances_skipped.push(format!(
                "{}:{} has no matching legacy endpoint",
                host_service.host_ip, host_service.service_name
            ));
            continue;
        };
        instance
            .validate()
            .map_err(|error| PostgresError::Invariant(error.to_string()))?;
        let payload = serde_json::to_string(&instance)?;
        let inserted = transaction.execute(
            "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES ($1, $2, $3, 'RUNNING', 'UNKNOWN', $4::text::jsonb) ON CONFLICT(deployment_id) DO NOTHING",
            &[
                &instance.instance.deployment_id,
                &instance.node_id,
                &instance.instance.service_id,
                &payload,
            ],
        )?;
        report.runtime_instances_imported += inserted as usize;
    }
    Ok(())
}

fn initial_report(source: &LegacySource) -> LegacyImportReport {
    LegacyImportReport {
        import_id: IMPORT_ID.to_string(),
        source_checksum: source.checksum(),
        topology_snapshot_id: None,
        topology_revision_id: None,
        topology_skipped_reason: None,
        runtime_instances_imported: 0,
        runtime_instances_skipped: Vec::new(),
    }
}

fn legacy_topology_revision(snapshot: &TopologySnapshot) -> Result<TopologyRevision, String> {
    let topology = &snapshot.topology;
    let root_endpoint = if topology.root_endpoint.trim().is_empty() {
        topology.authority.root_endpoint.trim()
    } else {
        topology.root_endpoint.trim()
    };
    let exposure_policy = if topology.authority.exposure_policy.trim().is_empty() {
        "private"
    } else {
        topology.authority.exposure_policy.trim()
    };
    let endpoints = topology
        .endpoints
        .iter()
        .map(|endpoint| TopologyEndpointSpec {
            endpoint: endpoint.endpoint.clone(),
            service_id: endpoint.service_id.clone(),
            protocol: endpoint.protocol.clone(),
            health_path: endpoint.health_path.clone(),
            display_name: endpoint.display_name.clone(),
            note: endpoint.note.clone(),
            config: endpoint.config.clone(),
        })
        .collect();
    let links = topology
        .links
        .iter()
        .map(|link| TopologyLinkSpec {
            source_endpoint: link.source_endpoint.clone(),
            target_endpoint: link.target_endpoint.clone(),
            protocol: link.protocol.clone(),
            auth_mode: link.auth_mode.clone(),
            scope: link.scope.clone(),
            enabled: link.enabled,
            config_ref: link.config_ref.clone(),
            secret_ref: link.secret_ref.clone(),
            policy: link.policy.clone(),
            api_bindings: Vec::new(),
        })
        .collect();
    let spec = TopologySpec::new(
        PRIMARY_TOPOLOGY_ID,
        root_endpoint,
        exposure_policy,
        endpoints,
        links,
    )
    .map_err(|error| {
        format!(
            "legacy topology snapshot {} cannot be imported: {error}",
            snapshot.snapshot_id
        )
    })?;
    let created_at = nonempty(&snapshot.created_at, IMPORT_TIMESTAMP);
    TopologyRevision::initial(
        spec,
        created_at,
        IMPORT_ACTOR,
        format!(
            "Imported legacy topology snapshot {} as an unapplied draft",
            snapshot.snapshot_id
        ),
    )
    .map_err(|error| error.to_string())
}

fn legacy_runtime_instance(
    host_service: &HostService,
    endpoints: &[Endpoint],
    nodes: &[NodeRecord],
) -> Result<Option<StoredRuntimeInstance>, String> {
    if matches!(
        host_service.status.trim().to_ascii_uppercase().as_str(),
        "REMOVED" | "UNINSTALLED" | "DELETED"
    ) {
        return Ok(None);
    }
    let endpoint = endpoints.iter().find(|endpoint| {
        endpoint.service_id == host_service.service_name
            && parse_endpoint_id(&endpoint.endpoint)
                .map(|identity| identity.host == host_service.host_ip)
                .unwrap_or(false)
    });
    let Some(endpoint) = endpoint else {
        return Ok(None);
    };
    let node_id = nodes
        .iter()
        .find(|node| node.host_ip == host_service.host_ip)
        .map(|node| node.node_id.clone())
        .unwrap_or_else(|| "external".to_string());
    let deployment_id = legacy_deployment_id(
        &host_service.host_ip,
        &host_service.service_name,
        &endpoint.endpoint,
    );
    Ok(Some(StoredRuntimeInstance {
        node_id,
        instance: RuntimeInstance {
            deployment_id,
            service_id: host_service.service_name.clone(),
            release_version: String::new(),
            container_id: String::new(),
            // v0.2 never persisted a RepoDigest. An empty digest is honest for
            // an External/Unknown projection and prevents it being managed as
            // a Docker instance until the operator imports a signed v1 release.
            artifact_digest: String::new(),
            runtime_contract: RuntimeContract::standard_v1(),
            runtime_policy_sha256: String::new(),
            effective_runtime_sha256: String::new(),
            runtime_attested: false,
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Unknown,
            health: "UNKNOWN".to_string(),
        },
        management_mode: RuntimeManagementMode::External,
        endpoint: endpoint.endpoint.clone(),
        external_probe_protocol: String::new(),
        external_probe_health_path: String::new(),
        last_observed_at_ms: 0,
        drift_reason: "legacy runtime has no authenticated Agent observation".to_string(),
        credential_expires_at_ms: 0,
        credential_last_success_at_ms: 0,
        credential_last_error: String::new(),
        updated_at: nonempty(
            &host_service.updated_at,
            nonempty(&host_service.created_at, IMPORT_TIMESTAMP),
        )
        .to_string(),
    }))
}

fn legacy_deployment_id(host: &str, service: &str, endpoint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(host.as_bytes());
    hasher.update([0]);
    hasher.update(service.as_bytes());
    hasher.update([0]);
    hasher.update(endpoint.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("legacy-{}", &digest[..24])
}

fn nonempty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_legacy::{Link, Topology, TopologyAuthority};
    use rusqlite::Connection;
    use serde_json::json;
    use tempfile::tempdir;

    fn snapshot() -> TopologySnapshot {
        let gateway = Endpoint {
            endpoint: "127.0.0.1:8080:gateway".to_string(),
            service_id: "gateway".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "healthy".to_string(),
            reachable: true,
            display_name: "Gateway".to_string(),
            note: String::new(),
            config: json!({"desired": true}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let worker = Endpoint {
            endpoint: "127.0.0.2:8081:worker".to_string(),
            service_id: "worker".to_string(),
            protocol: "http".to_string(),
            health_path: "/health".to_string(),
            health: "unhealthy".to_string(),
            reachable: false,
            display_name: "Worker".to_string(),
            note: String::new(),
            config: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        TopologySnapshot {
            snapshot_id: "legacy-snapshot-1".to_string(),
            topology: Topology {
                root_host: "127.0.0.1".to_string(),
                root_endpoint: gateway.endpoint.clone(),
                authority: TopologyAuthority {
                    root_host: "127.0.0.1".to_string(),
                    root_endpoint: gateway.endpoint.clone(),
                    exposure_policy: "private".to_string(),
                    notes: Vec::new(),
                },
                services: vec!["gateway".to_string(), "worker".to_string()],
                endpoints: vec![gateway.clone(), worker.clone()],
                links: vec![Link {
                    source_endpoint: gateway.endpoint,
                    target_endpoint: worker.endpoint,
                    protocol: "http".to_string(),
                    auth_mode: "service".to_string(),
                    scope: "internal".to_string(),
                    enabled: true,
                    health: "unhealthy".to_string(),
                    latency_ms: Some(99),
                    config_ref: String::new(),
                    secret_ref: String::new(),
                    policy: json!({}),
                    created_at: String::new(),
                    updated_at: String::new(),
                }],
                operations: Vec::new(),
                log_views: Vec::new(),
                diagnostic_reports: Vec::new(),
            },
            created_at: "2026-08-03T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn conversion_strips_observed_fields_and_keeps_revision_unapplied() {
        let revision = legacy_topology_revision(&snapshot()).unwrap();
        assert_eq!(revision.topology_id(), PRIMARY_TOPOLOGY_ID);
        assert_eq!(revision.spec().endpoints.len(), 2);
        assert_eq!(revision.spec().links.len(), 1);
        assert!(
            !serde_json::to_string(revision.spec())
                .unwrap()
                .contains("unhealthy")
        );
    }

    #[test]
    fn legacy_runtime_is_external_unknown_without_invented_digest() {
        let endpoint = snapshot().topology.endpoints.remove(1);
        let host = HostService {
            host_ip: "127.0.0.2".to_string(),
            service_name: "worker".to_string(),
            version: "0.2.0".to_string(),
            status: "running".to_string(),
            config: json!({}),
            labels: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let instance = legacy_runtime_instance(&host, &[endpoint], &[])
            .unwrap()
            .unwrap();
        assert_eq!(instance.management_mode, RuntimeManagementMode::External);
        assert_eq!(
            instance.instance.observed_state,
            RuntimeObservedState::Unknown
        );
        assert!(instance.instance.artifact_digest.is_empty());
        instance.validate().unwrap();
    }

    #[test]
    fn sqlite_upgrade_imports_an_unapplied_draft_and_external_unknown_runtime_once() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("orchestrator.db");
        // Establish the v1 schema, then emulate a database that already
        // contained v0.2 records before this importer was introduced.
        drop(SqliteOrchestratorStore::open(&database).unwrap());

        let snapshot = snapshot();
        let host_service = HostService {
            host_ip: "127.0.0.2".to_string(),
            service_name: "worker".to_string(),
            version: "0.2.0".to_string(),
            status: "running".to_string(),
            config: json!({}),
            labels: json!({}),
            created_at: "2026-08-02T00:00:00Z".to_string(),
            updated_at: "2026-08-03T00:00:00Z".to_string(),
        };
        let endpoint = snapshot.topology.endpoints[1].clone();
        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, '', ?3)",
                params![
                    TOPOLOGY_SNAPSHOTS,
                    snapshot.snapshot_id,
                    serde_json::to_string(&snapshot).unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, '', ?3)",
                params![
                    HOST_SERVICES,
                    "127.0.0.2:worker",
                    serde_json::to_string(&host_service).unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO orchestrator_records(kind, record_key, scope, payload) VALUES (?1, ?2, '', ?3)",
                params![
                    ENDPOINTS,
                    endpoint.endpoint,
                    serde_json::to_string(&endpoint).unwrap()
                ],
            )
            .unwrap();
        connection
            .execute(
                "DELETE FROM orchestrator_legacy_imports WHERE import_id = ?1",
                [IMPORT_ID],
            )
            .unwrap();
        drop(connection);

        let store = SqliteOrchestratorStore::open(&database).unwrap();
        let report = store.legacy_import_report().unwrap();
        assert_eq!(
            report.topology_snapshot_id.as_deref(),
            Some("legacy-snapshot-1")
        );
        assert_eq!(report.runtime_instances_imported, 1);
        let heads = store.topology_heads(PRIMARY_TOPOLOGY_ID).unwrap().unwrap();
        assert!(heads.applied_revision_id.is_none());
        assert!(heads.applying_revision_id.is_none());
        assert_eq!(
            store
                .topology_status(PRIMARY_TOPOLOGY_ID)
                .unwrap()
                .unwrap()
                .observed_revision_id,
            None
        );
        let runtimes = store.runtime_instances(None).unwrap();
        assert_eq!(runtimes.len(), 1);
        assert_eq!(runtimes[0].management_mode, RuntimeManagementMode::External);
        assert_eq!(
            runtimes[0].instance.observed_state,
            RuntimeObservedState::Unknown
        );
        assert!(runtimes[0].instance.artifact_digest.is_empty());

        drop(store);
        let reopened = SqliteOrchestratorStore::open(&database).unwrap();
        assert_eq!(
            reopened
                .topology_revisions(PRIMARY_TOPOLOGY_ID)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(reopened.runtime_instances(None).unwrap().len(), 1);
    }
}
