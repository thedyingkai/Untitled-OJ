use crate::{
    ApiOperationV3, EventContractV1, MediaSchemaContractV3, ParameterContractV3, PermissionScopeV1,
    RequestBodyContractV3, RouteContributionV1, ServiceContractV3,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const COMPATIBILITY_REPORT_SCHEMA: &str = "ojos.dev/compatibility-report/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatibilityReportV1 {
    pub schema_version: String,
    pub service_id: String,
    pub previous_service_version: semver::Version,
    pub current_service_version: semver::Version,
    pub compatible: bool,
    pub issues: Vec<CompatibilityIssueV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompatibilityIssueV1 {
    pub kind: String,
    pub api_id: String,
    pub operation_id: String,
    pub detail: String,
    pub previous_api_major: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_api_major: Option<u64>,
    pub accepted_by_major_bump: bool,
}

pub fn compare(
    previous: &ServiceContractV3,
    current: &ServiceContractV3,
) -> Result<CompatibilityReportV1, String> {
    crate::validate_contract_event_schemas(previous).map_err(|error| error.to_string())?;
    crate::validate_contract_event_schemas(current).map_err(|error| error.to_string())?;
    if previous.service_id != current.service_id {
        return Err(format!(
            "cannot compare service {} with {}",
            previous.service_id, current.service_id
        ));
    }
    if current.service_version <= previous.service_version {
        return Err(format!(
            "current service version {} must be greater than previous {}",
            current.service_version, previous.service_version
        ));
    }
    let previous_operations = previous
        .operations
        .iter()
        .map(|operation| (operation.operation_id.as_str(), operation))
        .collect::<BTreeMap<_, _>>();
    let current_operations = current
        .operations
        .iter()
        .map(|operation| (operation.operation_id.as_str(), operation))
        .collect::<BTreeMap<_, _>>();
    let current_api_majors = current
        .api_surfaces
        .iter()
        .map(|surface| (surface.api_id.as_str(), surface.version.major))
        .collect::<BTreeMap<_, _>>();
    let mut issues = Vec::new();

    for (operation_id, old) in previous_operations {
        let new = current_operations.get(operation_id).copied();
        let current_major = new
            .map(|operation| operation.api_version.major)
            .or_else(|| current_api_majors.get(old.api_id.as_str()).copied());
        let accepted = current_major.is_some_and(|major| major > old.api_version.major);
        let mut issue = |kind: &str, detail: String, accepted_by_major_bump: bool| {
            issues.push(CompatibilityIssueV1 {
                kind: kind.to_string(),
                api_id: old.api_id.clone(),
                operation_id: old.operation_id.clone(),
                detail,
                previous_api_major: old.api_version.major,
                current_api_major: current_major,
                accepted_by_major_bump,
            });
        };
        let Some(new) = new else {
            issue(
                "operation-removed",
                "operation was removed".to_string(),
                accepted,
            );
            continue;
        };
        let new_api_major = new.api_version.major;
        let same_api_line = old.api_id == new.api_id && new_api_major == old.api_version.major;
        if old.api_id != new.api_id {
            issue(
                "api-changed",
                format!("API changed from {} to {}", old.api_id, new.api_id),
                accepted,
            );
        }
        if old.provider_path != new.provider_path {
            issue(
                "path-changed",
                format!(
                    "provider path changed from {} to {}",
                    old.provider_path, new.provider_path
                ),
                accepted,
            );
        }
        if old.method != new.method {
            issue(
                "method-changed",
                format!("method changed from {} to {}", old.method, new.method),
                accepted,
            );
        }
        if auth_rank(&new.auth) > auth_rank(&old.auth) {
            issue(
                "auth-strengthened",
                format!("auth changed from {} to {}", old.auth, new.auth),
                accepted,
            );
        }
        if old.permission != new.permission {
            issue(
                "permission-changed",
                format!(
                    "permission changed from {:?} to {:?}",
                    old.permission, new.permission
                ),
                accepted,
            );
        }
        if effective_permission_scope(old) != effective_permission_scope(new) {
            issue(
                "permission-scope-changed",
                format!(
                    "permission scope changed from {:?} to {:?}",
                    old.permission_scope, new.permission_scope
                ),
                accepted,
            );
        }
        let mut schema_issue = |kind: &str, detail: String| issue(kind, detail, !same_api_line);
        compare_parameters(old, new, &mut schema_issue);
        compare_request_body(
            old.request_body.as_ref(),
            new.request_body.as_ref(),
            &mut schema_issue,
        );
        compare_responses(old, new, &mut schema_issue);
    }
    compare_routes(previous, current, &current_api_majors, &mut issues);
    compare_event_side(
        "published-event",
        &previous.events.publishes,
        &current.events.publishes,
        &mut issues,
    );
    compare_event_side(
        "subscribed-event",
        &previous.events.subscribes,
        &current.events.subscribes,
        &mut issues,
    );
    issues.sort_by(|left, right| {
        left.api_id
            .cmp(&right.api_id)
            .then(left.operation_id.cmp(&right.operation_id))
            .then(left.kind.cmp(&right.kind))
            .then(left.detail.cmp(&right.detail))
    });
    let compatible = issues.iter().all(|issue| issue.accepted_by_major_bump);
    Ok(CompatibilityReportV1 {
        schema_version: COMPATIBILITY_REPORT_SCHEMA.to_string(),
        service_id: current.service_id.clone(),
        previous_service_version: previous.service_version.clone(),
        current_service_version: current.service_version.clone(),
        compatible,
        issues,
    })
}

fn effective_permission_scope(operation: &ApiOperationV3) -> Option<PermissionScopeV1> {
    operation.permission_scope.clone().or_else(|| {
        operation
            .permission
            .as_ref()
            .map(|_| PermissionScopeV1::system())
    })
}

fn compare_routes(
    previous: &ServiceContractV3,
    current: &ServiceContractV3,
    current_api_majors: &BTreeMap<&str, u64>,
    issues: &mut Vec<CompatibilityIssueV1>,
) {
    let old_operations = previous
        .operations
        .iter()
        .map(|operation| (operation.operation_id.as_str(), operation))
        .collect::<BTreeMap<_, _>>();
    let current_routes = current
        .routes
        .iter()
        .map(|route| (route.operation_id.as_str(), route))
        .collect::<BTreeMap<_, _>>();
    for old in &previous.routes {
        let Some(operation) = old_operations.get(old.operation_id.as_str()).copied() else {
            continue;
        };
        let current_major = current_api_majors.get(operation.api_id.as_str()).copied();
        let accepted = current_major.is_some_and(|major| major > operation.api_version.major);
        match current_routes.get(old.operation_id.as_str()).copied() {
            None => push_route_issue(
                issues,
                old,
                operation.api_version.major,
                current_major,
                accepted,
                "external-route-removed",
                "external exposure was removed".to_string(),
            ),
            Some(new) => {
                if old.audience != new.audience || old.method != new.method || old.path != new.path
                {
                    push_route_issue(
                        issues,
                        old,
                        operation.api_version.major,
                        current_major,
                        accepted,
                        "external-route-changed",
                        format!(
                            "external route changed from {}/{}/{} to {}/{}/{}",
                            old.audience, old.method, old.path, new.audience, new.method, new.path
                        ),
                    );
                }
            }
        }
    }
}

fn push_route_issue(
    issues: &mut Vec<CompatibilityIssueV1>,
    route: &RouteContributionV1,
    previous_major: u64,
    current_major: Option<u64>,
    accepted: bool,
    kind: &str,
    detail: String,
) {
    issues.push(CompatibilityIssueV1 {
        kind: kind.to_string(),
        api_id: route.api_id.clone(),
        operation_id: route.operation_id.clone(),
        detail,
        previous_api_major: previous_major,
        current_api_major: current_major,
        accepted_by_major_bump: accepted,
    });
}

fn compare_event_side(
    kind_prefix: &str,
    previous: &[EventContractV1],
    current: &[EventContractV1],
    issues: &mut Vec<CompatibilityIssueV1>,
) {
    let exact = current
        .iter()
        .map(|event| ((event.event_type.as_str(), event.version), event))
        .collect::<BTreeMap<_, _>>();
    let latest = current
        .iter()
        .fold(BTreeMap::<&str, u32>::new(), |mut versions, event| {
            versions
                .entry(event.event_type.as_str())
                .and_modify(|version| *version = (*version).max(event.version))
                .or_insert(event.version);
            versions
        });
    for old in previous {
        let current_version = latest.get(old.event_type.as_str()).copied();
        let accepted = current_version.is_some_and(|version| version > old.version);
        let Some(new) = exact.get(&(old.event_type.as_str(), old.version)).copied() else {
            push_event_issue(
                issues,
                old,
                current_version,
                accepted,
                &format!("{kind_prefix}-removed"),
                if accepted {
                    format!(
                        "event {} major {} was retired in favor of a higher major",
                        old.event_type, old.version
                    )
                } else {
                    format!(
                        "event {} major {} was removed without a higher major replacement",
                        old.event_type, old.version
                    )
                },
            );
            continue;
        };
        if old.schema.digest != new.schema.digest {
            push_event_issue(
                issues,
                old,
                current_version,
                false,
                &format!("{kind_prefix}-schema-changed"),
                "event schema changed without increasing event major".to_string(),
            );
        }
        if old.delivery != new.delivery {
            push_event_issue(
                issues,
                old,
                current_version,
                false,
                &format!("{kind_prefix}-delivery-changed"),
                format!("delivery changed from {} to {}", old.delivery, new.delivery),
            );
        }
    }
}

fn push_event_issue(
    issues: &mut Vec<CompatibilityIssueV1>,
    event: &EventContractV1,
    current_version: Option<u32>,
    accepted: bool,
    kind: &str,
    detail: String,
) {
    issues.push(CompatibilityIssueV1 {
        kind: kind.to_string(),
        api_id: "events".to_string(),
        operation_id: event.event_type.clone(),
        detail,
        previous_api_major: u64::from(event.version),
        current_api_major: current_version.map(u64::from),
        accepted_by_major_bump: accepted,
    });
}

fn compare_parameters(
    old: &ApiOperationV3,
    new: &ApiOperationV3,
    issue: &mut impl FnMut(&str, String),
) {
    let old_parameters = parameter_map(&old.parameters);
    let new_parameters = parameter_map(&new.parameters);
    for (identity, current) in &new_parameters {
        match old_parameters.get(identity) {
            None if current.required => issue(
                "required-input-added",
                format!("required {} parameter {} was added", identity.0, identity.1),
            ),
            Some(previous) => {
                if !previous.required && current.required {
                    issue(
                        "input-became-required",
                        format!("{} parameter {} became required", identity.0, identity.1),
                    );
                }
                for detail in input_schema_breaks(&previous.schema, &current.schema, "schema") {
                    issue(
                        "input-schema-narrowed",
                        format!("{} parameter {}: {detail}", identity.0, identity.1),
                    );
                }
            }
            None => {}
        }
    }
}

fn parameter_map(
    parameters: &[ParameterContractV3],
) -> BTreeMap<(String, String), &ParameterContractV3> {
    parameters
        .iter()
        .map(|parameter| {
            (
                (parameter.location.clone(), parameter.name.clone()),
                parameter,
            )
        })
        .collect()
}

fn compare_request_body(
    old: Option<&RequestBodyContractV3>,
    new: Option<&RequestBodyContractV3>,
    issue: &mut impl FnMut(&str, String),
) {
    match (old, new) {
        (None, Some(current)) if current.required => issue(
            "required-request-body-added",
            "a required request body was added".to_string(),
        ),
        (Some(_), None) => issue(
            "request-body-removed",
            "the accepted request body was removed".to_string(),
        ),
        (Some(previous), Some(current)) => {
            if !previous.required && current.required {
                issue(
                    "request-body-became-required",
                    "request body became required".to_string(),
                );
            }
            compare_media(&previous.content, &current.content, true, "request", issue);
        }
        _ => {}
    }
}

fn compare_responses(
    old: &ApiOperationV3,
    new: &ApiOperationV3,
    issue: &mut impl FnMut(&str, String),
) {
    let current = new
        .responses
        .iter()
        .map(|response| (response.status.as_str(), response))
        .collect::<BTreeMap<_, _>>();
    for previous in &old.responses {
        let Some(response) = current.get(previous.status.as_str()) else {
            issue(
                "response-removed",
                format!("response status {} was removed", previous.status),
            );
            continue;
        };
        compare_media(
            &previous.content,
            &response.content,
            false,
            &format!("response {}", previous.status),
            issue,
        );
    }
}

fn compare_media(
    old: &[MediaSchemaContractV3],
    new: &[MediaSchemaContractV3],
    input: bool,
    location: &str,
    issue: &mut impl FnMut(&str, String),
) {
    let current = new
        .iter()
        .map(|media| (media.media_type.as_str(), media))
        .collect::<BTreeMap<_, _>>();
    for previous in old {
        let Some(media) = current.get(previous.media_type.as_str()) else {
            issue(
                if input {
                    "request-media-removed"
                } else {
                    "response-media-removed"
                },
                format!("{location} media type {} was removed", previous.media_type),
            );
            continue;
        };
        if let (Some(old_schema), Some(new_schema)) = (&previous.schema, &media.schema) {
            let details = if input {
                input_schema_breaks(old_schema, new_schema, "schema")
            } else {
                response_schema_breaks(old_schema, new_schema, "schema")
            };
            for detail in details {
                issue(
                    if input {
                        "input-schema-narrowed"
                    } else {
                        "response-schema-narrowed"
                    },
                    format!("{location} {}: {detail}", previous.media_type),
                );
            }
        }
    }
}

fn input_schema_breaks(old: &Value, new: &Value, pointer: &str) -> Vec<String> {
    let mut issues = common_schema_breaks(old, new, pointer);
    let old_required = string_set(old.get("required"));
    let new_required = string_set(new.get("required"));
    for name in new_required.difference(&old_required) {
        issues.push(format!("{pointer} added required field {name}"));
    }
    let old_properties = properties(old);
    let new_properties = properties(new);
    for name in old_properties.keys() {
        if !new_properties.contains_key(name) {
            issues.push(format!("{pointer} removed accepted input field {name}"));
        }
    }
    recurse_properties(old, new, pointer, true, &mut issues);
    issues
}

fn response_schema_breaks(old: &Value, new: &Value, pointer: &str) -> Vec<String> {
    let mut issues = common_schema_breaks(old, new, pointer);
    let old_properties = properties(old);
    let new_properties = properties(new);
    for name in old_properties.keys() {
        if !new_properties.contains_key(name) {
            issues.push(format!("{pointer} removed response field {name}"));
        }
    }
    recurse_properties(old, new, pointer, false, &mut issues);
    issues
}

fn common_schema_breaks(old: &Value, new: &Value, pointer: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if old.get("type") != new.get("type") {
        issues.push(format!("{pointer} changed type"));
    }
    let old_enum = value_set(old.get("enum"));
    let new_enum = value_set(new.get("enum"));
    if !old_enum.is_empty() && !new_enum.is_empty() && !old_enum.is_subset(&new_enum) {
        issues.push(format!("{pointer} narrowed enum values"));
    }
    if old_enum.is_empty() && !new_enum.is_empty() {
        issues.push(format!("{pointer} introduced an enum constraint"));
    }
    compare_lower_bound(old, new, pointer, "minimum", &mut issues);
    compare_lower_bound(old, new, pointer, "minLength", &mut issues);
    compare_lower_bound(old, new, pointer, "minItems", &mut issues);
    compare_upper_bound(old, new, pointer, "maximum", &mut issues);
    compare_upper_bound(old, new, pointer, "maxLength", &mut issues);
    compare_upper_bound(old, new, pointer, "maxItems", &mut issues);
    if old.get("pattern") != new.get("pattern") && new.get("pattern").is_some() {
        issues.push(format!("{pointer} added or changed a pattern constraint"));
    }
    if old.get("additionalProperties") != Some(&Value::Bool(false))
        && new.get("additionalProperties") == Some(&Value::Bool(false))
    {
        issues.push(format!("{pointer} disabled additional properties"));
    }
    issues
}

fn compare_lower_bound(
    old: &Value,
    new: &Value,
    pointer: &str,
    keyword: &str,
    issues: &mut Vec<String>,
) {
    let old = old.get(keyword).and_then(Value::as_f64);
    let new = new.get(keyword).and_then(Value::as_f64);
    if new.is_some_and(|new| old.is_none_or(|old| new > old)) {
        issues.push(format!("{pointer} increased {keyword}"));
    }
}

fn compare_upper_bound(
    old: &Value,
    new: &Value,
    pointer: &str,
    keyword: &str,
    issues: &mut Vec<String>,
) {
    let old = old.get(keyword).and_then(Value::as_f64);
    let new = new.get(keyword).and_then(Value::as_f64);
    if new.is_some_and(|new| old.is_none_or(|old| new < old)) {
        issues.push(format!("{pointer} decreased {keyword}"));
    }
}

fn recurse_properties(
    old: &Value,
    new: &Value,
    pointer: &str,
    input: bool,
    issues: &mut Vec<String>,
) {
    let old_properties = properties(old);
    let new_properties = properties(new);
    for (name, old_schema) in old_properties {
        let Some(new_schema) = new_properties.get(name) else {
            continue;
        };
        let nested = if input {
            input_schema_breaks(old_schema, new_schema, &format!("{pointer}.{name}"))
        } else {
            response_schema_breaks(old_schema, new_schema, &format!("{pointer}.{name}"))
        };
        issues.extend(nested);
    }
}

fn properties(value: &Value) -> BTreeMap<&str, &Value> {
    value
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect()
        })
        .unwrap_or_default()
}

fn string_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn value_set(value: Option<&Value>) -> BTreeSet<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| serde_json::to_string(value).ok())
        .collect()
}

fn auth_rank(auth: &str) -> u8 {
    match auth {
        "anonymous" => 0,
        "optional" => 1,
        "required" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiSurfaceV3, ArtifactFileV1, EventsContractV1, HealthSource, RuntimeSource};
    use sha2::{Digest, Sha256};

    fn contract(version: &str, api_version: &str, path: &str) -> ServiceContractV3 {
        ServiceContractV3 {
            schema_version: crate::SERVICE_CONTRACT_SCHEMA_VERSION.to_string(),
            compiler_version: crate::COMPILER_VERSION.to_string(),
            service_id: "contest".to_string(),
            service_version: semver::Version::parse(version).unwrap(),
            display_name: "Contest".to_string(),
            source_digest: format!("sha256:{}", "0".repeat(64)),
            runtime: RuntimeSource {
                profile: "standard-container-v1".to_string(),
                artifact: "runtime".to_string(),
                http_port: 8080,
                health: HealthSource {
                    path: "/healthz".to_string(),
                },
                volumes: Vec::new(),
            },
            api_surfaces: vec![ApiSurfaceV3 {
                api_id: "contest.api".to_string(),
                version: semver::Version::parse(api_version).unwrap(),
                document: "openapi.yaml".to_string(),
                document_digest: format!("sha256:{}", "1".repeat(64)),
            }],
            operations: vec![ApiOperationV3 {
                api_id: "contest.api".to_string(),
                api_version: semver::Version::parse(api_version).unwrap(),
                operation_id: "listContests".to_string(),
                provider_path: path.to_string(),
                method: "GET".to_string(),
                audience: "user".to_string(),
                auth: "required".to_string(),
                permission: Some("contest.read".to_string()),
                permission_scope: Some(crate::PermissionScopeV1::system()),
                parameters: vec![],
                request_body: None,
                responses: vec![],
            }],
            api_requirements: vec![],
            package_requirements: vec![],
            resource_claims: vec![],
            migrations: vec![],
            events: EventsContractV1::default(),
            permissions: vec![],
            permission_references: vec![],
            exposures: vec![],
            routes: vec![],
            frontends: vec![],
            config_schema: None,
        }
    }

    #[test]
    fn path_change_requires_api_major_bump() {
        let old = contract("1.0.0", "1.0.0", "/contests");
        let next_minor = contract("1.1.0", "1.1.0", "/competitions");
        assert!(!compare(&old, &next_minor).unwrap().compatible);
        let next_major = contract("2.0.0", "2.0.0", "/competitions");
        assert!(compare(&old, &next_major).unwrap().compatible);
    }

    #[test]
    fn event_schema_is_immutable_within_a_major() {
        let mut old = contract("1.0.0", "1.0.0", "/contests");
        old.events.publishes.push(event(1, 'a'));

        let mut changed = contract("1.1.0", "1.1.0", "/contests");
        changed.events.publishes.push(event(1, 'b'));
        let report = compare(&old, &changed).unwrap();
        assert!(!report.compatible);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.kind == "published-event-schema-changed")
        );

        let mut next_major = contract("2.0.0", "1.1.0", "/contests");
        next_major.events.publishes.push(event(2, 'b'));
        assert!(compare(&old, &next_major).unwrap().compatible);
    }

    #[test]
    fn external_mount_change_requires_api_major_bump() {
        let mut old = contract("1.0.0", "1.0.0", "/contests");
        old.routes.push(route("/api/contests"));
        let mut next_minor = contract("1.1.0", "1.1.0", "/contests");
        next_minor.routes.push(route("/v2/contests"));
        assert!(!compare(&old, &next_minor).unwrap().compatible);

        let mut next_major = contract("2.0.0", "2.0.0", "/contests");
        next_major.routes.push(route("/v2/contests"));
        assert!(compare(&old, &next_major).unwrap().compatible);
    }

    #[test]
    fn removing_an_accepted_input_field_is_breaking() {
        let mut old = contract("1.0.0", "1.0.0", "/contests");
        old.operations[0].request_body = Some(RequestBodyContractV3 {
            required: false,
            content: vec![MediaSchemaContractV3 {
                media_type: "application/json".to_string(),
                schema: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"name": {"type": "string"}}
                })),
                schema_digest: None,
            }],
        });
        let mut current = contract("1.1.0", "1.1.0", "/contests");
        current.operations[0].request_body = Some(RequestBodyContractV3 {
            required: false,
            content: vec![MediaSchemaContractV3 {
                media_type: "application/json".to_string(),
                schema: Some(serde_json::json!({"type": "object", "properties": {}})),
                schema_digest: None,
            }],
        });
        let report = compare(&old, &current).unwrap();
        assert!(!report.compatible);
        assert!(report.issues.iter().any(|issue| {
            issue.kind == "input-schema-narrowed"
                && issue.detail.contains("removed accepted input field name")
        }));
    }

    fn event(version: u32, digest_char: char) -> EventContractV1 {
        let payload_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer"},
                "schemaLine": {"const": digest_char.to_string()}
            },
            "required": ["id", "schemaLine"],
            "additionalProperties": false
        });
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                serde_json_canonicalizer::to_vec(&payload_schema)
                    .expect("test event schema canonicalizes")
            )
        );
        EventContractV1 {
            event_type: "io.ojos.contest.updated".to_string(),
            version,
            schema: ArtifactFileV1 {
                path: format!("events/contest-v{version}.schema.json"),
                digest,
            },
            payload_schema,
            delivery: "durable".to_string(),
        }
    }

    fn route(path: &str) -> RouteContributionV1 {
        RouteContributionV1 {
            exposure_id: "public".to_string(),
            audience: "user".to_string(),
            method: "GET".to_string(),
            path: path.to_string(),
            api_id: "contest.api".to_string(),
            operation_id: "listContests".to_string(),
            provider_path: "/contests".to_string(),
            auth: "required".to_string(),
            permission: Some("contest.read".to_string()),
            permission_scope: Some(crate::PermissionScopeV1::system()),
        }
    }

    #[test]
    fn legacy_implicit_system_scope_is_compatible_with_explicit_system_scope() {
        let previous = contract("1.0.0", "1.0.0", "/contests");
        let mut current = contract("1.0.1", "1.0.0", "/contests");
        let mut previous = previous;
        previous.operations[0].permission_scope = None;
        current.operations[0].permission_scope = Some(crate::PermissionScopeV1::system());
        let report = compare(&previous, &current).unwrap();
        assert!(report.compatible, "{:#?}", report.issues);
    }

    #[test]
    fn comparison_rejects_a_tampered_embedded_event_schema() {
        let mut previous = contract("1.0.0", "1.0.0", "/contests");
        previous.events.publishes.push(event(1, 'a'));
        let mut current = contract("1.0.1", "1.0.0", "/contests");
        current.events.publishes.push(event(1, 'a'));
        current.events.publishes[0].payload_schema["properties"]["id"]["type"] =
            serde_json::json!("string");
        assert!(
            compare(&previous, &current)
                .unwrap_err()
                .contains("does not match")
        );
    }
}
