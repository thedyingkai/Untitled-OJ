use orchestrator_control_plane::{
    ClaimRequest, CompleteRequest, CompletionStatus, DurableOperation, DurableOperationMode,
    DurableOperationStatus, HeartbeatRequest, JobKind, JobStatus, JobStore, MemoryJobStore, NewJob,
    NewJobEvent, OPERATION_SCHEMA_VERSION, OperationCoordinator, OperationRepository,
    OperationStoreError, PlanOperation, PlannedJob, ResolveExpiredSuccessRequest,
};
use orchestrator_legacy::{
    ApiBinding, ApiBindingState, NodeRecord, OrchestratorStore, TopologyEndpointSpec,
    TopologyLinkSpec, TopologySpec,
};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance, RuntimeObservedState};
use orchestrator_storage::{
    AuditOutcome, CERTIFICATE_LIFETIME_MS, EnrollmentRedemption, IdempotencyBegin, NewAuditRecord,
    NewNodeCertificate, NodeEnrollmentCode, PostgresError, PostgresJobStore,
    PostgresOperationStore, PostgresOptions, PostgresOrchestratorStore, PostgresTlsTrust,
    StoredIdempotentResponse, StoredNodeRuntimeFacts, StoredRuntimeInstance,
    TopologyApplyGroupMember, TopologyApplyOutcome,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Barrier},
    time::{SystemTime, UNIX_EPOCH},
};

/// Opt-in integration contract. CI can provide a dedicated TLS database with
/// `OJOS_TEST_POSTGRES_URL`; developer machines without PostgreSQL still run
/// all pure schema/configuration tests.
#[test]
fn postgres_repository_contract_when_configured() {
    let Some(database_url) = std::env::var("OJOS_TEST_POSTGRES_URL").ok() else {
        return;
    };
    let mut options = PostgresOptions {
        max_size: 40,
        min_idle: 2,
        ..PostgresOptions::default()
    };
    if let Ok(path) = std::env::var("OJOS_TEST_POSTGRES_CA") {
        options.tls_trust = PostgresTlsTrust::CaCertificate(PathBuf::from(path));
    }
    let reconnect_options = options.clone();
    let mut store = PostgresOrchestratorStore::connect(&database_url, options)
        .expect("connect dedicated PostgreSQL test database");
    let readiness = store.readiness().expect("readiness");
    assert!(readiness.tls_enabled);
    assert!(!readiness.in_recovery);
    assert_eq!(readiness.schema_version, readiness.expected_schema_version);

    let suffix = unique_suffix();
    let audit_request_id = format!("pg-audit-request-{suffix}");
    let audit_key = format!("pg-audit-key-{suffix}");
    let intent = store
        .append_audit_record(NewAuditRecord {
            request_id: audit_request_id.clone(),
            actor: "contract-admin".to_string(),
            action: "POST /api/v1/operations:plan".to_string(),
            resource: "/api/v1/operations:plan".to_string(),
            idempotency_key: audit_key.clone(),
            request_digest:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
            outcome: AuditOutcome::Intent,
            response_status: None,
            operation_id: None,
            timestamp_ms: 10,
        })
        .expect("append PostgreSQL audit intent");
    let result = store
        .append_audit_record(NewAuditRecord {
            request_id: audit_request_id.clone(),
            actor: "contract-admin".to_string(),
            action: "POST /api/v1/operations:plan".to_string(),
            resource: "/api/v1/operations:plan".to_string(),
            idempotency_key: audit_key,
            request_digest:
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                    .to_string(),
            outcome: AuditOutcome::Succeeded,
            response_status: Some(202),
            operation_id: Some(format!("pg-audit-operation-{suffix}")),
            timestamp_ms: 11,
        })
        .expect("append PostgreSQL audit result");
    assert!(result.sequence > intent.sequence);
    let audit_records = store
        .audit_records(Some(&audit_request_id), 0, 10)
        .expect("read PostgreSQL audit records");
    assert_eq!(audit_records.len(), 2);
    assert_eq!(audit_records[0].outcome, AuditOutcome::Intent);
    assert_eq!(audit_records[1].outcome, AuditOutcome::Succeeded);

    let state_key = format!("state-{suffix}");
    store
        .put_state("postgres-contract", &state_key, &json!({"durable": true}))
        .expect("write UI state");
    assert_eq!(
        store
            .get_state::<serde_json::Value>("postgres-contract", &state_key)
            .expect("read UI state"),
        Some(json!({"durable": true}))
    );

    let node_id = format!("pg-node-{suffix}");
    let host_ip = format!(
        "10.200.{}.{}",
        (suffix % 250) + 1,
        ((suffix / 250) % 250) + 1
    );
    store
        .upsert_node(NodeRecord {
            node_id: node_id.clone(),
            host_ip,
            parent_node_id: String::new(),
            role: "standalone".to_string(),
            labels: json!({"test": true}),
            status: "online".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        })
        .expect("persist node");
    assert!(store.get_node(&node_id).expect("read node").is_some());

    let job_id = format!("pg-job-{suffix}");
    let job = NewJob {
        job_id: job_id.clone(),
        operation_id: format!("pg-operation-{suffix}"),
        node_id: node_id.clone(),
        kind: JobKind::Install,
        payload: json!({"image": "registry/service@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        idempotency_key: format!("pg-key-{suffix}"),
        max_attempts: 3,
    };
    let mut jobs = PostgresJobStore::new(store.clone());
    let enqueued = jobs.enqueue(job.clone(), 0).expect("enqueue");
    assert_eq!(jobs.enqueue(job, 1).expect("idempotent enqueue"), enqueued);

    let handles = (0..32)
        .map(|index| {
            let mut jobs = PostgresJobStore::new(store.clone());
            let node_id = node_id.clone();
            std::thread::spawn(move || {
                jobs.claim(ClaimRequest {
                    node_id,
                    instance_id: format!("worker-{index}"),
                    lease_token: format!("lease-{index}"),
                    now_ms: 0,
                    lease_ms: 30_000,
                })
                .expect("claim")
            })
        })
        .collect::<Vec<_>>();
    let winners = handles
        .into_iter()
        .filter_map(|handle| handle.join().expect("claim thread"))
        .collect::<Vec<_>>();
    assert_eq!(winners.len(), 1);
    let leased = &winners[0];
    assert_eq!(leased.status, JobStatus::Leased);
    let lease_token = leased.lease_token.clone().expect("lease token");
    jobs.heartbeat(HeartbeatRequest {
        job_id: job_id.clone(),
        lease_token: lease_token.clone(),
        now_ms: 10,
        lease_ms: 30_000,
        events: vec![NewJobEvent {
            sequence: 1,
            event_type: "progress".to_string(),
            level: "info".to_string(),
            message: "pulled".to_string(),
            data: json!({"bytes": 10}),
        }],
    })
    .expect("heartbeat");
    let completed = jobs
        .complete(CompleteRequest {
            job_id: job_id.clone(),
            lease_token,
            status: CompletionStatus::Succeeded,
            result: json!({"container_id": "abc"}),
            error_message: String::new(),
            now_ms: 20,
            events: Vec::new(),
        })
        .expect("complete");
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(jobs.events(&job_id, 0).expect("events").len(), 1);

    verify_expired_request_contract(&store, suffix, &node_id);
    verify_expired_success_resolution_contract(&store, suffix, &node_id);
    verify_expired_lease_query_and_mutation_safety_contract(&store, suffix, &node_id);

    verify_node_enrollment_concurrency_contract(&store, suffix);
    verify_history_retention_contract(&store, suffix, &audit_request_id, &node_id);

    verify_topology_contract(&store, suffix);
    verify_idempotency_contract(&store, suffix);
    verify_operation_contract(&store, suffix);
    verify_control_plane_anomaly_contract(&store, suffix);
    verify_runtime_restart(&database_url, reconnect_options, &store, suffix);

    store.delete_node(&node_id).expect("delete node fixture");
    assert!(
        store
            .delete_state("postgres-contract", &state_key)
            .expect("delete UI state")
    );
}

fn verify_expired_request_contract(store: &PostgresOrchestratorStore, suffix: u64, node_id: &str) {
    let mut jobs = PostgresJobStore::new(store.clone());
    let baseline = PostgresOperationStore::new(store.clone())
        .anomaly_counters()
        .expect("read expired lease counter baseline")
        .expired_job_lease_transitions_total;
    let fixture = |name: &str| NewJob {
        job_id: format!("pg-expired-{name}-{suffix}"),
        operation_id: format!("pg-expired-operation-{name}-{suffix}"),
        node_id: node_id.to_string(),
        kind: JobKind::Health,
        payload: json!({}),
        idempotency_key: format!("pg-expired-key-{name}-{suffix}"),
        max_attempts: 3,
    };

    let heartbeat = fixture("heartbeat");
    jobs.enqueue(heartbeat.clone(), 0)
        .expect("enqueue expired heartbeat fixture");
    jobs.claim(ClaimRequest {
        node_id: node_id.to_string(),
        instance_id: "worker-expired-heartbeat".to_string(),
        lease_token: "current-heartbeat".to_string(),
        now_ms: 0,
        lease_ms: 30_000,
    })
    .expect("claim expired heartbeat fixture")
    .expect("expired heartbeat fixture");
    assert_eq!(
        jobs.heartbeat(HeartbeatRequest {
            job_id: heartbeat.job_id.clone(),
            lease_token: "old-token".to_string(),
            now_ms: 29_999,
            lease_ms: 30_000,
            events: Vec::new(),
        }),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert_eq!(
        jobs.get(&heartbeat.job_id)
            .expect("get live heartbeat fixture")
            .expect("heartbeat fixture")
            .status,
        JobStatus::Leased
    );
    assert_eq!(
        jobs.heartbeat(HeartbeatRequest {
            job_id: heartbeat.job_id.clone(),
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
        jobs.get(&heartbeat.job_id)
            .expect("get recovered heartbeat fixture")
            .expect("heartbeat fixture")
            .status,
        JobStatus::RetryWait
    );
    assert!(
        jobs.events(&heartbeat.job_id, 0)
            .expect("read rejected heartbeat events")
            .is_empty()
    );

    let completion = fixture("complete");
    jobs.enqueue(completion.clone(), 0)
        .expect("enqueue expired completion fixture");
    jobs.claim(ClaimRequest {
        node_id: node_id.to_string(),
        instance_id: "worker-expired-complete".to_string(),
        lease_token: "current-complete".to_string(),
        now_ms: 0,
        lease_ms: 30_000,
    })
    .expect("claim expired completion fixture")
    .expect("expired completion fixture");
    assert_eq!(
        jobs.complete(CompleteRequest {
            job_id: completion.job_id.clone(),
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
        jobs.get(&completion.job_id)
            .expect("get recovered completion fixture")
            .expect("completion fixture")
            .status,
        JobStatus::RetryWait
    );

    let race = fixture("race");
    jobs.enqueue(race.clone(), 0)
        .expect("enqueue expired race fixture");
    jobs.claim(ClaimRequest {
        node_id: node_id.to_string(),
        instance_id: "worker-expired-race".to_string(),
        lease_token: "race-token".to_string(),
        now_ms: 0,
        lease_ms: 30_000,
    })
    .expect("claim expired race fixture")
    .expect("expired race fixture");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let heartbeat_thread = {
        let barrier = barrier.clone();
        let mut jobs = PostgresJobStore::new(store.clone());
        let job_id = race.job_id.clone();
        std::thread::spawn(move || {
            barrier.wait();
            jobs.heartbeat(HeartbeatRequest {
                job_id,
                lease_token: "race-token".to_string(),
                now_ms: 30_000,
                lease_ms: 30_000,
                events: Vec::new(),
            })
        })
    };
    let recovery_thread = {
        let barrier = barrier.clone();
        let mut jobs = PostgresJobStore::new(store.clone());
        std::thread::spawn(move || {
            barrier.wait();
            jobs.recover_expired(30_000)
        })
    };
    assert_eq!(
        heartbeat_thread.join().expect("heartbeat thread"),
        Err(orchestrator_control_plane::JobError::StaleLease)
    );
    assert!(recovery_thread.join().expect("recovery thread").is_ok());
    assert_eq!(
        jobs.get(&race.job_id)
            .expect("get recovered race fixture")
            .expect("race fixture")
            .status,
        JobStatus::RetryWait
    );
    assert_eq!(
        PostgresOperationStore::new(store.clone())
            .anomaly_counters()
            .expect("read expired lease counter")
            .expired_job_lease_transitions_total,
        baseline + 3
    );
}

fn verify_expired_success_resolution_contract(
    store: &PostgresOrchestratorStore,
    suffix: u64,
    node_id: &str,
) {
    let mut jobs = PostgresJobStore::new(store.clone());
    let job_id = format!("pg-resolve-expired-{suffix}");
    jobs.enqueue(
        NewJob {
            job_id: job_id.clone(),
            operation_id: format!("pg-resolve-expired-operation-{suffix}"),
            node_id: node_id.to_string(),
            kind: JobKind::TopologyApply,
            payload: json!({"revision_id": format!("revision-{suffix}")}),
            idempotency_key: format!("pg-resolve-expired-key-{suffix}"),
            max_attempts: 2,
        },
        0,
    )
    .expect("enqueue expired success fixture");
    jobs.claim(ClaimRequest {
        node_id: node_id.to_string(),
        instance_id: "pg-resolve-worker-1".to_string(),
        lease_token: "pg-resolve-lease-1".to_string(),
        now_ms: 0,
        lease_ms: 30_000,
    })
    .expect("claim first expired success attempt")
    .expect("first expired success attempt");
    jobs.complete(CompleteRequest {
        job_id: job_id.clone(),
        lease_token: "pg-resolve-lease-1".to_string(),
        status: CompletionStatus::RetryableFailure,
        result: json!({}),
        error_message: "known retryable failure".to_string(),
        now_ms: 0,
        events: Vec::new(),
    })
    .expect("record known retryable failure");
    let leased = jobs
        .claim(ClaimRequest {
            node_id: node_id.to_string(),
            instance_id: "pg-resolve-worker-2".to_string(),
            lease_token: "pg-resolve-lease-2".to_string(),
            now_ms: 1_000,
            lease_ms: 30_000,
        })
        .expect("claim second expired success attempt")
        .expect("second expired success attempt");
    let result = json!({
        "topology_id": "primary",
        "revision_id": format!("revision-{suffix}"),
        "durable_evidence": {"applied_head": format!("revision-{suffix}")}
    });
    let request = ResolveExpiredSuccessRequest {
        job_id: job_id.clone(),
        now_ms: 31_000,
        result: result.clone(),
    };
    let resolved = jobs
        .resolve_expired_success(request.clone())
        .expect("resolve PostgreSQL expired success");
    assert_eq!(resolved.status, JobStatus::Succeeded);
    assert_eq!(resolved.result, Some(result));
    assert_eq!(resolved.error_message, None);
    assert_eq!(resolved.attempt, leased.attempt);
    assert_eq!(resolved.started_at_ms, leased.started_at_ms);
    assert_eq!(resolved.lease_owner, leased.lease_owner);
    assert_eq!(resolved.lease_token, leased.lease_token);
    assert_eq!(resolved.lease_expires_at_ms, leased.lease_expires_at_ms);
    assert_eq!(
        jobs.resolve_expired_success(request.clone())
            .expect("replay PostgreSQL expired success evidence"),
        resolved
    );
    assert!(matches!(
        jobs.resolve_expired_success(ResolveExpiredSuccessRequest {
            result: json!({"durable_evidence": "different"}),
            ..request
        }),
        Err(orchestrator_control_plane::JobError::InvalidTransition {
            from: JobStatus::Succeeded,
            ..
        })
    ));
    assert_eq!(
        jobs.get(&job_id)
            .expect("read PostgreSQL resolved success")
            .expect("resolved PostgreSQL Job"),
        resolved
    );
}

fn verify_expired_lease_query_and_mutation_safety_contract(
    store: &PostgresOrchestratorStore,
    suffix: u64,
    node_id: &str,
) {
    let mut jobs = PostgresJobStore::new(store.clone());
    for (id, lease_ms) in [
        (format!("pg-query-b-{suffix}"), 10_000),
        (format!("pg-query-a-{suffix}"), 10_000),
        (format!("pg-query-future-{suffix}"), 20_000),
    ] {
        jobs.enqueue(
            NewJob {
                job_id: id.clone(),
                operation_id: format!("operation-{id}"),
                node_id: node_id.to_string(),
                kind: JobKind::Health,
                payload: json!({}),
                idempotency_key: format!("key-{id}"),
                max_attempts: 2,
            },
            0,
        )
        .expect("enqueue PostgreSQL expired query fixture");
        jobs.claim(ClaimRequest {
            node_id: node_id.to_string(),
            instance_id: format!("worker-{id}"),
            lease_token: format!("lease-{id}"),
            now_ms: 0,
            lease_ms,
        })
        .expect("claim PostgreSQL expired query fixture")
        .expect("PostgreSQL expired query fixture");
    }
    let cancelling_id = format!("pg-query-b-{suffix}");
    jobs.request_cancel(&cancelling_id, 1)
        .expect("mark PostgreSQL expired query fixture cancelling");
    let expired = jobs
        .expired_leases(10_000)
        .expect("query PostgreSQL expired leases");
    assert_eq!(
        expired
            .iter()
            .map(|job| job.job_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            format!("pg-query-b-{suffix}"),
            format!("pg-query-a-{suffix}")
        ]
    );
    assert_eq!(
        jobs.get(&cancelling_id)
            .expect("read cancelling query fixture")
            .expect("cancelling query fixture")
            .status,
        JobStatus::CancelRequested
    );

    let mutating_id = format!("pg-mutating-expired-{suffix}");
    jobs.enqueue(
        NewJob {
            job_id: mutating_id.clone(),
            operation_id: format!("operation-{mutating_id}"),
            node_id: node_id.to_string(),
            kind: JobKind::TopologyApply,
            payload: json!({}),
            idempotency_key: format!("key-{mutating_id}"),
            max_attempts: 3,
        },
        0,
    )
    .expect("enqueue PostgreSQL mutating expiry fixture");
    jobs.claim(ClaimRequest {
        node_id: node_id.to_string(),
        instance_id: "pg-mutating-worker".to_string(),
        lease_token: "pg-mutating-lease".to_string(),
        now_ms: 0,
        lease_ms: 30_000,
    })
    .expect("claim PostgreSQL mutating expiry fixture")
    .expect("PostgreSQL mutating expiry fixture");
    let recovered = jobs
        .recover_expired(30_000)
        .expect("recover PostgreSQL expired leases");
    let mutating = recovered
        .iter()
        .find(|job| job.job_id == mutating_id)
        .expect("mutating Job is recovered");
    assert_eq!(mutating.status, JobStatus::NeedsAttention);
    assert_eq!(mutating.attempt, 1);
}

fn verify_node_enrollment_concurrency_contract(store: &PostgresOrchestratorStore, suffix: u64) {
    let node_id = format!("pg-enrollment-node-{suffix}");
    let digest = format!("sha256:{suffix:064x}");
    store
        .register_node_enrollment(
            &NodeRecord {
                node_id: node_id.clone(),
                host_ip: format!(
                    "10.201.{}.{}",
                    (suffix % 250) + 1,
                    ((suffix / 250) % 250) + 1
                ),
                parent_node_id: String::new(),
                role: "standalone".to_string(),
                labels: json!({"test": "node-enrollment-concurrency"}),
                status: "ENROLLMENT_PENDING".to_string(),
                created_at: "unix-ms:1".to_string(),
                updated_at: "unix-ms:1".to_string(),
            },
            &NodeEnrollmentCode {
                code_id: format!("pg-enrollment-code-{suffix}"),
                secret_sha256: digest.clone(),
                node_id: node_id.clone(),
                created_at_ms: 1,
                expires_at_ms: 10_000,
                redeemed_at_ms: None,
            },
        )
        .expect("register PostgreSQL enrollment fixture");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let csr_sha256 = format!("sha256:{}", "a".repeat(64));
    let handles = (0..8)
        .map(|index| {
            let store = store.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            let digest = digest.clone();
            let node_id = node_id.clone();
            let csr_sha256 = csr_sha256.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store
                    .redeem_node_enrollment_code(
                        &digest,
                        &csr_sha256,
                        2,
                        NewNodeCertificate {
                            serial_hex: format!("{suffix:016x}{index:02x}"),
                            node_id: node_id.clone(),
                            spiffe_id: format!("spiffe://ojos.local/node/{node_id}"),
                            certificate_pem: format!("certificate-{suffix}-{index}"),
                            fingerprint_sha256: format!("sha256:{suffix:064x}-{index}"),
                            issued_at_ms: 2,
                            not_before_ms: 2,
                            not_after_ms: 2 + CERTIFICATE_LIFETIME_MS,
                        },
                    )
                    .expect("redeem PostgreSQL enrollment code")
            })
        })
        .collect::<Vec<_>>();
    let outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("join PostgreSQL enrollment worker"))
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, EnrollmentRedemption::Redeemed(_)))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, EnrollmentRedemption::Replayed(_)))
            .count(),
        7
    );
    let returned_serials = outcomes
        .iter()
        .filter_map(|outcome| match outcome {
            EnrollmentRedemption::Redeemed(certificate)
            | EnrollmentRedemption::Replayed(certificate) => Some(certificate.serial_hex.as_str()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(returned_serials.len(), 1);
    assert!(matches!(
        store
            .redeem_node_enrollment_code(
                &digest,
                &format!("sha256:{}", "b".repeat(64)),
                3,
                NewNodeCertificate {
                    serial_hex: format!("{suffix:016x}ff"),
                    node_id: node_id.clone(),
                    spiffe_id: format!("spiffe://ojos.local/node/{node_id}"),
                    certificate_pem: format!("certificate-{suffix}-different-csr"),
                    fingerprint_sha256: format!("sha256:{suffix:064x}-different-csr"),
                    issued_at_ms: 3,
                    not_before_ms: 3,
                    not_after_ms: 3 + CERTIFICATE_LIFETIME_MS,
                },
            )
            .expect("reject different CSR replay"),
        EnrollmentRedemption::AlreadyRedeemed
    ));
    assert_eq!(
        store
            .get_node(&node_id)
            .expect("read redeemed PostgreSQL Node")
            .expect("redeemed PostgreSQL Node exists")
            .status,
        "READY"
    );

    store
        .pool()
        .with_transaction(|transaction| {
            transaction.execute(
                "DELETE FROM orchestrator_node_enrollment_codes WHERE node_id = $1",
                &[&node_id],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_node_certificates WHERE node_id = $1",
                &[&node_id],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_records WHERE kind = 'nodes' AND record_key = $1",
                &[&node_id],
            )?;
            Ok(())
        })
        .expect("remove PostgreSQL enrollment fixture");
}

fn verify_history_retention_contract(
    store: &PostgresOrchestratorStore,
    suffix: u64,
    audit_request_id: &str,
    node_id: &str,
) {
    let terminal_operation_id = format!("pg-retention-terminal-operation-{suffix}");
    let live_operation_id = format!("pg-retention-live-operation-{suffix}");
    let terminal_job_id = format!("pg-retention-terminal-job-{suffix}");
    let live_job_id = format!("pg-retention-live-job-{suffix}");
    store
        .pool()
        .with_transaction(|transaction| {
            for (operation_id, status) in [
                (&terminal_operation_id, "SUCCEEDED"),
                (&live_operation_id, "RUNNING"),
            ] {
                transaction.execute(
                    "INSERT INTO orchestrator_durable_operations(operation_id, revision, status, payload, created_at_ms, updated_at_ms)
                     VALUES ($1, 1, $2, '{}'::jsonb, -2000000, -2000000)",
                    &[operation_id, &status],
                )?;
                transaction.execute(
                    "INSERT INTO orchestrator_operation_logs_v2(operation_id, payload) VALUES ($1, '{}'::jsonb)",
                    &[operation_id],
                )?;
            }
            for (job_id, operation_id, status, completed_at) in [
                (
                    &terminal_job_id,
                    &terminal_operation_id,
                    "SUCCEEDED",
                    Some(-2_000_000_i64),
                ),
                (&live_job_id, &live_operation_id, "LEASED", None),
            ] {
                let payload = json!({
                    "job_id": job_id,
                    "completed_at_ms": completed_at,
                });
                transaction.execute(
                    "INSERT INTO orchestrator_jobs(job_id, operation_id, node_id, idempotency_key, payload_sha256, status, available_at_ms, created_at_ms, payload)
                     VALUES ($1, $2, $3, $4, $5, $6, -2000000, -2000000, $7::text::jsonb)",
                    &[
                        job_id,
                        operation_id,
                        &node_id,
                        &format!("pg-retention-{job_id}"),
                        &format!("sha256:{}", "3".repeat(64)),
                        &status,
                        &payload.to_string(),
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO orchestrator_job_events(job_id, sequence, payload, created_at_ms) VALUES ($1, 1, '{}'::jsonb, -2000000)",
                    &[job_id],
                )?;
            }
            Ok(())
        })
        .expect("seed PostgreSQL retention fixtures");

    let report = store
        .purge_terminal_history(-1_000_000, 0)
        .expect("purge PostgreSQL terminal history");
    assert_eq!(report.operation_logs_deleted, 1);
    assert_eq!(report.job_events_deleted, 1);

    store
        .pool()
        .with_client(|client| {
            let operation_resources: i64 = client
                .query_one(
                    "SELECT COUNT(*) FROM orchestrator_durable_operations WHERE operation_id IN ($1, $2)",
                    &[&terminal_operation_id, &live_operation_id],
                )?
                .get(0);
            let job_resources: i64 = client
                .query_one(
                    "SELECT COUNT(*) FROM orchestrator_jobs WHERE job_id IN ($1, $2)",
                    &[&terminal_job_id, &live_job_id],
                )?
                .get(0);
            let remaining_operation_logs: Vec<String> = client
                .query(
                    "SELECT operation_id FROM orchestrator_operation_logs_v2 WHERE operation_id IN ($1, $2) ORDER BY operation_id",
                    &[&terminal_operation_id, &live_operation_id],
                )?
                .into_iter()
                .map(|row| row.get(0))
                .collect();
            let remaining_job_events: Vec<String> = client
                .query(
                    "SELECT job_id FROM orchestrator_job_events WHERE job_id IN ($1, $2) ORDER BY job_id",
                    &[&terminal_job_id, &live_job_id],
                )?
                .into_iter()
                .map(|row| row.get(0))
                .collect();
            assert_eq!(operation_resources, 2, "retention must keep Operation resources");
            assert_eq!(job_resources, 2, "retention must keep Job resources");
            assert_eq!(remaining_operation_logs, vec![live_operation_id.clone()]);
            assert_eq!(remaining_job_events, vec![live_job_id.clone()]);
            Ok(())
        })
        .expect("verify PostgreSQL retention result");
    assert_eq!(
        store
            .audit_records(Some(audit_request_id), 0, 10)
            .expect("audit remains queryable after retention")
            .len(),
        2,
        "retention must never prune append-only audit",
    );
    assert!(
        store
            .get_node(node_id)
            .expect("resource remains queryable after retention")
            .is_some()
    );

    store
        .pool()
        .with_transaction(|transaction| {
            transaction.execute(
                "DELETE FROM orchestrator_job_events WHERE job_id IN ($1, $2)",
                &[&terminal_job_id, &live_job_id],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_jobs WHERE job_id IN ($1, $2)",
                &[&terminal_job_id, &live_job_id],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_operation_logs_v2 WHERE operation_id IN ($1, $2)",
                &[&terminal_operation_id, &live_operation_id],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_durable_operations WHERE operation_id IN ($1, $2)",
                &[&terminal_operation_id, &live_operation_id],
            )?;
            Ok(())
        })
        .expect("clean PostgreSQL retention fixtures");
}

#[test]
fn postgres_legacy_upgrade_imports_draft_and_external_unknown_when_configured() {
    let Some(database_url) = std::env::var("OJOS_TEST_POSTGRES_URL").ok() else {
        return;
    };
    let mut options = PostgresOptions::default();
    if let Ok(path) = std::env::var("OJOS_TEST_POSTGRES_CA") {
        options.tls_trust = PostgresTlsTrust::CaCertificate(PathBuf::from(path));
    }
    let store = PostgresOrchestratorStore::connect(&database_url, options.clone())
        .expect("connect dedicated PostgreSQL test database");
    let root_endpoint = "127.0.0.1:18080:legacy-gateway";
    let worker_endpoint = "127.0.0.2:18081:legacy-worker";
    let snapshot_id = "pg-legacy-snapshot-v0-2";
    let snapshot = json!({
        "snapshot_id": snapshot_id,
        "topology": {
            "root_host": "127.0.0.1",
            "root_endpoint": root_endpoint,
            "authority": {
                "root_host": "127.0.0.1",
                "root_endpoint": root_endpoint,
                "exposure_policy": "private",
                "notes": []
            },
            "services": ["legacy-gateway", "legacy-worker"],
            "endpoints": [
                {
                    "endpoint": root_endpoint,
                    "service_id": "legacy-gateway",
                    "protocol": "http",
                    "health_path": "/health",
                    "health": "healthy",
                    "reachable": true
                },
                {
                    "endpoint": worker_endpoint,
                    "service_id": "legacy-worker",
                    "protocol": "http",
                    "health_path": "/health",
                    "health": "unreachable",
                    "reachable": false
                }
            ],
            "links": [],
            "operations": [],
            "log_views": [],
            "diagnostic_reports": []
        },
        "created_at": "2026-08-03T00:00:00Z"
    });
    let endpoint = snapshot["topology"]["endpoints"][1].clone();
    store
        .pool()
        .with_client(|client| {
            let mut transaction = client.transaction()?;
            transaction.batch_execute(
                "CREATE TABLE IF NOT EXISTS topology_snapshots (
                    snapshot_id TEXT PRIMARY KEY,
                    topology JSONB NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                 );
                 CREATE TABLE IF NOT EXISTS host_services (
                    host_ip TEXT NOT NULL,
                    service_name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    status TEXT NOT NULL,
                    config JSONB NOT NULL DEFAULT '{}'::jsonb,
                    labels JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    PRIMARY KEY(host_ip, service_name)
                 );
                 CREATE TABLE IF NOT EXISTS service_endpoints (
                    endpoint TEXT PRIMARY KEY,
                    service_id TEXT NOT NULL,
                    protocol TEXT NOT NULL,
                    health_path TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'unknown',
                    reachable BOOLEAN NOT NULL DEFAULT FALSE,
                    display_name TEXT NOT NULL DEFAULT '',
                    note TEXT NOT NULL DEFAULT '',
                    config JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
                 );",
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_status WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_heads WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_revisions WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE service_id = 'legacy-worker'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM topology_snapshots WHERE snapshot_id = $1",
                &[&snapshot_id],
            )?;
            transaction.execute(
                "DELETE FROM host_services WHERE host_ip = '127.0.0.2' AND service_name = 'legacy-worker'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM service_endpoints WHERE endpoint IN ($1, $2)",
                &[&root_endpoint, &worker_endpoint],
            )?;
            transaction.execute(
                "INSERT INTO topology_snapshots(snapshot_id, topology, created_at) VALUES ($1, $2::text::jsonb, '2026-08-03T00:00:00Z')",
                &[&snapshot_id, &snapshot["topology"].to_string()],
            )?;
            transaction.execute(
                "INSERT INTO host_services(host_ip, service_name, version, status, config, labels, created_at, updated_at) VALUES ('127.0.0.2', 'legacy-worker', '0.2.0', 'running', '{}'::jsonb, '{}'::jsonb, '2026-08-02T00:00:00Z', '2026-08-03T00:00:00Z')",
                &[],
            )?;
            for value in [&snapshot["topology"]["endpoints"][0], &endpoint] {
                transaction.execute(
                    "INSERT INTO service_endpoints(endpoint, service_id, protocol, health_path, status, reachable, display_name, note, config, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, '', '', '{}'::jsonb, '2026-08-02T00:00:00Z', '2026-08-03T00:00:00Z')",
                    &[
                        &value["endpoint"].as_str().unwrap(),
                        &value["service_id"].as_str().unwrap(),
                        &value["protocol"].as_str().unwrap(),
                        &value["health_path"].as_str().unwrap(),
                        &value["health"].as_str().unwrap(),
                        &value["reachable"].as_bool().unwrap(),
                    ],
                )?;
            }
            transaction.execute(
                "DELETE FROM orchestrator_legacy_imports WHERE import_id = 'v0.2-records-to-v1'",
                &[],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .expect("seed legacy PostgreSQL records");

    let upgraded = PostgresOrchestratorStore::connect(&database_url, options)
        .expect("reopen and import legacy PostgreSQL records");
    let report = upgraded
        .legacy_import_report()
        .expect("legacy import report");
    assert_eq!(report.topology_snapshot_id.as_deref(), Some(snapshot_id));
    assert_eq!(report.runtime_instances_imported, 1);
    let heads = upgraded
        .topology_heads("primary")
        .expect("topology heads")
        .expect("imported primary topology");
    assert!(heads.applied_revision_id.is_none());
    assert!(heads.applying_revision_id.is_none());
    let runtime = upgraded
        .runtime_instances(None)
        .expect("runtime projections")
        .into_iter()
        .find(|instance| instance.instance.service_id == "legacy-worker")
        .expect("imported legacy runtime");
    assert_eq!(
        runtime.management_mode,
        orchestrator_storage::RuntimeManagementMode::External
    );
    assert_eq!(
        runtime.instance.observed_state,
        RuntimeObservedState::Unknown
    );
    assert!(runtime.instance.artifact_digest.is_empty());

    upgraded
        .pool()
        .with_client(|client| {
            let mut transaction = client.transaction()?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_status WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_heads WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_revisions WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE service_id = 'legacy-worker'",
                &[],
            )?;
            transaction.execute("DELETE FROM topology_snapshots WHERE snapshot_id = $1", &[&snapshot_id])?;
            transaction.execute("DELETE FROM host_services WHERE host_ip = '127.0.0.2' AND service_name = 'legacy-worker'", &[])?;
            transaction.execute("DELETE FROM service_endpoints WHERE endpoint IN ($1, $2)", &[&root_endpoint, &worker_endpoint])?;
            transaction.execute(
                "DELETE FROM orchestrator_legacy_imports WHERE import_id = 'v0.2-records-to-v1'",
                &[],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .expect("clean legacy PostgreSQL fixture");
}

/// Release-only upgrade drill. A separately built v0.2 repository fixture
/// writes the normalized legacy tables first; this test then opens that same
/// database through the v1 TLS store and verifies the real one-time import.
#[test]
fn postgres_upgrade_from_v0_2_writer_when_configured() {
    let Some(database_url) = std::env::var("OJOS_TEST_POSTGRES_UPGRADE_URL").ok() else {
        return;
    };
    let mut options = PostgresOptions::default();
    if let Ok(path) = std::env::var("OJOS_TEST_POSTGRES_CA") {
        options.tls_trust = PostgresTlsTrust::CaCertificate(PathBuf::from(path));
    }
    let reopened_options = options.clone();
    let upgraded = PostgresOrchestratorStore::connect(&database_url, options)
        .expect("open the v0.2 database through the v1 TLS store");
    let report = upgraded
        .legacy_import_report()
        .expect("read the v0.2-to-v1 import report");
    assert_eq!(
        report.topology_snapshot_id.as_deref(),
        Some("release-v0-2-upgrade-snapshot")
    );
    assert_eq!(report.runtime_instances_imported, 1);
    assert!(report.runtime_instances_skipped.is_empty());

    let heads = upgraded
        .topology_heads("primary")
        .expect("read imported topology heads")
        .expect("v0.2 snapshot became the primary topology");
    assert!(heads.applied_revision_id.is_none());
    assert!(heads.applying_revision_id.is_none());
    let revisions = upgraded
        .topology_revisions("primary")
        .expect("read imported topology revisions");
    assert_eq!(revisions.len(), 1);
    let encoded_spec = serde_json::to_string(revisions[0].spec()).unwrap();
    assert!(!encoded_spec.contains("healthy"));
    assert!(!encoded_spec.contains("unreachable"));

    let runtime = upgraded
        .runtime_instances(None)
        .expect("read imported runtime projection")
        .into_iter()
        .find(|instance| instance.instance.service_id == "release-v0-2-upgrade-worker")
        .expect("v0.2 running HostService became a runtime projection");
    assert_eq!(
        runtime.management_mode,
        orchestrator_storage::RuntimeManagementMode::External
    );
    assert_eq!(
        runtime.instance.observed_state,
        RuntimeObservedState::Unknown
    );
    assert!(runtime.instance.artifact_digest.is_empty());

    let reopened = PostgresOrchestratorStore::connect(&database_url, reopened_options)
        .expect("reopen the upgraded database");
    assert_eq!(
        reopened
            .topology_revisions("primary")
            .expect("read revisions after restart")
            .len(),
        1,
        "the one-time importer must not create a second revision"
    );
    assert_eq!(
        reopened
            .runtime_instances(None)
            .expect("read runtimes after restart")
            .into_iter()
            .filter(|instance| instance.instance.service_id == "release-v0-2-upgrade-worker")
            .count(),
        1,
        "the one-time importer must not create a second runtime projection"
    );

    reopened
        .pool()
        .with_transaction(|transaction| {
            transaction.execute(
                "DELETE FROM orchestrator_topology_status WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_heads WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_topology_revisions WHERE topology_id = 'primary'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_runtime_instances WHERE service_id = 'release-v0-2-upgrade-worker'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM topology_snapshots WHERE snapshot_id = 'release-v0-2-upgrade-snapshot'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM host_services WHERE host_ip = '127.0.0.2' AND service_name = 'release-v0-2-upgrade-worker'",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM service_endpoints WHERE service_id IN ('release-v0-2-upgrade-gateway', 'release-v0-2-upgrade-worker')",
                &[],
            )?;
            transaction.execute(
                "DELETE FROM orchestrator_legacy_imports WHERE import_id = 'v0.2-records-to-v1'",
                &[],
            )?;
            Ok(())
        })
        .expect("clean real v0.2 upgrade fixture");
}

fn verify_operation_contract(store: &PostgresOrchestratorStore, suffix: u64) {
    let operation_id = format!("pg-operation-coordinator-{suffix}");
    let mut operations = PostgresOperationStore::new(store.clone());
    let mut jobs = MemoryJobStore::default();
    let running = {
        let mut coordinator = OperationCoordinator::new(&mut operations, &mut jobs);
        coordinator
            .plan(
                PlanOperation {
                    operation_id: operation_id.clone(),
                    action: "release.install".to_string(),
                    target_type: "Release".to_string(),
                    target_id: "judge".to_string(),
                    request: json!({}),
                    jobs: vec![PlannedJob {
                        step_id: "install".to_string(),
                        node_id: format!("pg-node-{suffix}"),
                        kind: JobKind::Install,
                        depends_on: vec![],
                        condition: Default::default(),
                        payload: json!({"spec": {}}),
                        max_attempts: 3,
                    }],
                },
                1,
            )
            .expect("plan durable operation");
        coordinator
            .confirm(&operation_id, 2)
            .expect("confirm durable operation");
        coordinator
            .enqueue(&operation_id, 3)
            .expect("enqueue durable operation")
    };
    assert_eq!(running.status, DurableOperationStatus::Running);
    assert!(matches!(
        operations.compare_and_swap(1, running.clone()),
        Err(OperationStoreError::RevisionConflict { .. })
    ));
    let recovered = PostgresOperationStore::new(store.clone())
        .recoverable()
        .expect("recover durable operations");
    assert!(
        recovered
            .iter()
            .any(|operation| operation.operation_id == operation_id)
    );
}

fn anomaly_operation(
    operation_id: String,
    status: DurableOperationStatus,
    updated_at_ms: i64,
    started_at_ms: Option<i64>,
) -> DurableOperation {
    DurableOperation {
        schema_version: OPERATION_SCHEMA_VERSION,
        operation_id,
        mode: DurableOperationMode::Apply,
        rollback_of_operation_id: None,
        action: "deployment.start".to_string(),
        target_type: "Deployment".to_string(),
        target_id: "deployment-1".to_string(),
        status,
        request: json!({}),
        plan_sha256: "0".repeat(64),
        planned_jobs: vec![],
        job_bindings: vec![],
        pending_step_ids: vec![],
        attention_job_ids: vec![],
        generation: 1,
        revision: 1,
        result: json!({}),
        error_message: String::new(),
        created_at_ms: 1,
        updated_at_ms,
        confirmed_at_ms: Some(1),
        started_at_ms,
        finished_at_ms: None,
    }
}

fn terminal_operation(operation: &DurableOperation, finished_at_ms: i64) -> DurableOperation {
    let mut terminal = operation.clone();
    terminal.status = DurableOperationStatus::Succeeded;
    terminal.revision += 1;
    terminal.updated_at_ms = finished_at_ms;
    terminal.finished_at_ms = Some(finished_at_ms);
    terminal
}

fn verify_control_plane_anomaly_contract(store: &PostgresOrchestratorStore, suffix: u64) {
    let mut operations = PostgresOperationStore::new(store.clone());
    let baseline = operations
        .anomaly_counters()
        .expect("read PostgreSQL anomaly counter baseline")
        .operation_over_300_seconds_transitions_total;
    let identity_id = format!("pg-anomaly-identity-{suffix}");
    let enqueuing = anomaly_operation(
        identity_id.clone(),
        DurableOperationStatus::Enqueuing,
        1,
        None,
    );
    operations
        .create(enqueuing.clone())
        .expect("create PostgreSQL identity-change anomaly fixture");
    assert_eq!(
        operations
            .observe_active_operation_anomalies(std::slice::from_ref(&enqueuing), 300_002,)
            .expect("observe PostgreSQL old Operation episode")
            .operation_over_300_seconds_transitions_total,
        baseline + 1
    );
    let mut running = enqueuing.clone();
    running.status = DurableOperationStatus::Running;
    running.revision = 2;
    running.started_at_ms = Some(300_002);
    running.updated_at_ms = 300_002;
    operations
        .compare_and_swap(1, running.clone())
        .expect("change PostgreSQL Operation episode identity");
    let marker_count: i64 = store
        .pool()
        .with_client(|client| {
            Ok(client
                .query_one(
                    "SELECT COUNT(*)::BIGINT FROM orchestrator_active_operation_anomalies WHERE operation_id = $1",
                    &[&identity_id],
                )?
                .get(0))
        })
        .expect("read PostgreSQL anomaly markers");
    assert_eq!(marker_count, 0);
    operations
        .observe_active_operation_anomalies(std::slice::from_ref(&running), 600_003)
        .expect("observe PostgreSQL new Operation episode");
    operations
        .compare_and_swap(2, terminal_operation(&running, 600_004))
        .expect("finish PostgreSQL new Operation episode");

    let exact = anomaly_operation(
        format!("pg-anomaly-exact-{suffix}"),
        DurableOperationStatus::Running,
        1,
        Some(1),
    );
    operations.create(exact.clone()).unwrap();
    let mut exact_next = exact.clone();
    exact_next.revision = 2;
    exact_next.generation = 2;
    exact_next.updated_at_ms = 300_001;
    exact_next.started_at_ms = Some(300_001);
    operations
        .compare_and_swap(1, exact_next)
        .expect("change identity at exact PostgreSQL anomaly threshold");
    assert_eq!(
        operations
            .anomaly_counters()
            .expect("read exact-threshold PostgreSQL anomaly counter")
            .operation_over_300_seconds_transitions_total,
        baseline + 2
    );

    let over = anomaly_operation(
        format!("pg-anomaly-over-{suffix}"),
        DurableOperationStatus::Running,
        1,
        Some(1),
    );
    operations.create(over.clone()).unwrap();
    let mut over_next = over.clone();
    over_next.revision = 2;
    over_next.generation = 2;
    over_next.updated_at_ms = 300_002;
    over_next.started_at_ms = Some(300_002);
    operations
        .compare_and_swap(1, over_next)
        .expect("change unobserved PostgreSQL anomaly identity over threshold");

    let observed = anomaly_operation(
        format!("pg-anomaly-observed-{suffix}"),
        DurableOperationStatus::Running,
        1,
        Some(1),
    );
    operations.create(observed.clone()).unwrap();
    operations
        .observe_active_operation_anomalies(std::slice::from_ref(&observed), 300_002)
        .expect("observe PostgreSQL old identity before transition");
    let mut observed_next = observed.clone();
    observed_next.revision = 2;
    observed_next.generation = 2;
    observed_next.updated_at_ms = 300_003;
    observed_next.started_at_ms = Some(300_003);
    operations
        .compare_and_swap(1, observed_next)
        .expect("change observed PostgreSQL anomaly identity");
    assert_eq!(
        operations
            .anomaly_counters()
            .expect("read identity-change PostgreSQL anomaly counter")
            .operation_over_300_seconds_transitions_total,
        baseline + 4
    );

    let later = anomaly_operation(
        format!("pg-anomaly-later-{suffix}"),
        DurableOperationStatus::Running,
        1,
        Some(1),
    );
    let earlier = anomaly_operation(
        format!("pg-anomaly-earlier-{suffix}"),
        DurableOperationStatus::Running,
        1,
        Some(1),
    );
    operations.create(later.clone()).unwrap();
    operations.create(earlier.clone()).unwrap();
    operations
        .compare_and_swap(1, terminal_operation(&later, 500_000))
        .expect("commit later-finished PostgreSQL Operation first");
    operations
        .compare_and_swap(1, terminal_operation(&earlier, 400_000))
        .expect("commit earlier-finished PostgreSQL Operation second");
    assert_eq!(
        operations
            .anomaly_counters()
            .expect("read final PostgreSQL anomaly counters")
            .operation_over_300_seconds_transitions_total,
        baseline + 6
    );
    store
        .pool()
        .with_client(|client| {
            client.execute(
                "DELETE FROM orchestrator_durable_operations WHERE operation_id = ANY($1)",
                &[&vec![
                    identity_id,
                    exact.operation_id,
                    over.operation_id,
                    observed.operation_id,
                    later.operation_id,
                    earlier.operation_id,
                ]],
            )?;
            Ok(())
        })
        .expect("remove PostgreSQL anomaly fixtures");
}

fn verify_topology_contract(store: &PostgresOrchestratorStore, suffix: u64) {
    let topology_id = format!("pg-topology-{suffix}");
    let first = store
        .create_initial_topology_revision(
            topology_spec(&topology_id, "first"),
            "t1",
            "admin",
            "initial",
        )
        .expect("initial revision");
    store
        .begin_topology_apply(&topology_id, first.revision_id(), "op-failed", "t2")
        .expect("begin failed apply");
    let failed = store
        .finish_topology_apply(
            &topology_id,
            first.revision_id(),
            "op-failed",
            TopologyApplyOutcome::Failed,
            "t3",
        )
        .expect("finish failed apply");
    assert_eq!(failed.applied_revision_id, None);
    store
        .begin_topology_apply(&topology_id, first.revision_id(), "op-success", "t4")
        .expect("begin successful apply");
    let succeeded = store
        .finish_topology_apply(
            &topology_id,
            first.revision_id(),
            "op-success",
            TopologyApplyOutcome::Succeeded,
            "t5",
        )
        .expect("finish successful apply");
    assert_eq!(
        succeeded.applied_revision_id.as_deref(),
        Some(first.revision_id())
    );
    let second = store
        .create_next_topology_revision(
            &topology_id,
            first.revision_id(),
            topology_spec(&topology_id, "second"),
            "t6",
            "admin",
            "edit",
        )
        .expect("next revision");
    assert!(matches!(
        store.create_next_topology_revision(
            &topology_id,
            first.revision_id(),
            topology_spec(&topology_id, "stale"),
            "t7",
            "admin",
            "stale",
        ),
        Err(PostgresError::Conflict(_))
    ));
    let rollback = store
        .create_topology_rollback_revision(
            &topology_id,
            second.revision_id(),
            first.revision_id(),
            "t8",
            "admin",
            "rollback",
        )
        .expect("rollback revision");
    assert_eq!(rollback.revision_number(), 3);
    assert_eq!(
        rollback.rollback_of_revision_id(),
        Some(first.revision_id())
    );
    assert_eq!(rollback.spec(), first.spec());
    assert_eq!(store.topology_revisions(&topology_id).unwrap().len(), 3);

    let group_topology_id = format!("pg-topology-group-{suffix}");
    let group_revision = store
        .create_initial_topology_revision(
            topology_spec(&group_topology_id, "group terminal bindings"),
            "t9",
            "admin",
            "group initial",
        )
        .expect("initial grouped revision");
    let operation_id = format!("pg-group-operation-{suffix}");
    store
        .begin_topology_apply(
            &group_topology_id,
            group_revision.revision_id(),
            &operation_id,
            "t10",
        )
        .expect("begin grouped apply");
    let retained = finalized_api_binding(
        &format!("pg-binding-active-{suffix}"),
        "permission_check",
        &format!("pg-consumer-{suffix}"),
        &group_topology_id,
        group_revision.revision_id(),
        &operation_id,
    );
    let mut revoked = finalized_api_binding(
        &format!("pg-binding-revoked-{suffix}"),
        "echo",
        &format!("pg-consumer-{suffix}"),
        &group_topology_id,
        group_revision.revision_id(),
        &operation_id,
    );
    revoked.desired_state = "REVOKED".to_string();
    revoked.observed_state = "REVOKED".to_string();
    revoked.health = "UNKNOWN".to_string();
    revoked.state = ApiBindingState::Revoked;
    revoked.optional = true;
    let members = vec![TopologyApplyGroupMember {
        topology_id: group_topology_id.clone(),
        revision_id: group_revision.revision_id().to_string(),
        active_bindings: vec![retained, revoked],
    }];

    let mut unstaged = members.clone();
    unstaged[0].active_bindings[0].last_operation_id = "older-operation".to_string();
    assert!(matches!(
        store.finish_topology_apply_group(&unstaged, &operation_id, "t11"),
        Err(PostgresError::Invariant(_))
    ));
    let mut duplicate = members.clone();
    duplicate[0].active_bindings[1].binding_id = duplicate[0].active_bindings[0].binding_id.clone();
    assert!(matches!(
        store.finish_topology_apply_group(&duplicate, &operation_id, "t11"),
        Err(PostgresError::Invariant(_))
    ));
    store
        .finish_topology_apply_group(&members, &operation_id, "t12")
        .expect("finish grouped apply with retained and revoked terminal bindings");
    assert_eq!(
        store
            .api_bindings_for_topology(&group_topology_id)
            .expect("read grouped PostgreSQL bindings"),
        members[0].active_bindings
    );

    // Agent completion replaces the consumer view while Topology finalization
    // replaces the topology view. They deliberately overlap on the same rows;
    // all writers must serialize instead of interleaving DELETE/INSERT pairs.
    let race_topology_id = format!("pg-binding-race-topology-{suffix}");
    let race_deployment_id = format!("pg-binding-race-deployment-{suffix}");
    let race_binding = finalized_api_binding(
        &format!("pg-binding-race-{suffix}"),
        "judge_control",
        &race_deployment_id,
        &race_topology_id,
        &format!("pg-binding-race-revision-{suffix}"),
        &format!("pg-binding-race-operation-{suffix}"),
    );
    store
        .replace_topology_api_bindings(&race_topology_id, std::slice::from_ref(&race_binding))
        .expect("seed overlapping binding projection");
    let writer_count = 24;
    let barrier = Arc::new(Barrier::new(writer_count));
    let handles = (0..writer_count)
        .map(|index| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            let binding = race_binding.clone();
            let deployment_id = race_deployment_id.clone();
            let topology_id = race_topology_id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                if index % 2 == 0 {
                    store.replace_deployment_api_bindings(
                        &deployment_id,
                        std::slice::from_ref(&binding),
                    )
                } else {
                    store
                        .replace_topology_api_bindings(&topology_id, std::slice::from_ref(&binding))
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle
            .join()
            .expect("binding projection writer thread")
            .expect("serialized overlapping binding replacement");
    }
    assert_eq!(
        store
            .api_bindings_for_topology(&race_topology_id)
            .expect("read serialized binding projection"),
        vec![race_binding]
    );
}

fn verify_idempotency_contract(store: &PostgresOrchestratorStore, suffix: u64) {
    let scope = format!("POST /api/v1/pg-contract/{suffix}");
    let key = format!("request-{suffix}");
    let digest = format!("sha256:{}", "a".repeat(64));
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &key, &digest, 1)
            .expect("reserve idempotency"),
        IdempotencyBegin::Started
    );
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &key, &digest, 2)
            .expect("request in progress"),
        IdempotencyBegin::InProgress
    );
    let response = StoredIdempotentResponse {
        status: 202,
        content_type: "application/json".to_string(),
        headers: BTreeMap::from([("X-Request-ID".to_string(), "req-1".to_string())]),
        body: json!({"operation_id": "op-1"}),
    };
    store
        .complete_idempotent_request(&scope, &key, &digest, &response, 3)
        .expect("complete idempotency");
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &key, &digest, 4)
            .expect("replay idempotency"),
        IdempotencyBegin::Replay(response)
    );
    let other_digest = format!("sha256:{}", "b".repeat(64));
    assert!(matches!(
        store.begin_idempotent_request(&scope, &key, &other_digest, 5),
        Err(PostgresError::Conflict(_))
    ));

    let stale_key = format!("stale-request-{suffix}");
    store
        .begin_idempotent_request(&scope, &stale_key, &digest, 1)
        .expect("reserve stale fixture");
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &stale_key, &digest, 300_001)
            .expect("stale request"),
        IdempotencyBegin::NeedsAttention
    );

    let aborted_key = format!("aborted-request-{suffix}");
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &aborted_key, &digest, 1)
            .expect("reserve pre-dispatch fixture"),
        IdempotencyBegin::Started
    );
    store
        .abort_idempotent_request(&scope, &aborted_key, &digest)
        .expect("release pre-dispatch reservation");
    assert_eq!(
        store
            .begin_idempotent_request(&scope, &aborted_key, &digest, 2)
            .expect("retry released reservation"),
        IdempotencyBegin::Started
    );
}

fn verify_runtime_restart(
    database_url: &str,
    options: PostgresOptions,
    store: &PostgresOrchestratorStore,
    suffix: u64,
) {
    let deployment_id = format!("pg-deployment-{suffix}");
    let value = StoredRuntimeInstance {
        node_id: format!("spiffe://ojos/node/pg-node-{suffix}"),
        instance: RuntimeInstance {
            deployment_id: deployment_id.clone(),
            service_id: "judge".to_string(),
            release_version: "1.0.0".to_string(),
            container_id: format!("container-{suffix}"),
            artifact_digest: format!("sha256:{}", "c".repeat(64)),
            runtime_contract: orchestrator_runtime::RuntimeContract::standard_v1(),
            runtime_policy_sha256: String::new(),
            effective_runtime_sha256: String::new(),
            runtime_attested: false,
            desired_state: RuntimeDesiredState::Running,
            observed_state: RuntimeObservedState::Running,
            health: "healthy".to_string(),
        },
        management_mode: orchestrator_storage::RuntimeManagementMode::Managed,
        endpoint: String::new(),
        external_probe_protocol: String::new(),
        external_probe_health_path: String::new(),
        last_observed_at_ms: 0,
        drift_reason: String::new(),
        credential_expires_at_ms: 0,
        credential_last_success_at_ms: 0,
        credential_last_error: String::new(),
        updated_at: "2026-08-03T00:00:00Z".to_string(),
    };
    store
        .put_runtime_instance(&value)
        .expect("persist runtime instance");
    let reopened =
        PostgresOrchestratorStore::connect(database_url, options).expect("reopen PostgreSQL store");
    assert_eq!(
        reopened
            .runtime_instance(&deployment_id)
            .expect("read runtime after restart"),
        Some(value.clone())
    );
    let mut external = value.clone();
    external.node_id = "external".to_string();
    external.instance.deployment_id = format!("pg-external-deployment-{suffix}");
    external.instance.container_id.clear();
    external.instance.runtime_attested = false;
    external.management_mode = orchestrator_storage::RuntimeManagementMode::External;
    external.endpoint = "https://external.example".to_string();
    external.external_probe_protocol = "https".to_string();
    external.external_probe_health_path = "/healthz/ready".to_string();
    external.last_observed_at_ms = 123_456;
    reopened
        .put_runtime_instance(&external)
        .expect("persist External probe contract");
    assert_eq!(
        reopened
            .runtime_instance(&external.instance.deployment_id)
            .expect("read External probe contract"),
        Some(external)
    );
    let report = StoredNodeRuntimeFacts {
        node_id: value.node_id.clone(),
        observed_at_ms: 100,
        received_at_ms: 101,
        facts: json!({"schema_version": 1, "report_id": format!("pg-report-{suffix}")}),
    };
    let mut projected = value.clone();
    projected.last_observed_at_ms = 100;
    projected.instance.runtime_attested = true;
    reopened
        .apply_node_runtime_report(
            &report,
            Some(std::slice::from_ref(&deployment_id)),
            &[(value.clone(), projected.clone())],
        )
        .expect("atomically apply PostgreSQL runtime report");
    reopened
        .apply_node_runtime_report(
            &report,
            Some(std::slice::from_ref(&deployment_id)),
            &[(value.clone(), projected.clone())],
        )
        .expect("replay identical PostgreSQL runtime report");

    let mut lifecycle = projected.clone();
    lifecycle.instance.desired_state = RuntimeDesiredState::Stopped;
    lifecycle.instance.observed_state = RuntimeObservedState::Stopped;
    lifecycle.instance.health = "NONE".to_string();
    lifecycle.updated_at = "2026-08-03T00:00:01Z".to_string();
    reopened
        .put_runtime_instance(&lifecycle)
        .expect("persist concurrent PostgreSQL lifecycle state");
    let newer_report = StoredNodeRuntimeFacts {
        node_id: value.node_id.clone(),
        observed_at_ms: 200,
        received_at_ms: 201,
        facts: json!({"schema_version": 1, "report_id": format!("pg-report-newer-{suffix}")}),
    };
    let mut stale_projection = projected.clone();
    stale_projection.last_observed_at_ms = 200;
    assert!(matches!(
        reopened.apply_node_runtime_report(
            &newer_report,
            Some(std::slice::from_ref(&deployment_id)),
            &[(projected.clone(), stale_projection)],
        ),
        Err(PostgresError::Conflict(_))
    ));
    assert_eq!(
        reopened
            .node_runtime_facts(&value.node_id)
            .expect("read PostgreSQL runtime facts after CAS conflict"),
        Some(report)
    );
    assert_eq!(
        reopened
            .runtime_instance(&deployment_id)
            .expect("read PostgreSQL lifecycle state after CAS conflict"),
        Some(lifecycle)
    );
    assert!(
        reopened
            .delete_runtime_instance(&deployment_id)
            .expect("delete runtime fixture")
    );
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
                protocol: "https".to_string(),
                health_path: "/healthz".to_string(),
                display_name: "Gateway".to_string(),
                note: note.to_string(),
                config: json!({}),
            },
            TopologyEndpointSpec {
                endpoint: "127.0.0.1:8081:worker".to_string(),
                service_id: "worker".to_string(),
                protocol: "https".to_string(),
                health_path: "/healthz".to_string(),
                display_name: "Worker".to_string(),
                note: String::new(),
                config: json!({}),
            },
        ],
        vec![TopologyLinkSpec {
            source_endpoint: "127.0.0.1:8080:gateway".to_string(),
            target_endpoint: "127.0.0.1:8081:worker".to_string(),
            protocol: "https".to_string(),
            auth_mode: "internal".to_string(),
            scope: "worker.invoke".to_string(),
            enabled: true,
            config_ref: String::new(),
            secret_ref: String::new(),
            policy: json!({}),
            api_bindings: Vec::new(),
        }],
    )
    .expect("valid topology spec")
}

fn finalized_api_binding(
    binding_id: &str,
    requirement_name: &str,
    consumer_deployment_id: &str,
    topology_id: &str,
    revision_id: &str,
    operation_id: &str,
) -> ApiBinding {
    ApiBinding {
        binding_id: binding_id.to_string(),
        requirement_name: requirement_name.to_string(),
        api_id: format!("fixture.{requirement_name}"),
        api_version: "1.0.0".to_string(),
        consumer_deployment_id: consumer_deployment_id.to_string(),
        consumer_service_id: "fixture-consumer".to_string(),
        consumer_node_id: "node-consumer".to_string(),
        consumer_endpoint: "10.0.0.2:9000:consumer".to_string(),
        provider_deployment_id: format!("provider-{binding_id}"),
        provider_service_id: "fixture-provider".to_string(),
        provider_node_id: "node-provider".to_string(),
        provider_endpoint: "10.0.0.1:8080:provider".to_string(),
        provider_path: format!("/{requirement_name}"),
        virtual_endpoint: format!("/internal/apis/fixture.{requirement_name}"),
        protocol: "http".to_string(),
        methods: vec!["GET".to_string()],
        auth_mode: "workload".to_string(),
        provider_auth_mode: "workload".to_string(),
        permission: format!("fixture.{requirement_name}"),
        timeout_ms: Some(5_000),
        topology_id: topology_id.to_string(),
        topology_revision_id: revision_id.to_string(),
        link_source_endpoint: "10.0.0.2:9000:consumer".to_string(),
        link_target_endpoint: "10.0.0.1:8080:provider".to_string(),
        credential_ref: String::new(),
        credential_generation: 2,
        context_generation: 2,
        desired_state: "ACTIVE".to_string(),
        observed_state: "ACTIVE".to_string(),
        health: "HEALTHY".to_string(),
        drift: Vec::new(),
        last_operation_id: operation_id.to_string(),
        state: ApiBindingState::Active,
        optional: false,
        reason: String::new(),
        created_at: "unix-ms:1".to_string(),
        updated_at: "unix-ms:2".to_string(),
    }
}

fn unique_suffix() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    (nanos as u64) ^ u64::from(std::process::id())
}
