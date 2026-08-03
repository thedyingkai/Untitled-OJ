//! Backend-facing adapter over the two production persistence dialects.
//!
//! HTTP handlers use this type instead of branching on SQLite.  Keeping the
//! branch here makes it impossible for the PostgreSQL daemon to advertise a
//! capability whose mutation is rejected only because a route was wired to a
//! concrete SQLite type.

use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, DurableOperation, HeartbeatRequest, Job, JobError, JobEvent,
    JobStore, NewJob, OperationRepository, OperationStoreError,
};
use orchestrator_legacy::{
    NodeRecord, OrchestratorStore, ServiceReleaseManifest, TopologyRevision, TopologySpec,
    TopologyStatus, release_supports_link_probe_v1, validate_service_release,
};
use orchestrator_storage::{
    AuditRecord, CertificateActivation, CertificateRotation, ControlPlaneAnomalyCounters,
    EnrollmentLookup, EnrollmentRedemption, HistoryRetentionReport, IdempotencyBegin,
    JobMetricsSnapshot, NewAuditRecord, NewNodeCertificate, NodeCertificateRecord,
    NodeEnrollmentCode, PostgresError, PostgresJobStore, PostgresOperationStore,
    PostgresOrchestratorStore, SqliteJobStore, SqliteOperationStore, SqliteOrchestratorStore,
    StorageError, StoredIdempotentResponse, StoredRuntimeInstance, TopologyApplyOutcome,
    TopologyHeads,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone)]
pub(crate) enum DurableStore {
    Sqlite(SqliteOrchestratorStore),
    Postgres(PostgresOrchestratorStore),
}

impl DurableStore {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Sqlite(_) => "sqlite",
            Self::Postgres(_) => "postgres",
        }
    }

    pub(crate) fn readiness(&self) -> Result<Value, DurableError> {
        match self {
            Self::Sqlite(store) => {
                let report = store.readiness().map_err(DurableError::from)?;
                let legacy_import = store.legacy_import_report().map_err(DurableError::from)?;
                Ok(json!({
                    "store": "sqlite",
                    "schema_version": report.schema_version,
                    "expected_schema_version": report.expected_schema_version,
                    "journal_mode": report.journal_mode,
                    "foreign_keys": report.foreign_keys,
                    "legacy_import": legacy_import,
                }))
            }
            Self::Postgres(store) => {
                let report = store.readiness().map_err(DurableError::from)?;
                let legacy_import = store.legacy_import_report().map_err(DurableError::from)?;
                Ok(json!({
                    "store": "postgres",
                    "schema_version": report.schema_version,
                    "expected_schema_version": report.expected_schema_version,
                    "tls_enabled": report.tls_enabled,
                    "in_recovery": report.in_recovery,
                    "pool_connections": report.pool_connections,
                    "pool_idle_connections": report.pool_idle_connections,
                    "legacy_import": legacy_import,
                }))
            }
        }
    }

    pub(crate) fn as_sqlite(&self) -> Option<&SqliteOrchestratorStore> {
        match self {
            Self::Sqlite(store) => Some(store),
            Self::Postgres(_) => None,
        }
    }

    pub(crate) fn job_store(&self) -> DurableJobStore {
        match self {
            Self::Sqlite(store) => DurableJobStore::Sqlite(SqliteJobStore::new(store.clone())),
            Self::Postgres(store) => {
                DurableJobStore::Postgres(PostgresJobStore::new(store.clone()))
            }
        }
    }

    pub(crate) fn operation_store(&self) -> DurableOperationStore {
        match self {
            Self::Sqlite(store) => {
                DurableOperationStore::Sqlite(SqliteOperationStore::new(store.clone()))
            }
            Self::Postgres(store) => {
                DurableOperationStore::Postgres(PostgresOperationStore::new(store.clone()))
            }
        }
    }

    pub(crate) fn begin_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        now_ms: i64,
    ) -> Result<IdempotencyBegin, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .begin_idempotent_request(scope, key, request_sha256, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .begin_idempotent_request(scope, key, request_sha256, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn complete_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
        response: &StoredIdempotentResponse,
        now_ms: i64,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .complete_idempotent_request(scope, key, request_sha256, response, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .complete_idempotent_request(scope, key, request_sha256, response, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn abort_idempotent_request(
        &self,
        scope: &str,
        key: &str,
        request_sha256: &str,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .abort_idempotent_request(scope, key, request_sha256)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .abort_idempotent_request(scope, key, request_sha256)
                .map_err(Into::into),
        }
    }

    pub(crate) fn append_audit_record(
        &self,
        record: NewAuditRecord,
    ) -> Result<AuditRecord, DurableError> {
        match self {
            Self::Sqlite(store) => store.append_audit_record(record).map_err(Into::into),
            Self::Postgres(store) => store.append_audit_record(record).map_err(Into::into),
        }
    }

    #[cfg(test)]
    pub(crate) fn audit_records(
        &self,
        request_id: Option<&str>,
        after_sequence: u64,
        limit: u32,
    ) -> Result<Vec<AuditRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .audit_records(request_id, after_sequence, limit)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .audit_records(request_id, after_sequence, limit)
                .map_err(Into::into),
        }
    }

    pub(crate) fn registered_service_ids(&self) -> Result<BTreeSet<String>, DurableError> {
        let services = match self {
            Self::Sqlite(store) => store.list_services(),
            Self::Postgres(store) => store.list_services(),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))?;
        Ok(services.into_iter().map(|service| service.id).collect())
    }

    pub(crate) fn link_probe_source_endpoints(
        &self,
        spec: &TopologySpec,
    ) -> Result<BTreeSet<String>, LinkProbeBindingError> {
        let runtime_instances = self
            .runtime_instances(None)
            .map_err(|error| LinkProbeBindingError::Storage(error.to_string()))?;
        // One runtime scan and one release query per validation/reconcile,
        // independent of Link cardinality. The two indexes below make the
        // 2,000 Endpoint / 8,000 Link production shape O(E + R + L), while a
        // per-release cache parses each signed manifest at most once.
        let mut runtime_by_endpoint_service =
            BTreeMap::<(&str, &str), Vec<&StoredRuntimeInstance>>::new();
        for stored in &runtime_instances {
            runtime_by_endpoint_service
                .entry((
                    stored.endpoint.as_str(),
                    stored.instance.service_id.as_str(),
                ))
                .or_default()
                .push(stored);
        }
        let releases = match self {
            Self::Sqlite(store) => store.list_service_releases(),
            Self::Postgres(store) => store.list_service_releases(),
        }
        .map_err(|error| LinkProbeBindingError::Storage(error.to_string()))?;
        let releases_by_identity = releases
            .iter()
            .map(|release| {
                (
                    (release.service_name.as_str(), release.version.as_str()),
                    release,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let endpoint_services = spec
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.endpoint.as_str(), endpoint.service_id.as_str()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut capable_endpoints = BTreeSet::new();
        let mut release_capability_cache = BTreeMap::<(&str, &str), Result<bool, String>>::new();
        for link in spec.links.iter().filter(|link| link.enabled) {
            let service_id = endpoint_services
                .get(link.source_endpoint.as_str())
                .ok_or_else(|| {
                    LinkProbeBindingError::Binding(format!(
                        "link source endpoint {} does not exist",
                        link.source_endpoint
                    ))
                })?;
            let matching = runtime_by_endpoint_service
                .get(&(link.source_endpoint.as_str(), *service_id))
                .map(Vec::as_slice)
                .unwrap_or_default();
            if matching.len() != 1 {
                return Err(LinkProbeBindingError::Binding(format!(
                    "link {} -> {} requires exactly one runtime projection bound to source endpoint {} and service {}; found {}",
                    link.source_endpoint,
                    link.target_endpoint,
                    link.source_endpoint,
                    service_id,
                    matching.len()
                )));
            }
            let release_version = matching[0].instance.release_version.trim();
            if release_version.is_empty() {
                return Err(LinkProbeBindingError::Binding(format!(
                    "link source endpoint {} has no deterministic release version binding; controlled reprovision is required",
                    link.source_endpoint
                )));
            }
            let capability = release_capability_cache
                .entry((*service_id, release_version))
                .or_insert_with(|| {
                    let release = releases_by_identity
                        .get(&(*service_id, release_version))
                        .ok_or_else(|| "the exact release record is missing".to_string())?;
                    let manifest: ServiceReleaseManifest =
                        serde_json::from_value(release.manifest.clone())
                            .map_err(|error| format!("release manifest is invalid: {error}"))?;
                    validate_service_release(&manifest)
                        .map_err(|error| format!("release manifest is invalid: {error}"))?;
                    if manifest.service_name.as_str() != *service_id
                        || manifest.version != release_version
                    {
                        return Err(
                            "release record identity does not match its manifest".to_string()
                        );
                    }
                    Ok(release_supports_link_probe_v1(&manifest))
                })
                .clone()
                .map_err(|detail| {
                    LinkProbeBindingError::Binding(format!(
                        "link source endpoint {} is bound to invalid release {}@{}: {detail}",
                        link.source_endpoint, service_id, release_version
                    ))
                })?;
            if !capability {
                return Err(LinkProbeBindingError::Capability(format!(
                    "link {} -> {} source release {}@{} does not declare orchestrator.link-probe.v1",
                    link.source_endpoint, link.target_endpoint, service_id, release_version
                )));
            }
            capable_endpoints.insert(link.source_endpoint.clone());
        }
        Ok(capable_endpoints)
    }

    pub(crate) fn get_node(&self, node_id: &str) -> Result<Option<NodeRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.get_node(node_id),
            Self::Postgres(store) => store.get_node(node_id),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn list_nodes(&self) -> Result<Vec<NodeRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.list_nodes(),
            Self::Postgres(store) => store.list_nodes(),
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn upsert_node(&self, node: NodeRecord) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => {
                let mut store = store.clone();
                store.upsert_node(node)
            }
            Self::Postgres(store) => {
                let mut store = store.clone();
                store.upsert_node(node)
            }
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn delete_node(&self, node_id: &str) -> Result<(), DurableError> {
        // Removal is also an identity boundary. Revocation is committed before
        // the node record disappears so an already-open certificate cannot be
        // used to claim work after administrative removal.
        self.revoke_node_certificates(node_id, current_time_ms(), "node removed")?;
        match self {
            Self::Sqlite(store) => {
                let mut store = store.clone();
                store.delete_node(node_id)
            }
            Self::Postgres(store) => {
                let mut store = store.clone();
                store.delete_node(node_id)
            }
        }
        .map_err(|error| DurableError::Storage(error.to_string()))
    }

    pub(crate) fn register_node_enrollment(
        &self,
        node: &NodeRecord,
        code: &NodeEnrollmentCode,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .register_node_enrollment(node, code)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .register_node_enrollment(node, code)
                .map_err(Into::into),
        }
    }

    #[cfg(test)]
    pub(crate) fn node_enrollment_code_by_digest(
        &self,
        digest: &str,
    ) -> Result<Option<NodeEnrollmentCode>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .node_enrollment_code_by_digest(digest)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .node_enrollment_code_by_digest(digest)
                .map_err(Into::into),
        }
    }

    pub(crate) fn lookup_node_enrollment(
        &self,
        digest: &str,
        csr_sha256: &str,
    ) -> Result<EnrollmentLookup, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .lookup_node_enrollment(digest, csr_sha256)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .lookup_node_enrollment(digest, csr_sha256)
                .map_err(Into::into),
        }
    }

    pub(crate) fn redeem_node_enrollment_code(
        &self,
        digest: &str,
        csr_sha256: &str,
        now_ms: i64,
        certificate: NewNodeCertificate,
    ) -> Result<EnrollmentRedemption, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .redeem_node_enrollment_code(digest, csr_sha256, now_ms, certificate)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .redeem_node_enrollment_code(digest, csr_sha256, now_ms, certificate)
                .map_err(Into::into),
        }
    }

    pub(crate) fn node_certificate(
        &self,
        serial_hex: &str,
    ) -> Result<Option<NodeCertificateRecord>, DurableError> {
        match self {
            Self::Sqlite(store) => store.node_certificate(serial_hex).map_err(Into::into),
            Self::Postgres(store) => store.node_certificate(serial_hex).map_err(Into::into),
        }
    }

    pub(crate) fn rotate_node_certificate(
        &self,
        current_serial: &str,
        node_id: &str,
        now_ms: i64,
        replacement: NewNodeCertificate,
    ) -> Result<CertificateRotation, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .rotate_node_certificate(current_serial, node_id, now_ms, replacement)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .rotate_node_certificate(current_serial, node_id, now_ms, replacement)
                .map_err(Into::into),
        }
    }

    pub(crate) fn revoke_node_certificates(
        &self,
        node_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<u64, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .revoke_node_certificates(node_id, now_ms, reason)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .revoke_node_certificates(node_id, now_ms, reason)
                .map_err(Into::into),
        }
    }

    pub(crate) fn activate_node_certificate(
        &self,
        node_id: &str,
        current_serial: &str,
        now_ms: i64,
    ) -> Result<CertificateActivation, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .activate_node_certificate(node_id, current_serial, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .activate_node_certificate(node_id, current_serial, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_initial_topology_revision(
        &self,
        spec: TopologySpec,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_initial_topology_revision(spec, created_at, created_by, message)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_initial_topology_revision(spec, created_at, created_by, message)
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_next_topology_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        spec: TopologySpec,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_next_topology_revision(
                    topology_id,
                    expected_draft_revision_id,
                    spec,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_next_topology_revision(
                    topology_id,
                    expected_draft_revision_id,
                    spec,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn create_topology_rollback_revision(
        &self,
        topology_id: &str,
        expected_draft_revision_id: &str,
        rollback_to_revision_id: &str,
        created_at: String,
        created_by: String,
        message: String,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .create_topology_rollback_revision(
                    topology_id,
                    expected_draft_revision_id,
                    rollback_to_revision_id,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
            Self::Postgres(store) => store
                .create_topology_rollback_revision(
                    topology_id,
                    expected_draft_revision_id,
                    rollback_to_revision_id,
                    created_at,
                    created_by,
                    message,
                )
                .map_err(Into::into),
        }
    }

    pub(crate) fn begin_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        updated_at: &str,
    ) -> Result<TopologyRevision, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .begin_topology_apply(topology_id, revision_id, operation_id, updated_at)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .begin_topology_apply(topology_id, revision_id, operation_id, updated_at)
                .map_err(Into::into),
        }
    }

    pub(crate) fn finish_topology_apply(
        &self,
        topology_id: &str,
        revision_id: &str,
        operation_id: &str,
        outcome: TopologyApplyOutcome,
        updated_at: &str,
    ) -> Result<TopologyHeads, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .finish_topology_apply(topology_id, revision_id, operation_id, outcome, updated_at)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .finish_topology_apply(topology_id, revision_id, operation_id, outcome, updated_at)
                .map_err(Into::into),
        }
    }

    pub(crate) fn topology_heads(
        &self,
        topology_id: &str,
    ) -> Result<Option<TopologyHeads>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_heads(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_heads(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn list_topology_heads(&self) -> Result<Vec<TopologyHeads>, DurableError> {
        match self {
            Self::Sqlite(store) => store.list_topology_heads().map_err(Into::into),
            Self::Postgres(store) => store.list_topology_heads().map_err(Into::into),
        }
    }

    pub(crate) fn topology_revision(
        &self,
        topology_id: &str,
        revision_id: &str,
    ) -> Result<Option<TopologyRevision>, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .topology_revision(topology_id, revision_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .topology_revision(topology_id, revision_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn topology_revisions(
        &self,
        topology_id: &str,
    ) -> Result<Vec<TopologyRevision>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_revisions(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_revisions(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn topology_status(
        &self,
        topology_id: &str,
    ) -> Result<Option<TopologyStatus>, DurableError> {
        match self {
            Self::Sqlite(store) => store.topology_status(topology_id).map_err(Into::into),
            Self::Postgres(store) => store.topology_status(topology_id).map_err(Into::into),
        }
    }

    pub(crate) fn put_topology_status(&self, status: &TopologyStatus) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_topology_status(status).map_err(Into::into),
            Self::Postgres(store) => store.put_topology_status(status).map_err(Into::into),
        }
    }

    pub(crate) fn put_reconciled_topology_status(
        &self,
        status: &TopologyStatus,
        expected_applied_revision_id: &str,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .put_reconciled_topology_status(status, expected_applied_revision_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .put_reconciled_topology_status(status, expected_applied_revision_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn runtime_instance(
        &self,
        deployment_id: &str,
    ) -> Result<Option<StoredRuntimeInstance>, DurableError> {
        match self {
            Self::Sqlite(store) => store.runtime_instance(deployment_id).map_err(Into::into),
            Self::Postgres(store) => store.runtime_instance(deployment_id).map_err(Into::into),
        }
    }

    pub(crate) fn put_runtime_instance(
        &self,
        value: &StoredRuntimeInstance,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_runtime_instance(value).map_err(Into::into),
            Self::Postgres(store) => store.put_runtime_instance(value).map_err(Into::into),
        }
    }

    pub(crate) fn delete_runtime_instance(
        &self,
        deployment_id: &str,
    ) -> Result<bool, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .delete_runtime_instance(deployment_id)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .delete_runtime_instance(deployment_id)
                .map_err(Into::into),
        }
    }

    pub(crate) fn replace_runtime_instance(
        &self,
        replaced_deployment_id: &str,
        value: &StoredRuntimeInstance,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store
                .replace_runtime_instance(replaced_deployment_id, value)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .replace_runtime_instance(replaced_deployment_id, value)
                .map_err(Into::into),
        }
    }

    pub(crate) fn runtime_instances(
        &self,
        node_id: Option<&str>,
    ) -> Result<Vec<StoredRuntimeInstance>, DurableError> {
        match self {
            Self::Sqlite(store) => store.runtime_instances(node_id).map_err(Into::into),
            Self::Postgres(store) => store.runtime_instances(node_id).map_err(Into::into),
        }
    }

    pub(crate) fn purge_terminal_history(
        &self,
        completed_before_ms: i64,
        now_ms: i64,
    ) -> Result<HistoryRetentionReport, DurableError> {
        match self {
            Self::Sqlite(store) => store
                .purge_terminal_history(completed_before_ms, now_ms)
                .map_err(Into::into),
            Self::Postgres(store) => store
                .purge_terminal_history(completed_before_ms, now_ms)
                .map_err(Into::into),
        }
    }

    pub(crate) fn put_state<T: Serialize>(
        &self,
        namespace: &str,
        key: &str,
        value: &T,
    ) -> Result<(), DurableError> {
        match self {
            Self::Sqlite(store) => store.put_state(namespace, key, value).map_err(Into::into),
            Self::Postgres(store) => store.put_state(namespace, key, value).map_err(Into::into),
        }
    }

    pub(crate) fn get_state<T: DeserializeOwned>(
        &self,
        namespace: &str,
        key: &str,
    ) -> Result<Option<T>, DurableError> {
        match self {
            Self::Sqlite(store) => store.get_state(namespace, key).map_err(Into::into),
            Self::Postgres(store) => store.get_state(namespace, key).map_err(Into::into),
        }
    }
}

fn current_time_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Debug)]
pub(crate) enum DurableJobStore {
    Sqlite(SqliteJobStore),
    Postgres(PostgresJobStore),
}

impl DurableJobStore {
    pub(crate) fn active_job_count(&self, node_id: &str) -> Result<u64, JobError> {
        match self {
            Self::Sqlite(store) => store.active_job_count(node_id),
            Self::Postgres(store) => store.active_job_count(node_id),
        }
    }

    pub(crate) fn metrics_snapshot(&self, now_ms: i64) -> Result<JobMetricsSnapshot, JobError> {
        match self {
            Self::Sqlite(store) => store.metrics_snapshot(now_ms),
            Self::Postgres(store) => store.metrics_snapshot(now_ms),
        }
    }
}

impl JobStore for DurableJobStore {
    fn enqueue(&mut self, job: NewJob, now_ms: i64) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.enqueue(job, now_ms),
            Self::Postgres(store) => store.enqueue(job, now_ms),
        }
    }

    fn claim(&mut self, request: ClaimRequest) -> Result<Option<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.claim(request),
            Self::Postgres(store) => store.claim(request),
        }
    }

    fn heartbeat(&mut self, request: HeartbeatRequest) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.heartbeat(request),
            Self::Postgres(store) => store.heartbeat(request),
        }
    }

    fn complete(&mut self, request: CompleteRequest) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.complete(request),
            Self::Postgres(store) => store.complete(request),
        }
    }

    fn request_cancel(&mut self, job_id: &str, now_ms: i64) -> Result<Job, JobError> {
        match self {
            Self::Sqlite(store) => store.request_cancel(job_id, now_ms),
            Self::Postgres(store) => store.request_cancel(job_id, now_ms),
        }
    }

    fn recover_expired(&mut self, now_ms: i64) -> Result<Vec<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.recover_expired(now_ms),
            Self::Postgres(store) => store.recover_expired(now_ms),
        }
    }

    fn get(&self, job_id: &str) -> Result<Option<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.get(job_id),
            Self::Postgres(store) => store.get(job_id),
        }
    }

    fn list(&self) -> Result<Vec<Job>, JobError> {
        match self {
            Self::Sqlite(store) => store.list(),
            Self::Postgres(store) => store.list(),
        }
    }

    fn events(&self, job_id: &str, after_sequence: u64) -> Result<Vec<JobEvent>, JobError> {
        match self {
            Self::Sqlite(store) => store.events(job_id, after_sequence),
            Self::Postgres(store) => store.events(job_id, after_sequence),
        }
    }
}

#[derive(Debug)]
pub(crate) enum DurableOperationStore {
    Sqlite(SqliteOperationStore),
    Postgres(PostgresOperationStore),
}

impl DurableOperationStore {
    pub(crate) fn anomaly_candidates(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.anomaly_candidates(),
            Self::Postgres(store) => store.anomaly_candidates(),
        }
    }

    pub(crate) fn observe_active_operation_anomalies(
        &self,
        candidates: &[DurableOperation],
        now_ms: i64,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.observe_active_operation_anomalies(candidates, now_ms),
            Self::Postgres(store) => store.observe_active_operation_anomalies(candidates, now_ms),
        }
    }

    pub(crate) fn migrate_legacy_anomaly_state(
        &self,
        expired_leases: u64,
        long_operations: u64,
        expired_lease_episodes: &std::collections::BTreeMap<String, String>,
        long_operation_episodes: &std::collections::BTreeSet<String>,
    ) -> Result<ControlPlaneAnomalyCounters, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.migrate_legacy_anomaly_state(
                expired_leases,
                long_operations,
                expired_lease_episodes,
                long_operation_episodes,
            ),
            Self::Postgres(store) => store.migrate_legacy_anomaly_state(
                expired_leases,
                long_operations,
                expired_lease_episodes,
                long_operation_episodes,
            ),
        }
    }
}

impl OperationRepository for DurableOperationStore {
    fn create(
        &mut self,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.create(operation),
            Self::Postgres(store) => store.create(operation),
        }
    }

    fn get(&self, operation_id: &str) -> Result<Option<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.get(operation_id),
            Self::Postgres(store) => store.get(operation_id),
        }
    }

    fn compare_and_swap(
        &mut self,
        expected_revision: u64,
        operation: DurableOperation,
    ) -> Result<DurableOperation, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.compare_and_swap(expected_revision, operation),
            Self::Postgres(store) => store.compare_and_swap(expected_revision, operation),
        }
    }

    fn recoverable(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.recoverable(),
            Self::Postgres(store) => store.recoverable(),
        }
    }

    fn list(&self) -> Result<Vec<DurableOperation>, OperationStoreError> {
        match self {
            Self::Sqlite(store) => store.list(),
            Self::Postgres(store) => store.list(),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum DurableError {
    #[error("optimistic concurrency conflict: {0}")]
    Conflict(String),
    #[error("storage invariant failed: {0}")]
    Invariant(String),
    #[error("domain validation failed: {0}")]
    Domain(String),
    #[error("durable storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Error)]
pub(crate) enum LinkProbeBindingError {
    #[error("{0}")]
    Binding(String),
    #[error("{0}")]
    Capability(String),
    #[error("durable storage error: {0}")]
    Storage(String),
}

impl From<StorageError> for DurableError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::Conflict(detail) => Self::Conflict(detail),
            StorageError::Invariant(detail) => Self::Invariant(detail),
            StorageError::Domain(detail) => Self::Domain(detail),
            other => Self::Storage(other.to_string()),
        }
    }
}

impl From<PostgresError> for DurableError {
    fn from(error: PostgresError) -> Self {
        match error {
            PostgresError::Conflict(detail) => Self::Conflict(detail),
            PostgresError::Invariant(detail) => Self::Invariant(detail),
            PostgresError::Domain(detail) => Self::Domain(detail),
            other => Self::Storage(other.to_string()),
        }
    }
}
