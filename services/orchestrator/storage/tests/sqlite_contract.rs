use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, HeartbeatRequest, JobKind, JobStatus,
    JobStore, NewJob, NewJobEvent,
};
use orchestrator_legacy::{
    Endpoint, EndpointDecl, Link, LogView, NodeRecord, OrchestratorStore, RuntimeMode,
    ServiceHealthDecl, ServiceManifest, ServiceProvides, ServiceRelease, ServiceRequires,
    ServiceRuntimeDecl, ServiceSecurityDecl, SourceDecl, TopologyEndpointSpec, TopologyLinkSpec,
    TopologyReconciliationState, TopologySpec,
};
use orchestrator_storage::{
    AuditOutcome, NewAuditRecord, SqliteJobStore, SqliteOperationStore, SqliteOrchestratorStore,
    StorageError, TopologyApplyOutcome,
};
use serde_json::json;

#[test]
fn opens_with_verified_production_pragmas() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = SqliteOrchestratorStore::open(temp.path().join("orchestrator.db"))
        .expect("open sqlite store");
    let readiness = store.readiness().expect("readiness");
    assert_eq!(readiness.quick_check, "ok");
    assert_eq!(readiness.journal_mode.to_ascii_lowercase(), "wal");
    assert!(readiness.foreign_keys);
    assert_eq!(readiness.busy_timeout_ms, 5_000);
    assert_eq!(readiness.schema_version, readiness.expected_schema_version);
    assert!(!store.applied_migrations().expect("migrations").is_empty());
}

#[test]
fn audit_ledger_is_durable_ordered_and_database_enforced_append_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    let first_sequence;
    {
        let store = SqliteOrchestratorStore::open(&path).expect("open");
        let intent = store
            .append_audit_record(audit_record(AuditOutcome::Intent, None, 10))
            .expect("append intent");
        first_sequence = intent.sequence;
        let result = store
            .append_audit_record(audit_record(
                AuditOutcome::Succeeded,
                Some("operation-1"),
                11,
            ))
            .expect("append result");
        assert!(result.sequence > intent.sequence);
    }

    let store = SqliteOrchestratorStore::open(&path).expect("reopen");
    let records = store
        .audit_records(Some("request-1"), 0, 10)
        .expect("read audit records");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, first_sequence);
    assert_eq!(records[0].outcome, AuditOutcome::Intent);
    assert_eq!(records[1].outcome, AuditOutcome::Succeeded);
    assert_eq!(records[1].operation_id.as_deref(), Some("operation-1"));

    let raw = rusqlite::Connection::open(&path).expect("raw connection");
    assert!(
        raw.execute(
            "UPDATE orchestrator_audit_log SET actor = 'changed' WHERE sequence = ?1",
            [first_sequence],
        )
        .is_err()
    );
    assert!(
        raw.execute(
            "DELETE FROM orchestrator_audit_log WHERE sequence = ?1",
            [first_sequence],
        )
        .is_err()
    );
    assert_eq!(
        store
            .audit_records(Some("request-1"), 0, 10)
            .expect("audit rows remain")
            .len(),
        2
    );
}

fn audit_record(
    outcome: AuditOutcome,
    operation_id: Option<&str>,
    timestamp_ms: i64,
) -> NewAuditRecord {
    NewAuditRecord {
        request_id: "request-1".to_string(),
        actor: "desktop-admin".to_string(),
        action: "POST /api/v1/operations:plan".to_string(),
        resource: "/api/v1/operations:plan".to_string(),
        idempotency_key: "audit-contract-1".to_string(),
        request_digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
        outcome,
        response_status: (outcome != AuditOutcome::Intent).then_some(202),
        operation_id: operation_id.map(str::to_string),
        timestamp_ms,
    }
}

#[test]
fn rejects_schema_checksum_drift() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    {
        SqliteOrchestratorStore::open(&path).expect("initial open");
    }
    rusqlite::Connection::open(&path)
        .expect("raw connection")
        .execute(
            "UPDATE orchestrator_schema_migrations SET checksum = 'changed' WHERE version = 1",
            [],
        )
        .expect("change checksum");
    let error = SqliteOrchestratorStore::open(&path).expect_err("checksum drift must fail");
    assert!(matches!(
        error,
        StorageError::MigrationChecksum { version: 1, .. }
    ));
}

#[test]
fn readiness_rejects_missing_schema_objects() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    {
        SqliteOrchestratorStore::open(&path).expect("initial open");
    }
    rusqlite::Connection::open(&path)
        .expect("raw connection")
        .execute("DROP TABLE orchestrator_state", [])
        .expect("drop required table");
    let error = SqliteOrchestratorStore::open(&path).expect_err("missing table must fail");
    assert!(matches!(error, StorageError::MissingSchemaObject(_)));
}

#[test]
fn readiness_rejects_missing_append_only_audit_trigger() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    {
        SqliteOrchestratorStore::open(&path).expect("initial open");
    }
    rusqlite::Connection::open(&path)
        .expect("raw connection")
        .execute("DROP TRIGGER orchestrator_audit_log_no_delete", [])
        .expect("drop required trigger");
    let error = SqliteOrchestratorStore::open(&path).expect_err("missing trigger must fail");
    assert!(matches!(error, StorageError::MissingSchemaObject(_)));
}

#[test]
fn atomic_service_release_registration_rolls_back_service_on_release_failure() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    let mut store = SqliteOrchestratorStore::open(&path).expect("open");
    rusqlite::Connection::open(&path)
        .expect("raw connection")
        .execute_batch(
            "CREATE TRIGGER force_release_registration_failure BEFORE INSERT ON orchestrator_records WHEN NEW.kind = 'service-releases' BEGIN SELECT RAISE(ABORT, 'forced release failure'); END;",
        )
        .expect("failure trigger");
    let service = service("atomic-fixture", 8080);
    let release = ServiceRelease {
        service_name: service.id.clone(),
        version: service.version.clone(),
        release_url: "fixture.release.yaml".to_string(),
        manifest: json!({"service_name": service.id, "version": service.version}),
        checksum: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_string(),
        created_at: "t0".to_string(),
    };
    store
        .register_service_release_atomic(service.clone(), release.clone())
        .expect_err("forced release write must fail");
    assert!(
        store
            .get_service(&service.id)
            .expect("read service")
            .is_none(),
        "the Service insert must be rolled back with the Release insert"
    );
    assert!(
        store
            .get_service_release(&release.service_name, &release.version)
            .expect("read release")
            .is_none()
    );
}

#[test]
fn instance_lock_is_exclusive_and_released() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    let first = SqliteOrchestratorStore::open(&path).expect("first owner");
    let error = SqliteOrchestratorStore::open(&path).expect_err("second owner must fail");
    assert!(
        matches!(error, StorageError::AlreadyLocked(_)),
        "unexpected second-open error: {error:?}"
    );
    drop(first);
    SqliteOrchestratorStore::open(&path).expect("lock is released on drop");
}

#[test]
fn state_and_domain_records_survive_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    {
        let mut store = SqliteOrchestratorStore::open(&path).expect("open");
        store
            .put_state("layout:user-1", "topology-1", &json!({"x": 10, "y": 20}))
            .expect("put layout");
        store.upsert_node(root_node()).expect("put node");
    }
    let store = SqliteOrchestratorStore::open(&path).expect("reopen");
    assert_eq!(
        store
            .get_node("root")
            .expect("node read")
            .expect("node")
            .status,
        "online"
    );
    assert_eq!(
        store
            .get_state::<serde_json::Value>("layout:user-1", "topology-1")
            .expect("layout read"),
        Some(json!({"x": 10, "y": 20}))
    );
}

#[test]
fn topology_revisions_heads_and_status_survive_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    let (first_revision_id, rollback_revision_id) = {
        let store = SqliteOrchestratorStore::open(&path).expect("open");
        let first = store
            .create_initial_topology_revision(
                topology_spec("durable-topology", "first"),
                "unix-ms:1",
                "admin",
                "initial",
            )
            .expect("initial revision");
        store
            .begin_topology_apply(
                "durable-topology",
                first.revision_id(),
                "op-initial",
                "unix-ms:2",
            )
            .expect("begin apply");
        store
            .finish_topology_apply(
                "durable-topology",
                first.revision_id(),
                "op-initial",
                TopologyApplyOutcome::Succeeded,
                "unix-ms:3",
            )
            .expect("finish apply");
        let second = store
            .create_next_topology_revision(
                "durable-topology",
                first.revision_id(),
                topology_spec("durable-topology", "second"),
                "unix-ms:4",
                "admin",
                "edit",
            )
            .expect("second revision");
        let rollback = store
            .create_topology_rollback_revision(
                "durable-topology",
                second.revision_id(),
                first.revision_id(),
                "unix-ms:5",
                "admin",
                "rollback",
            )
            .expect("rollback revision");
        store
            .begin_topology_apply(
                "durable-topology",
                rollback.revision_id(),
                "op-rollback",
                "unix-ms:6",
            )
            .expect("begin rollback apply");
        store
            .finish_topology_apply(
                "durable-topology",
                rollback.revision_id(),
                "op-rollback",
                TopologyApplyOutcome::Succeeded,
                "unix-ms:7",
            )
            .expect("finish rollback apply");
        (
            first.revision_id().to_string(),
            rollback.revision_id().to_string(),
        )
    };

    let reopened = SqliteOrchestratorStore::open(&path).expect("reopen");
    let heads = reopened
        .topology_heads("durable-topology")
        .expect("heads")
        .expect("topology heads");
    assert_eq!(heads.draft_revision_id, rollback_revision_id);
    assert_eq!(
        heads.applied_revision_id.as_deref(),
        Some(rollback_revision_id.as_str())
    );
    assert!(heads.applying_revision_id.is_none());
    let revisions = reopened
        .topology_revisions("durable-topology")
        .expect("revisions");
    assert_eq!(revisions.len(), 3);
    let rollback = reopened
        .topology_revision("durable-topology", &rollback_revision_id)
        .expect("rollback lookup")
        .expect("rollback");
    assert_eq!(
        rollback.rollback_of_revision_id(),
        Some(first_revision_id.as_str())
    );
    let status = reopened
        .topology_status("durable-topology")
        .expect("status")
        .expect("topology status");
    assert_eq!(status.state, TopologyReconciliationState::InSync);
    assert_eq!(
        status.observed_revision_id.as_deref(),
        Some(rollback_revision_id.as_str())
    );
}

#[test]
fn endpoint_delete_cascades_link_and_log_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut store =
        SqliteOrchestratorStore::open(temp.path().join("orchestrator.db")).expect("open");
    store
        .upsert_service(service("source", 8080))
        .expect("source service");
    store
        .upsert_service(service("target", 8081))
        .expect("target service");
    let source = endpoint("127.0.0.1:8080:source", "source");
    let target = endpoint("127.0.0.1:8081:target", "target");
    store
        .upsert_endpoint(source.clone())
        .expect("source endpoint");
    store
        .upsert_endpoint(target.clone())
        .expect("target endpoint");
    store
        .upsert_link(Link {
            source_endpoint: source.endpoint.clone(),
            target_endpoint: target.endpoint.clone(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: String::new(),
            enabled: true,
            health: "unknown".to_string(),
            latency_ms: None,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: json!({}),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("link");
    store
        .upsert_log_source(LogView {
            source_id: "source-log".to_string(),
            endpoint: source.endpoint.clone(),
            service_id: "source".to_string(),
            operation_id: String::new(),
            path: "service.log".to_string(),
            driver: "file".to_string(),
            read_policy: "service-scoped".to_string(),
            display_name: "Source".to_string(),
        })
        .expect("log source");
    store
        .delete_endpoint(&source.endpoint)
        .expect("delete endpoint");
    assert!(store.list_links().expect("links").is_empty());
    assert!(store.list_log_sources().expect("logs").is_empty());
}

#[test]
fn job_lifecycle_events_and_restart_are_durable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("orchestrator.db");
    {
        let storage = SqliteOrchestratorStore::open(&path).expect("open");
        let mut jobs = SqliteJobStore::new(storage);
        let original = jobs.enqueue(new_job("1"), 0).expect("enqueue");
        assert_eq!(
            jobs.enqueue(new_job("1"), 1).expect("idempotent enqueue"),
            original
        );
        let leased = jobs
            .claim(claim_request("lease-1", 0))
            .expect("claim")
            .expect("job");
        assert_eq!(leased.status, JobStatus::Leased);
        let event = NewJobEvent {
            sequence: 1,
            event_type: "progress".to_string(),
            level: "info".to_string(),
            message: "pulled".to_string(),
            data: json!({"bytes": 10}),
        };
        jobs.heartbeat(HeartbeatRequest {
            job_id: "1".to_string(),
            lease_token: "lease-1".to_string(),
            now_ms: 10,
            lease_ms: 30_000,
            events: vec![event],
        })
        .expect("heartbeat");
        jobs.complete(CompleteRequest {
            job_id: "1".to_string(),
            lease_token: "lease-1".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({"container_id": "abc"}),
            error_message: String::new(),
            now_ms: 20,
            events: Vec::new(),
        })
        .expect("complete");
    }
    let storage = SqliteOrchestratorStore::open(&path).expect("reopen");
    let jobs = SqliteJobStore::new(storage);
    assert_eq!(
        jobs.get("1").expect("get").expect("job").status,
        JobStatus::Succeeded
    );
    assert_eq!(jobs.events("1", 0).expect("events").len(), 1);
}

#[test]
fn concurrent_claim_has_exactly_one_winner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = SqliteOrchestratorStore::open(temp.path().join("orchestrator.db")).expect("open");
    let mut jobs = SqliteJobStore::new(storage.clone());
    jobs.enqueue(new_job("only"), 0).expect("enqueue");
    let handles = (0..32)
        .map(|index| {
            let mut jobs = SqliteJobStore::new(storage.clone());
            std::thread::spawn(move || {
                jobs.claim(claim_request(&format!("lease-{index}"), 0))
                    .expect("claim")
                    .is_some()
            })
        })
        .collect::<Vec<_>>();
    let winners = handles
        .into_iter()
        .map(|handle| usize::from(handle.join().expect("claim thread")))
        .sum::<usize>();
    assert_eq!(winners, 1);
}

#[test]
fn expired_lease_requeues_then_requires_attention() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = SqliteOrchestratorStore::open(temp.path().join("orchestrator.db")).expect("open");
    let mut jobs = SqliteJobStore::new(storage);
    let mut job = new_job("retry");
    job.max_attempts = 2;
    jobs.enqueue(job, 0).expect("enqueue");
    jobs.claim(claim_request("lease-1", 0)).expect("claim");
    assert_eq!(
        jobs.recover_expired(30_000).expect("first recovery")[0].status,
        JobStatus::RetryWait
    );
    jobs.claim(claim_request("lease-2", 31_000))
        .expect("retry claim");
    assert_eq!(
        jobs.recover_expired(61_000).expect("second recovery")[0].status,
        JobStatus::NeedsAttention
    );
}

#[test]
fn expired_heartbeat_and_completion_recover_atomically_at_the_lease_boundary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = SqliteOrchestratorStore::open(temp.path().join("orchestrator.db")).expect("open");
    let mut jobs = SqliteJobStore::new(storage.clone());

    jobs.enqueue(new_job("expired-heartbeat"), 0)
        .expect("enqueue heartbeat fixture");
    jobs.claim(claim_request("current-heartbeat", 0))
        .expect("claim heartbeat fixture");
    assert_eq!(
        jobs.heartbeat(HeartbeatRequest {
            job_id: "expired-heartbeat".to_string(),
            lease_token: "old-token".to_string(),
            now_ms: 29_999,
            lease_ms: 30_000,
            events: Vec::new(),
        }),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert_eq!(
        jobs.get("expired-heartbeat")
            .expect("get live heartbeat fixture")
            .expect("heartbeat fixture")
            .status,
        JobStatus::Leased
    );
    assert_eq!(
        jobs.heartbeat(HeartbeatRequest {
            job_id: "expired-heartbeat".to_string(),
            lease_token: "old-token".to_string(),
            now_ms: 30_000,
            lease_ms: 30_000,
            events: vec![NewJobEvent {
                sequence: 1,
                event_type: "late".to_string(),
                level: "info".to_string(),
                message: "must not be committed".to_string(),
                data: json!({}),
            }],
        }),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert_eq!(
        jobs.get("expired-heartbeat")
            .expect("get recovered heartbeat fixture")
            .expect("heartbeat fixture")
            .status,
        JobStatus::RetryWait
    );
    assert!(
        jobs.events("expired-heartbeat", 0)
            .expect("read rejected heartbeat events")
            .is_empty()
    );

    jobs.enqueue(new_job("expired-complete"), 0)
        .expect("enqueue completion fixture");
    jobs.claim(claim_request("current-complete", 0))
        .expect("claim completion fixture");
    assert_eq!(
        jobs.complete(CompleteRequest {
            job_id: "expired-complete".to_string(),
            lease_token: "current-complete".to_string(),
            status: CompletionStatus::Succeeded,
            result: json!({}),
            error_message: String::new(),
            now_ms: 30_001,
            events: Vec::new(),
        }),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert_eq!(
        jobs.get("expired-complete")
            .expect("get recovered completion fixture")
            .expect("completion fixture")
            .status,
        JobStatus::RetryWait
    );

    jobs.enqueue(new_job("expired-race"), 0)
        .expect("enqueue race fixture");
    jobs.claim(claim_request("race-token", 0))
        .expect("claim race fixture");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let heartbeat = {
        let barrier = barrier.clone();
        let mut jobs = SqliteJobStore::new(storage.clone());
        std::thread::spawn(move || {
            barrier.wait();
            jobs.heartbeat(HeartbeatRequest {
                job_id: "expired-race".to_string(),
                lease_token: "race-token".to_string(),
                now_ms: 30_000,
                lease_ms: 30_000,
                events: Vec::new(),
            })
        })
    };
    let recovery = {
        let barrier = barrier.clone();
        let mut jobs = SqliteJobStore::new(storage.clone());
        std::thread::spawn(move || {
            barrier.wait();
            jobs.recover_expired(30_000)
        })
    };
    assert_eq!(
        heartbeat.join().expect("heartbeat thread"),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert!(recovery.join().expect("recovery thread").is_ok());
    assert_eq!(
        jobs.get("expired-race")
            .expect("get recovered race fixture")
            .expect("race fixture")
            .status,
        JobStatus::RetryWait
    );
    assert_eq!(
        SqliteOperationStore::new(storage)
            .anomaly_counters()
            .expect("read expired lease transition counter")
            .expired_job_lease_transitions_total,
        3
    );
}

fn new_job(id: &str) -> NewJob {
    NewJob {
        job_id: id.to_string(),
        operation_id: format!("op-{id}"),
        node_id: "node-a".to_string(),
        kind: JobKind::Install,
        payload: json!({"image": "registry/service@sha256:abc"}),
        idempotency_key: format!("key-{id}"),
        max_attempts: 3,
    }
}

fn claim_request(token: &str, now_ms: i64) -> ClaimRequest {
    ClaimRequest {
        node_id: "node-a".to_string(),
        instance_id: "worker-1".to_string(),
        lease_token: token.to_string(),
        now_ms,
        lease_ms: 30_000,
    }
}

fn root_node() -> NodeRecord {
    NodeRecord {
        node_id: "root".to_string(),
        host_ip: "127.0.0.1".to_string(),
        parent_node_id: String::new(),
        role: "root".to_string(),
        labels: json!({}),
        status: "online".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn endpoint(id: &str, service_id: &str) -> Endpoint {
    Endpoint {
        endpoint: id.to_string(),
        service_id: service_id.to_string(),
        protocol: "http".to_string(),
        health_path: "/health".to_string(),
        health: "unknown".to_string(),
        reachable: false,
        display_name: service_id.to_string(),
        note: String::new(),
        config: json!({}),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

fn service(id: &str, port: u16) -> ServiceManifest {
    ServiceManifest {
        schema_version: 1,
        name: id.to_string(),
        id: id.to_string(),
        version: "1.0.0".to_string(),
        kind: "backend-api".to_string(),
        description: String::new(),
        endpoint: EndpointDecl {
            protocol: "http".to_string(),
            default_port: port,
            health_path: "/health".to_string(),
            expose: true,
            routes: Vec::new(),
        },
        runtime: ServiceRuntimeDecl {
            mode: RuntimeMode::External,
            driver: "external-endpoint".to_string(),
            root_allowed: true,
            non_root_allowed: false,
            start_policy: "manual".to_string(),
            restart_policy: "manual".to_string(),
        },
        config_schema: json!({}),
        requires: ServiceRequires::default(),
        provides: ServiceProvides::default(),
        ui: Default::default(),
        permissions: Vec::new(),
        security: ServiceSecurityDecl::default(),
        source: SourceDecl {
            r#type: "local".to_string(),
            reference: "test".to_string(),
            build: json!({}),
            artifact: json!({}),
        },
        health: ServiceHealthDecl {
            checks: vec!["http".to_string()],
            timeout_seconds: 3,
            interval_seconds: 10,
        },
        resources: json!({}),
    }
}

fn topology_spec(topology_id: &str, note: &str) -> TopologySpec {
    TopologySpec::new(
        topology_id,
        "127.0.0.1:8080:gateway",
        "private",
        vec![
            TopologyEndpointSpec {
                endpoint: "127.0.0.1:8080:gateway".to_string(),
                service_id: "gateway".to_string(),
                protocol: "http".to_string(),
                health_path: "/health".to_string(),
                display_name: "Gateway".to_string(),
                note: note.to_string(),
                config: json!({}),
            },
            TopologyEndpointSpec {
                endpoint: "127.0.0.1:8081:worker".to_string(),
                service_id: "worker".to_string(),
                protocol: "http".to_string(),
                health_path: "/health".to_string(),
                display_name: "Worker".to_string(),
                note: String::new(),
                config: json!({}),
            },
        ],
        vec![TopologyLinkSpec {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8081:worker".to_string(),
            protocol: "http".to_string(),
            auth_mode: "internal".to_string(),
            scope: "worker.invoke".to_string(),
            enabled: true,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: json!({}),
        }],
    )
    .expect("valid topology spec")
}
