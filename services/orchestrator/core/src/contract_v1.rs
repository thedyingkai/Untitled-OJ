#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum V1Role {
    Viewer,
    Operator,
    Admin,
}

impl V1Role {
    pub const fn permission(self) -> &'static str {
        match self {
            Self::Viewer => "orchestrator.read",
            Self::Operator => "orchestrator.operate",
            Self::Admin => "orchestrator.admin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V1ActionDescriptor {
    pub action_id: &'static str,
    pub target_type: &'static str,
    pub role: V1Role,
    pub asynchronous: bool,
}

const fn action(
    action_id: &'static str,
    target_type: &'static str,
    role: V1Role,
    asynchronous: bool,
) -> V1ActionDescriptor {
    V1ActionDescriptor {
        action_id,
        target_type,
        role,
        asynchronous,
    }
}

/// The only public v1 action vocabulary. Internal release-pipeline steps such
/// as route/config/secret/migration are deliberately absent.
pub const V1_ACTIONS: &[V1ActionDescriptor] = &[
    action("catalog.list", "Catalog", V1Role::Viewer, false),
    action("catalog.search", "Catalog", V1Role::Viewer, false),
    action("catalog.register", "Catalog", V1Role::Admin, false),
    action("catalog.remove", "Catalog", V1Role::Admin, false),
    action("release.import", "Release", V1Role::Operator, false),
    action("release.validate", "Release", V1Role::Operator, false),
    action("release.install", "Release", V1Role::Operator, true),
    action("release.upgrade", "Release", V1Role::Operator, true),
    action("release.rollback", "Release", V1Role::Operator, true),
    action("release.delete", "Release", V1Role::Admin, false),
    action("node.register", "Node", V1Role::Admin, false),
    action("node.revoke", "Node", V1Role::Admin, false),
    action("node.list", "Node", V1Role::Viewer, false),
    action("node.health", "Node", V1Role::Viewer, false),
    action("node.drain", "Node", V1Role::Admin, true),
    action("node.remove", "Node", V1Role::Admin, true),
    action("deployment.list", "Deployment", V1Role::Viewer, false),
    action("deployment.get", "Deployment", V1Role::Viewer, false),
    action("deployment.start", "Deployment", V1Role::Operator, true),
    action("deployment.stop", "Deployment", V1Role::Operator, true),
    action("deployment.restart", "Deployment", V1Role::Operator, true),
    action("deployment.uninstall", "Deployment", V1Role::Admin, true),
    action("deployment.health", "Deployment", V1Role::Viewer, false),
    action("resource.purge", "ResourceClaim", V1Role::Admin, true),
    action("topology.draft", "Topology", V1Role::Operator, false),
    action("topology.revision", "Topology", V1Role::Operator, false),
    action(
        "topology.endpoint.edit",
        "Topology",
        V1Role::Operator,
        false,
    ),
    action("topology.link.edit", "Topology", V1Role::Operator, false),
    action("topology.validate", "Topology", V1Role::Operator, false),
    action("topology.diff", "Topology", V1Role::Operator, false),
    action("topology.apply", "Topology", V1Role::Operator, true),
    action("topology.rollback", "Topology", V1Role::Operator, true),
    action("topology.status", "Topology", V1Role::Viewer, false),
    action("topology.export", "Topology", V1Role::Viewer, false),
    action("operation.plan", "Operation", V1Role::Operator, false),
    action("operation.confirm", "Operation", V1Role::Operator, false),
    action("operation.apply", "Operation", V1Role::Operator, true),
    action("operation.cancel", "Operation", V1Role::Operator, true),
    action("operation.retry", "Operation", V1Role::Operator, true),
    action("operation.rollback", "Operation", V1Role::Operator, true),
    action("operation.logs", "Operation", V1Role::Viewer, false),
    action("operation.events", "Operation", V1Role::Viewer, false),
    // Report creation is a bounded transactional snapshot in v1; unlike
    // install/apply it does not perform remote side effects and returns 201.
    action("diagnostic.create", "Diagnostic", V1Role::Operator, false),
    action("diagnostic.list", "Diagnostic", V1Role::Viewer, false),
    action("diagnostic.get", "Diagnostic", V1Role::Viewer, false),
    action("diagnostic.export", "Diagnostic", V1Role::Viewer, false),
];

pub fn v1_action(action_id: &str) -> Option<&'static V1ActionDescriptor> {
    V1_ACTIONS
        .iter()
        .find(|descriptor| descriptor.action_id == action_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_yaml::Value;
    use std::collections::BTreeSet;

    #[derive(Debug, Deserialize)]
    struct CheckedInActionMatrix {
        schema_version: u64,
        api_version: String,
        product: String,
        roles: std::collections::BTreeMap<String, String>,
        actions: Vec<CheckedInAction>,
    }

    #[derive(Debug, Deserialize)]
    struct CheckedInAction {
        action: String,
        target_type: String,
        role: String,
        asynchronous: bool,
    }

    #[derive(Debug, Deserialize)]
    struct CheckedInActionRoute {
        action: String,
        method: String,
        openapi_path: String,
        probe_path: String,
    }

    #[test]
    fn published_v1_vocabulary_is_unique_and_contains_no_internal_crud() {
        let ids = V1_ACTIONS
            .iter()
            .map(|descriptor| descriptor.action_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), V1_ACTIONS.len());
        for forbidden in [
            "operation.create",
            "operation.update",
            "operation.delete",
            "route.create",
            "config.create",
            "secret.create",
            "log.delete",
            "diagnostic.update",
        ] {
            assert!(!ids.contains(forbidden), "{forbidden} leaked into v1");
        }
    }

    #[test]
    fn every_v1_action_has_a_fixed_rbac_permission() {
        assert!(V1_ACTIONS.iter().all(|descriptor| matches!(
            descriptor.role.permission(),
            "orchestrator.read" | "orchestrator.operate" | "orchestrator.admin"
        )));
    }

    #[test]
    fn checked_in_v1_action_matrix_exactly_matches_compiled_contract() {
        let matrix: CheckedInActionMatrix = serde_yaml::from_str(include_str!(
            "../../../../platform/schemas/orchestrator/actions-v1.yaml"
        ))
        .expect("valid checked-in v1 action matrix");
        assert_eq!(matrix.schema_version, 1);
        assert_eq!(matrix.api_version, "v1");
        assert_eq!(matrix.product, "OJOS Orchestrator");
        assert_eq!(
            matrix.roles.get("viewer").map(String::as_str),
            Some("orchestrator.read")
        );
        assert_eq!(
            matrix.roles.get("operator").map(String::as_str),
            Some("orchestrator.operate")
        );
        assert_eq!(
            matrix.roles.get("admin").map(String::as_str),
            Some("orchestrator.admin")
        );
        assert_eq!(matrix.actions.len(), V1_ACTIONS.len());
        for (checked_in, compiled) in matrix.actions.iter().zip(V1_ACTIONS) {
            assert_eq!(checked_in.action, compiled.action_id);
            assert_eq!(checked_in.target_type, compiled.target_type);
            assert_eq!(
                checked_in.role,
                match compiled.role {
                    V1Role::Viewer => "viewer",
                    V1Role::Operator => "operator",
                    V1Role::Admin => "admin",
                }
            );
            assert_eq!(checked_in.asynchronous, compiled.asynchronous);
        }
    }

    #[test]
    fn openapi_declares_the_exact_compiled_action_vocabulary() {
        let document = openapi_document();
        assert_eq!(document["openapi"].as_str(), Some("3.1.0"));
        assert_eq!(document["info"]["version"].as_str(), Some("1.0.0"));
        let declared = document["x-ojos-published-actions"]
            .as_sequence()
            .expect("OpenAPI x-ojos-published-actions")
            .iter()
            .map(|value| value.as_str().expect("action id"))
            .collect::<Vec<_>>();
        let compiled = V1_ACTIONS
            .iter()
            .map(|descriptor| descriptor.action_id)
            .collect::<Vec<_>>();
        assert_eq!(declared, compiled);
    }

    #[test]
    fn every_published_action_has_a_declared_reachable_openapi_operation() {
        let document = openapi_document();
        let routes = serde_yaml::from_value::<Vec<CheckedInActionRoute>>(
            document["x-ojos-action-routes"].clone(),
        )
        .expect("valid x-ojos-action-routes");
        assert_eq!(routes.len(), V1_ACTIONS.len());
        let route_actions = routes
            .iter()
            .map(|route| route.action.as_str())
            .collect::<BTreeSet<_>>();
        let published_actions = V1_ACTIONS
            .iter()
            .map(|descriptor| descriptor.action_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(route_actions, published_actions);

        for route in &routes {
            let descriptor = v1_action(&route.action).expect("route action is published");
            let method = route.method.to_ascii_lowercase();
            assert!(
                matches!(method.as_str(), "get" | "post" | "put" | "patch" | "delete"),
                "{} has invalid HTTP method {}",
                route.action,
                route.method
            );
            let operation = &document["paths"][route.openapi_path.as_str()][method.as_str()];
            assert!(
                !operation.is_null(),
                "{} points to missing OpenAPI operation {} {}",
                route.action,
                route.method,
                route.openapi_path
            );
            assert!(
                route.probe_path.starts_with("/api/v1/"),
                "{} probe must use the formal /api/v1 prefix",
                route.action
            );
            assert_eq!(
                !operation["responses"]["202"].is_null(),
                descriptor.asynchronous,
                "{} asynchronous metadata disagrees with {} {} responses",
                route.action,
                route.method,
                route.openapi_path
            );
        }
    }

    #[test]
    fn every_openapi_mutation_requires_an_idempotency_key() {
        let document = openapi_document();
        let paths = document["paths"].as_mapping().expect("OpenAPI paths");
        for (path, path_item) in paths {
            let path = path.as_str().expect("string path");
            for method in ["post", "put", "patch", "delete"] {
                let Some(operation) = mapping_value(path_item, method) else {
                    continue;
                };
                assert!(
                    has_parameter_ref(operation, "#/components/parameters/IdempotencyKey"),
                    "{method} {path} must require Idempotency-Key"
                );
            }
        }
    }

    #[test]
    fn every_v1_collection_uses_limit_and_cursor_pagination() {
        let document = openapi_document();
        for path in [
            "/operations",
            "/operations/{operationId}/logs",
            "/store/catalogs",
            "/store/packages",
            "/nodes",
            "/deployments",
            "/topologies",
            "/topologies/{topologyId}/revisions",
            "/diagnostics",
        ] {
            let operation = &document["paths"][path]["get"];
            assert!(!operation.is_null(), "missing GET collection route {path}");
            for reference in [
                "#/components/parameters/Limit",
                "#/components/parameters/Cursor",
            ] {
                assert!(
                    has_parameter_ref(operation, reference),
                    "GET {path} must declare {reference}"
                );
            }
        }
    }

    #[test]
    fn catalog_registration_accepts_a_write_only_bootstrap_public_key() {
        let document = openapi_document();
        assert_eq!(
            document["paths"]["/store/catalogs"]["post"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str(),
            Some("#/components/schemas/CatalogSourceRegistration")
        );
        let registration = &document["components"]["schemas"]["CatalogSourceRegistration"];
        assert_eq!(
            registration["properties"]["public_key"]["writeOnly"].as_bool(),
            Some(true)
        );
        assert_eq!(
            registration["properties"]["public_key"]["minLength"].as_i64(),
            Some(44)
        );
        assert_eq!(
            registration["properties"]["public_key"]["maxLength"].as_i64(),
            Some(44)
        );
        assert!(
            document["components"]["schemas"]["CatalogSource"]["properties"]["public_key"]
                .is_null(),
            "CatalogSource responses must never expose the bootstrap public key"
        );
    }

    #[test]
    fn topology_revision_mutations_require_optimistic_concurrency() {
        let document = openapi_document();
        for path in [
            "/topologies/{topologyId}/revisions",
            "/topologies/{topologyId}:apply",
            "/topologies/{topologyId}:rollback",
        ] {
            let operation = &document["paths"][path]["post"];
            assert!(!operation.is_null(), "missing topology mutation {path}");
            assert!(
                has_parameter_ref(operation, "#/components/parameters/IfMatch"),
                "POST {path} must require If-Match"
            );
        }
    }

    #[test]
    fn every_accepted_mutation_returns_a_typed_operation_id() {
        let document = openapi_document();
        let paths = document["paths"].as_mapping().expect("OpenAPI paths");
        let mut accepted_operations = 0usize;
        for (path, path_item) in paths {
            let path = path.as_str().expect("string path");
            for method in ["post", "put", "patch", "delete"] {
                let Some(operation) = mapping_value(path_item, method) else {
                    continue;
                };
                let Some(response) = mapping_value(&operation["responses"], "202") else {
                    continue;
                };
                accepted_operations += 1;
                assert_eq!(
                    response["$ref"].as_str(),
                    Some("#/components/responses/AsyncOperation"),
                    "{method} {path} must use the typed asynchronous response"
                );
            }
        }
        assert!(
            accepted_operations > 0,
            "v1 must expose asynchronous mutations"
        );

        let schema = &document["components"]["schemas"]["AsyncOperationEnvelope"];
        let data_required = schema["properties"]["data"]["required"]
            .as_sequence()
            .expect("AsyncOperationEnvelope.data.required")
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        assert!(data_required.contains("operation_id"));
        assert!(data_required.contains("operation"));
    }

    #[test]
    fn openapi_binds_link_probe_to_the_signed_catalog_and_exact_manifest_shape() {
        let document = openapi_document();
        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["RuntimeCapabilityV2"]["enum"]
                .as_sequence()
                .expect("RuntimeCapabilityV2 enum")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            vec!["link-probe-v1"]
        );
        assert_eq!(
            schemas["CatalogReleaseV2"]["properties"]["runtime_capabilities"]["items"]["$ref"]
                .as_str(),
            Some("#/components/schemas/RuntimeCapabilityV2")
        );

        let api = &schemas["LinkProbeV1ManifestApi"]["properties"];
        for (field, expected) in [
            ("api_id", "orchestrator.link-probe.v1"),
            ("protocol", "http"),
            ("port_name", "default"),
            ("path_prefix", "/probe"),
            ("visibility", "global"),
            ("auth_mode", "public"),
            ("permission", "public"),
            ("stability", "stable"),
            ("version", "v1"),
        ] {
            assert_eq!(api[field]["const"].as_str(), Some(expected), "{field}");
        }
        assert_eq!(api["methods"]["minItems"].as_u64(), Some(1));
        assert_eq!(api["methods"]["maxItems"].as_u64(), Some(1));
        assert_eq!(api["methods"]["items"]["const"].as_str(), Some("GET"));
        assert_eq!(api["allowed_callers"]["maxItems"].as_u64(), Some(0));
        assert_eq!(api["denied_callers"]["maxItems"].as_u64(), Some(0));

        assert_eq!(
            document["paths"]["/store/packages"]["get"]["responses"]["200"]["$ref"].as_str(),
            Some("#/components/responses/CatalogPackages")
        );
    }

    fn openapi_document() -> Value {
        serde_yaml::from_str(include_str!(
            "../../../../platform/schemas/orchestrator/openapi-v1.yaml"
        ))
        .expect("valid checked-in OpenAPI v1 document")
    }

    fn mapping_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
        value.as_mapping()?.get(Value::String(key.to_string()))
    }

    fn has_parameter_ref(operation: &Value, expected: &str) -> bool {
        operation["parameters"]
            .as_sequence()
            .into_iter()
            .flatten()
            .any(|parameter| parameter["$ref"].as_str() == Some(expected))
    }
}
