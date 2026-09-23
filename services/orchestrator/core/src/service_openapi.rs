use crate::ServiceReleaseContract;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// One checked, published OpenAPI operation and the Service Contract API
/// surface it belongs to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceOpenApiOperation {
    pub operation_id: String,
    pub api_id: String,
    pub api_version: String,
    pub router_path: String,
    pub published_path: String,
    pub method: String,
    pub auth_mode: String,
    pub permission: String,
}

/// Deterministic result returned by the reusable Service Contract/OpenAPI lint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceOpenApiLintReport {
    pub service_id: String,
    pub release_version: String,
    pub server_path_prefix: String,
    pub operations: Vec<ServiceOpenApiOperation>,
}

#[derive(Debug, Error)]
pub enum ServiceOpenApiLintError {
    #[error("service OpenAPI YAML is invalid: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("service OpenAPI document is invalid: {0}")]
    InvalidDocument(String),
    #[error("service OpenAPI declares service {actual}, expected {expected}")]
    ServiceMismatch { expected: String, actual: String },
    #[error("service OpenAPI declares release version {actual}, expected {expected}")]
    ReleaseVersionMismatch { expected: String, actual: String },
    #[error("OpenAPI operation {operation_id} maps undeclared API {api_id}")]
    UndeclaredApi {
        operation_id: String,
        api_id: String,
    },
    #[error("OpenAPI operation {operation_id} has no x-ojos-api mapping")]
    UnmappedOperation { operation_id: String },
    #[error("duplicate OpenAPI operationId {operation_id}")]
    DuplicateOperationId { operation_id: String },
    #[error("duplicate x-ojos API mapping for {api_id} {method} {path}")]
    DuplicateApiMapping {
        api_id: String,
        method: String,
        path: String,
    },
    #[error("OpenAPI operation {operation_id} API version {actual} does not match {expected}")]
    ApiVersionMismatch {
        operation_id: String,
        expected: String,
        actual: String,
    },
    #[error("OpenAPI operation {operation_id} path {actual} is outside declared prefix {expected}")]
    PathPrefixMismatch {
        operation_id: String,
        expected: String,
        actual: String,
    },
    #[error("OpenAPI operation {operation_id} method {method} is not declared for {api_id}")]
    MethodMismatch {
        operation_id: String,
        api_id: String,
        method: String,
    },
    #[error("OpenAPI operation {operation_id} auth mode {actual} does not match {expected}")]
    AuthModeMismatch {
        operation_id: String,
        expected: String,
        actual: String,
    },
    #[error("OpenAPI operation {operation_id} permission {actual} does not match {expected}")]
    PermissionMismatch {
        operation_id: String,
        expected: String,
        actual: String,
    },
    #[error("declared API {api_id} has no OpenAPI operation")]
    MissingApi { api_id: String },
    #[error("declared API {api_id} method {method} has no OpenAPI operation")]
    MissingApiMethod { api_id: String, method: String },
}

/// Parse a checked-in OpenAPI YAML document and lint it against a normalized
/// Service Contract v2 release.
pub fn lint_service_openapi_yaml(
    contract: &ServiceReleaseContract,
    document: &str,
) -> Result<ServiceOpenApiLintReport, ServiceOpenApiLintError> {
    let document: Value = serde_yaml::from_str(document)?;
    lint_service_openapi_value(contract, &document)
}

/// Lint a parsed OpenAPI document. Every HTTP operation in the document is
/// considered published and therefore must carry exactly one `x-ojos-api`
/// mapping to a declared provider surface.
pub fn lint_service_openapi_value(
    contract: &ServiceReleaseContract,
    document: &Value,
) -> Result<ServiceOpenApiLintReport, ServiceOpenApiLintError> {
    let root = document
        .as_object()
        .ok_or_else(|| invalid("root must be an object"))?;
    let openapi = text(root.get("openapi"), "openapi")?;
    if !openapi.starts_with("3.0.") && !openapi.starts_with("3.1.") {
        return Err(invalid("openapi must declare a supported 3.0/3.1 version"));
    }

    let identity = root
        .get("x-ojos-service")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("x-ojos-service must be an object"))?;
    let service_id = text(identity.get("id"), "x-ojos-service.id")?;
    if service_id != contract.release.service_name {
        return Err(ServiceOpenApiLintError::ServiceMismatch {
            expected: contract.release.service_name.clone(),
            actual: service_id.to_string(),
        });
    }
    let identity_version = text(identity.get("version"), "x-ojos-service.version")?;
    if identity_version != contract.release.version {
        return Err(ServiceOpenApiLintError::ReleaseVersionMismatch {
            expected: contract.release.version.clone(),
            actual: identity_version.to_string(),
        });
    }
    let info_version = root
        .get("info")
        .and_then(Value::as_object)
        .and_then(|info| info.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("info.version must be a string"))?;
    if info_version != contract.release.version {
        return Err(ServiceOpenApiLintError::ReleaseVersionMismatch {
            expected: contract.release.version.clone(),
            actual: info_version.to_string(),
        });
    }

    let server_path_prefix = server_path_prefix(root.get("servers"))?;
    let paths = root
        .get("paths")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("paths must be an object"))?;

    let mut declarations = BTreeMap::new();
    let mut declared_methods = BTreeMap::new();
    for api in &contract.release.apis {
        if declarations.insert(api.api_id.as_str(), api).is_some() {
            return Err(invalid(format!(
                "Service Contract repeats provided API {}",
                api.api_id
            )));
        }
        declared_methods.insert(
            api.api_id.as_str(),
            api.methods
                .iter()
                .map(|method| method.trim().to_ascii_uppercase())
                .collect::<BTreeSet<_>>(),
        );
    }

    let mut operation_ids = BTreeSet::new();
    let mut mapping_keys = BTreeSet::new();
    let mut covered_methods: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut operations = Vec::new();
    for (router_path, path_item) in paths {
        let router_path = normalize_path(router_path, "OpenAPI router path")?;
        let path_item = path_item
            .as_object()
            .ok_or_else(|| invalid(format!("path item {router_path} must be an object")))?;
        for method_key in HTTP_METHODS {
            let Some(operation) = path_item.get(*method_key) else {
                continue;
            };
            let operation = operation.as_object().ok_or_else(|| {
                invalid(format!(
                    "operation {method_key} {router_path} must be an object"
                ))
            })?;
            let fallback_id = format!("{} {}", method_key.to_ascii_uppercase(), router_path);
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| invalid(format!("operation {fallback_id} requires operationId")))?
                .trim()
                .to_string();
            if !operation_ids.insert(operation_id.clone()) {
                return Err(ServiceOpenApiLintError::DuplicateOperationId { operation_id });
            }
            let mapping = operation
                .get("x-ojos-api")
                .and_then(Value::as_object)
                .ok_or_else(|| ServiceOpenApiLintError::UnmappedOperation {
                    operation_id: operation_id.clone(),
                })?;
            let api_id = text(mapping.get("id"), "x-ojos-api.id")?;
            let declaration =
                declarations
                    .get(api_id)
                    .ok_or_else(|| ServiceOpenApiLintError::UndeclaredApi {
                        operation_id: operation_id.clone(),
                        api_id: api_id.to_string(),
                    })?;
            let api_version = text(mapping.get("version"), "x-ojos-api.version")?;
            if api_version != declaration.version {
                return Err(ServiceOpenApiLintError::ApiVersionMismatch {
                    operation_id,
                    expected: declaration.version.clone(),
                    actual: api_version.to_string(),
                });
            }
            let auth_mode = text(mapping.get("auth"), "x-ojos-api.auth")?;
            if !auth_mode.eq_ignore_ascii_case(&declaration.auth_mode) {
                return Err(ServiceOpenApiLintError::AuthModeMismatch {
                    operation_id,
                    expected: declaration.auth_mode.clone(),
                    actual: auth_mode.to_string(),
                });
            }
            let permission = text(mapping.get("permission"), "x-ojos-api.permission")?;
            if permission != declaration.permission {
                return Err(ServiceOpenApiLintError::PermissionMismatch {
                    operation_id,
                    expected: declaration.permission.clone(),
                    actual: permission.to_string(),
                });
            }

            let published_path = join_paths(&server_path_prefix, &router_path)?;
            let declared_prefix = normalize_path(
                &declaration.path_prefix,
                &format!("Service Contract path for {api_id}"),
            )?;
            if !is_same_or_descendant(&published_path, &declared_prefix) {
                return Err(ServiceOpenApiLintError::PathPrefixMismatch {
                    operation_id,
                    expected: declared_prefix,
                    actual: published_path,
                });
            }
            let method = method_key.to_ascii_uppercase();
            if !declared_methods
                .get(api_id)
                .is_some_and(|methods| methods.contains(&method))
            {
                return Err(ServiceOpenApiLintError::MethodMismatch {
                    operation_id,
                    api_id: api_id.to_string(),
                    method,
                });
            }
            let mapping_key = (api_id.to_string(), method.clone(), published_path.clone());
            if !mapping_keys.insert(mapping_key) {
                return Err(ServiceOpenApiLintError::DuplicateApiMapping {
                    api_id: api_id.to_string(),
                    method,
                    path: published_path,
                });
            }
            covered_methods
                .entry(api_id)
                .or_default()
                .insert(method.clone());
            operations.push(ServiceOpenApiOperation {
                operation_id,
                api_id: api_id.to_string(),
                api_version: api_version.to_string(),
                router_path: router_path.clone(),
                published_path,
                method,
                auth_mode: auth_mode.to_ascii_lowercase(),
                permission: permission.to_string(),
            });
        }
    }

    for (api_id, methods) in declared_methods {
        let Some(covered) = covered_methods.get(api_id) else {
            return Err(ServiceOpenApiLintError::MissingApi {
                api_id: api_id.to_string(),
            });
        };
        for method in methods {
            if !covered.contains(&method) {
                return Err(ServiceOpenApiLintError::MissingApiMethod {
                    api_id: api_id.to_string(),
                    method,
                });
            }
        }
    }
    operations.sort_by(|left, right| {
        left.published_path
            .cmp(&right.published_path)
            .then_with(|| left.method.cmp(&right.method))
            .then_with(|| left.operation_id.cmp(&right.operation_id))
    });
    Ok(ServiceOpenApiLintReport {
        service_id: service_id.to_string(),
        release_version: identity_version.to_string(),
        server_path_prefix,
        operations,
    })
}

fn invalid(message: impl Into<String>) -> ServiceOpenApiLintError {
    ServiceOpenApiLintError::InvalidDocument(message.into())
}

fn text<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a str, ServiceOpenApiLintError> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .ok_or_else(|| invalid(format!("{field} must be a non-empty string")))
}

fn server_path_prefix(value: Option<&Value>) -> Result<String, ServiceOpenApiLintError> {
    let Some(value) = value else {
        return Ok(String::new());
    };
    let servers = value
        .as_array()
        .ok_or_else(|| invalid("servers must be an array"))?;
    if servers.len() > 1 {
        return Err(invalid(
            "service OpenAPI may declare at most one unambiguous server path prefix",
        ));
    }
    let Some(server) = servers.first() else {
        return Ok(String::new());
    };
    let url = server
        .as_object()
        .and_then(|server| server.get("url"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("servers[0].url must be a string"))?;
    if url == "/" || url.is_empty() {
        return Ok(String::new());
    }
    normalize_path(url, "servers[0].url")
}

fn normalize_path(value: &str, field: &str) -> Result<String, ServiceOpenApiLintError> {
    let value = value.trim();
    if !value.starts_with('/') || value.contains('?') || value.contains('#') || value.contains("//")
    {
        return Err(invalid(format!(
            "{field} must be an absolute path without query, fragment, or empty segment"
        )));
    }
    if value.len() > 1 {
        Ok(value.trim_end_matches('/').to_string())
    } else {
        Ok(value.to_string())
    }
}

fn join_paths(prefix: &str, path: &str) -> Result<String, ServiceOpenApiLintError> {
    if prefix.is_empty() || prefix == "/" {
        return Ok(path.to_string());
    }
    normalize_path(&format!("{prefix}{path}"), "published OpenAPI path")
}

fn is_same_or_descendant(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
