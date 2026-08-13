use orchestrator_core::{
    ContributionActivationV1, ContributionApiSurfaceV1, ContributionPermissionDefinitionV1,
    ContributionRevisionV1, PermissionAssignmentV1, PermissionSubjectKindV1, ProjectionReceiptV1,
    ProjectionTargetV1,
};
use orchestrator_storage::{
    ContributionRepository, PostgresOptions, PostgresOrchestratorStore, PostgresTlsTrust,
};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn required_database_url() -> Option<String> {
    let configured = std::env::var("OJOS_TEST_POSTGRES_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let required = std::env::var("OJOS_REQUIRE_POSTGRES_CONTRACT")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false);
    if configured.is_none() && required {
        panic!("OJOS_TEST_POSTGRES_URL must be set when OJOS_REQUIRE_POSTGRES_CONTRACT=1");
    }
    configured
}

fn digest(ch: char) -> String {
    format!("sha256:{}", ch.to_string().repeat(64))
}

fn revision(suffix: &str, generation: u64, previous: Option<String>) -> ContributionRevisionV1 {
    let service_id = format!("contest-{suffix}");
    ContributionRevisionV1::stage(
        format!("scope:{suffix}"),
        format!("{service_id}-deployment-{generation}"),
        &service_id,
        digest('a'),
        digest('b'),
        generation,
        previous,
        vec![ContributionApiSurfaceV1 {
            api_id: format!("{service_id}.api"),
            api_version: "1.0.0".to_string(),
            protocol: "http".to_string(),
            base_path: "/v1".to_string(),
        }],
        Vec::new(),
        vec![ContributionPermissionDefinitionV1 {
            key: format!("{service_id}.read"),
            title: "Read contests".to_string(),
            description: String::new(),
        }],
        Vec::new(),
        Vec::new(),
    )
    .expect("valid contribution fixture")
}

/// CI supplies a dedicated TLS PostgreSQL database and sets
/// OJOS_REQUIRE_POSTGRES_CONTRACT=1. Local runs without PostgreSQL keep this
/// contract opt-in while still compiling every PostgreSQL repository path.
#[test]
fn postgres_contribution_repository_contract_when_configured() {
    let Some(database_url) = required_database_url() else {
        eprintln!("skipping PostgreSQL contribution contract: database is not configured");
        return;
    };
    let mut options = PostgresOptions::default();
    if let Ok(path) = std::env::var("OJOS_TEST_POSTGRES_CA") {
        options.tls_trust = PostgresTlsTrust::CaCertificate(PathBuf::from(path));
    }
    let store = PostgresOrchestratorStore::connect(&database_url, options)
        .expect("connect dedicated PostgreSQL contribution contract database");
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    );
    let suffix = suffix.replace('-', "");

    let first = revision(&suffix, 1, None);
    store
        .insert_contribution_revision(&first)
        .expect("insert staged revision");
    store
        .insert_contribution_revision(&first)
        .expect("exact revision insert is idempotent");

    let activation_id = format!("activation-{suffix}");
    let activation = ContributionActivationV1::prepare(&activation_id, &first, None)
        .expect("prepare activation");
    let receipts = [
        ProjectionReceiptV1::pending(&activation_id, ProjectionTargetV1::Auth, &first)
            .expect("auth receipt"),
        ProjectionReceiptV1::pending(&activation_id, ProjectionTargetV1::Gateway, &first)
            .expect("gateway receipt"),
    ];
    store
        .put_contribution_activation_bundle(&activation, &receipts)
        .expect("persist activation bundle atomically");
    assert_eq!(
        store
            .contribution_projection_receipts(&activation_id)
            .expect("read receipts")
            .len(),
        2
    );

    let first_active = first.activate().expect("activate first revision");
    let head = store
        .compare_and_swap_contribution_head(None, &first_active)
        .expect("create initial head atomically with activation");
    assert!(
        store
            .compare_and_swap_contribution_head(Some(&digest('f')), &first_active)
            .is_err(),
        "stale ETag must not change the head"
    );

    let assignment = PermissionAssignmentV1 {
        assignment_id: format!("assignment-{suffix}"),
        scope_id: first.scope_id().to_string(),
        permission_key: format!("{}.read", first.service_id()),
        subject_kind: PermissionSubjectKindV1::Role,
        subject_id: format!("role-{suffix}"),
    };
    store
        .insert_permission_assignment(&assignment)
        .expect("insert independent permission assignment");

    let second = revision(&suffix, 2, Some(first.revision_id().to_string()));
    store
        .insert_contribution_revision(&second)
        .expect("insert upgrade revision");
    store
        .compare_and_swap_contribution_head(
            Some(head.etag()),
            &second.activate().expect("activate upgrade revision"),
        )
        .expect("CAS upgrade head");
    store
        .transition_contribution_revision(&first_active.retire().expect("retire old revision"))
        .expect("persist retirement");

    assert_eq!(
        store
            .permission_assignments(first.scope_id(), Some(&assignment.permission_key))
            .expect("read independent assignments"),
        vec![assignment],
        "revision upgrade and retirement must not delete assignments"
    );
}
