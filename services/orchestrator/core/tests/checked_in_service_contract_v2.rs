use orchestrator_core::{ServiceReleaseContract, lint_service_openapi_yaml};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn parse(document: &str) -> ServiceReleaseContract {
    ServiceReleaseContract::from_yaml_str(document).expect("checked-in v2 release must be valid")
}

#[test]
fn cross_machine_services_publish_unambiguous_v2_contracts() {
    let auth = parse(include_str!(
        "../../../../services/auth-service/release.yaml"
    ));
    assert_eq!(auth.contract_version, 2);
    assert!(auth.release.apis.iter().any(|api| {
        api.api_id == "auth.user.permission.check"
            && api.version == "1.0.0"
            && api.path_prefix == "/auth/admin/permission-check"
            && api.auth_mode == "workload"
            && api.permission == "auth.permission.check"
            && api.visibility == "explicit"
    }));

    let worker = parse(include_str!(
        "../../../../services/judge-worker/release.yaml"
    ));
    assert_eq!(worker.contract_version, 2);
    assert_eq!(worker.runtime_contract.id, "judge-sandbox-v1");
    assert_eq!(
        worker
            .requirements()
            .iter()
            .map(|requirement| (requirement.binding_name(), requirement.api_id()))
            .collect::<Vec<_>>(),
        vec![
            ("judge_control", "judge.worker.control"),
            ("storage_get", "storage.object.get")
        ]
    );

    let judge = parse(include_str!("../../../../services/judge-api/release.yaml"));
    assert_eq!(judge.runtime_contract.id, "standard-container-v1");
    assert!(
        judge
            .release
            .apis
            .iter()
            .any(|api| api.api_id == "judge.worker.control"
                && api.path_prefix == "/api/judge/worker"
                && api.version == "1.0.0")
    );
    assert!(judge.requirements().iter().any(|requirement| {
        requirement.binding_name() == "permission_check"
            && requirement.api_id() == "auth.user.permission.check"
    }));
    let mut judge_submission_permissions = judge
        .release
        .permissions
        .iter()
        .map(String::as_str)
        .filter(|permission| permission.starts_with("judge.submission."))
        .collect::<Vec<_>>();
    judge_submission_permissions.sort_unstable();
    assert_eq!(
        judge_submission_permissions,
        vec![
            "judge.submission.manage",
            "judge.submission.view.all",
            "judge.submission.view.own",
        ]
    );
    assert_eq!(
        judge.release.routes[0].permission,
        "judge.submission.view.own"
    );
    assert!(
        judge
            .events
            .subscribes
            .iter()
            .any(|event| event.event_id() == "io.ojos.problem.snapshot.v1")
    );
    assert_eq!(
        judge
            .events
            .subscribes
            .iter()
            .map(|event| (event.event_id(), event.consumer_group()))
            .collect::<Vec<_>>(),
        vec![
            ("io.ojos.problem.deleted.v1", "judge-api"),
            ("io.ojos.problem.snapshot.v1", "judge-api"),
        ]
    );

    let problem = parse(include_str!(
        "../../../../services/problem-service/release.yaml"
    ));
    assert_eq!(problem.runtime_contract.id, "standard-container-v1");
    assert_eq!(
        problem.release.routes[0].path,
        "/api/problem/admin/artifact-gc/**"
    );
    assert_eq!(problem.release.routes[0].method, "ANY");
    assert_eq!(problem.release.routes[0].permission, "problem.manage.data");
    assert_eq!(problem.release.routes[1].path, "/api/problem/**");
    assert_eq!(problem.release.routes[1].permission, "problem.view");
    assert!(
        problem
            .events
            .publishes
            .iter()
            .any(|event| event.event_id() == "io.ojos.problem.snapshot.v1")
    );
    assert_eq!(
        problem
            .events
            .publishes
            .iter()
            .map(|event| event.event_id())
            .collect::<Vec<_>>(),
        vec!["io.ojos.problem.deleted.v1", "io.ojos.problem.snapshot.v1",]
    );
    assert!(problem.requirements().iter().any(|requirement| {
        requirement.binding_name() == "permission_check"
            && requirement.api_id() == "auth.user.permission.check"
    }));

    let user = parse(include_str!(
        "../../../../services/user-service/release.yaml"
    ));
    assert_eq!(user.contract_version, 2);
    assert!(user.requirements().iter().any(|requirement| {
        requirement.binding_name() == "permission_check"
            && requirement.api_id() == "auth.user.permission.check"
    }));
    assert!(
        problem
            .requirements()
            .iter()
            .any(|requirement| requirement.binding_name() == "storage_head")
    );
    let storage_delete = problem
        .requirements()
        .iter()
        .find(|requirement| {
            requirement.binding_name() == "storage_delete"
                && requirement.api_id() == "storage.object.delete"
        })
        .expect("problem-service must require storage_delete");
    assert_eq!(storage_delete.timeout_ms(), Some(60_000));

    let storage = parse(include_str!(
        "../../../../services/storage-service/release.yaml"
    ));
    assert_eq!(storage.runtime_contract.id, "standard-container-v1");
    for api_id in [
        "storage.object.get",
        "storage.object.put",
        "storage.object.head",
        "storage.object.delete",
    ] {
        assert!(
            storage.release.apis.iter().any(|api| api.api_id == api_id),
            "storage provider is missing {api_id}"
        );
    }
}

#[test]
fn checked_in_service_openapi_matches_every_published_contract_surface() {
    for (release, openapi, expected_service, expected_operations) in [
        (
            include_str!("../../../../services/auth-service/release.yaml"),
            include_str!("../../../../services/auth-service/openapi.yaml"),
            "auth-service",
            3,
        ),
        (
            include_str!("../../../../services/judge-api/release.yaml"),
            include_str!("../../../../services/judge-api/openapi.yaml"),
            "judge-api",
            16,
        ),
        (
            include_str!("../../../../services/problem-service/release.yaml"),
            include_str!("../../../../services/problem-service/openapi.yaml"),
            "problem-service",
            14,
        ),
        (
            include_str!("../../../../services/storage-service/release.yaml"),
            include_str!("../../../../services/storage-service/openapi.yaml"),
            "storage-service",
            5,
        ),
        (
            include_str!("../../../../services/user-service/release.yaml"),
            include_str!("../../../../services/user-service/openapi.yaml"),
            "user-service",
            6,
        ),
    ] {
        let contract = parse(release);
        let report = lint_service_openapi_yaml(&contract, openapi)
            .unwrap_or_else(|error| panic!("{expected_service} OpenAPI drift: {error}"));
        assert_eq!(report.service_id, expected_service);
        assert_eq!(report.operations.len(), expected_operations);
    }
}

#[test]
fn store_index_checksums_match_published_service_contracts() {
    let index: Value = serde_json::from_str(include_str!("../../../../store/index.json"))
        .expect("checked-in Store index must be valid JSON");
    let modules = index["modules"]
        .as_array()
        .expect("Store index modules must be an array");
    for (service_id, release) in [
        (
            "auth-service",
            include_bytes!("../../../../services/auth-service/release.yaml").as_slice(),
        ),
        (
            "judge-api",
            include_bytes!("../../../../services/judge-api/release.yaml").as_slice(),
        ),
        (
            "problem-service",
            include_bytes!("../../../../services/problem-service/release.yaml").as_slice(),
        ),
        (
            "storage-service",
            include_bytes!("../../../../services/storage-service/release.yaml").as_slice(),
        ),
        (
            "user-service",
            include_bytes!("../../../../services/user-service/release.yaml").as_slice(),
        ),
    ] {
        let module = modules
            .iter()
            .find(|module| module["id"] == service_id)
            .unwrap_or_else(|| panic!("Store index has no {service_id}"));
        let expected = format!("sha256:{:x}", Sha256::digest(release));
        assert_eq!(
            module["checksum"].as_str(),
            Some(expected.as_str()),
            "Store index checksum drift for {service_id}"
        );
    }
}

#[test]
fn generic_cross_machine_fixtures_are_manifest_only_v2_contracts() {
    let provider = parse(include_str!(
        "../../../../deploy/cross-machine/fixture/contracts/echo-provider.release.yaml"
    ));
    let consumer = parse(include_str!(
        "../../../../deploy/cross-machine/fixture/contracts/echo-consumer.release.yaml"
    ));
    let auth = parse(include_str!(
        "../../../../deploy/cross-machine/fixture/contracts/auth-permission-provider.release.yaml"
    ));

    assert_eq!(provider.release.service_name, "contract-echo-provider");
    assert!(
        provider
            .release
            .apis
            .iter()
            .any(|api| { api.api_id == "fixture.contract.echo" && api.auth_mode == "workload" })
    );
    assert_eq!(consumer.release.service_name, "contract-echo-consumer");
    let requirements = consumer.requirements();
    assert_eq!(
        requirements
            .iter()
            .map(|requirement| (requirement.binding_name(), requirement.api_id()))
            .collect::<Vec<_>>(),
        vec![
            ("echo", "fixture.contract.echo"),
            ("permission_check", "auth.user.permission.check"),
        ]
    );
    assert!(
        requirements
            .iter()
            .find(|requirement| requirement.binding_name() == "echo")
            .is_some_and(|requirement| requirement.optional())
    );
    assert!(
        requirements
            .iter()
            .find(|requirement| requirement.binding_name() == "permission_check")
            .is_some_and(|requirement| !requirement.optional())
    );
    assert!(auth.release.apis.iter().any(|api| {
        api.api_id == "auth.user.permission.check"
            && api.auth_mode == "workload"
            && api.permission == "auth.permission.check"
    }));
}

#[test]
fn production_contracts_do_not_reintroduce_global_management_credentials() {
    for document in [
        include_str!("../../../../services/auth-service/release.yaml"),
        include_str!("../../../../services/judge-worker/release.yaml"),
        include_str!("../../../../services/judge-api/release.yaml"),
        include_str!("../../../../services/problem-service/release.yaml"),
        include_str!("../../../../services/storage-service/release.yaml"),
        include_str!("../../../../services/user-service/release.yaml"),
    ] {
        let contract = parse(document);
        assert!(contract.release.service_identity.allowed_apis.is_empty());
        assert!(
            contract
                .release
                .secrets
                .iter()
                .all(|secret| !matches!(secret.as_str(), "worker-token" | "admin-token"))
        );
    }
}

#[test]
fn cross_machine_wire_contracts_have_versioned_checked_in_schemas() {
    for (document, expected_id) in [
        (
            include_str!(
                "../../../../platform/schemas/orchestrator/service-context-v1.schema.json"
            ),
            "https://schemas.ojos.dev/orchestrator/service-context-v1.schema.json",
        ),
        (
            include_str!("../../../../platform/schemas/orchestrator/api-binding-v1.schema.json"),
            "https://schemas.ojos.dev/orchestrator/api-binding-v1.schema.json",
        ),
        (
            include_str!("../../../../platform/schemas/orchestrator/runtime-report-v1.schema.json"),
            "https://schemas.ojos.dev/orchestrator/runtime-report-v1.schema.json",
        ),
        (
            include_str!(
                "../../../../platform/schemas/orchestrator/api-resource-ref-v1.schema.json"
            ),
            "https://schemas.ojos.dev/orchestrator/api-resource-ref-v1.schema.json",
        ),
        (
            include_str!("../../../../platform/schemas/orchestrator/event-context-v1.schema.json"),
            "https://schemas.ojos.dev/orchestrator/event-context-v1.schema.json",
        ),
    ] {
        let schema: Value = serde_json::from_str(document).expect("schema must be valid JSON");
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["$id"], expected_id);
        assert_eq!(schema["additionalProperties"], false);
    }
}
