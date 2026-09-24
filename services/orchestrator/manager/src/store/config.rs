//! Deterministic validation of signed release configuration and opaque secret references.
use super::StoreRuleError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Scalar values are safe to materialize; secret references never enter this map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReleaseConfig {
    pub values: BTreeMap<String, String>,
    pub secret_paths: BTreeSet<String>,
    pub schema_controls_requiredness: bool,
}

pub fn validate_release_config(
    schema: &Value,
    requested: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<ValidatedReleaseConfig, StoreRuleError> {
    if schema.get("$schema").is_some() {
        let (config, secrets) =
            validate_json_schema_config(schema, requested, requested_secret_refs)?;
        return Ok(ValidatedReleaseConfig {
            values: config,
            secret_paths: secrets,
            schema_controls_requiredness: true,
        });
    }
    let requested = match requested {
        Value::Null => serde_json::Map::new(),
        Value::Object(values) => values.clone(),
        _ => {
            return Err(StoreRuleError::invalid(
                "STORE_CONFIG_INVALID",
                "config must be a JSON object",
            ));
        }
    };
    let Some(schema) = schema.as_object() else {
        if schema.is_null() {
            if requested.is_empty() {
                return Ok(ValidatedReleaseConfig {
                    values: BTreeMap::new(),
                    secret_paths: BTreeSet::new(),
                    schema_controls_requiredness: false,
                });
            }
            return Err(StoreRuleError::invalid(
                "STORE_CONFIG_UNKNOWN",
                "release declares no configurable fields",
            ));
        }
        return Err(StoreRuleError::invalid(
            "STORE_CONFIG_SCHEMA_INVALID",
            "signed config_schema must be an object",
        ));
    };
    let (properties, required, allow_extra) = if schema.contains_key("properties") {
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                StoreRuleError::invalid(
                    "STORE_CONFIG_SCHEMA_INVALID",
                    "config_schema.properties must be an object",
                )
            })?;
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let allow_extra = schema
            .get("additionalProperties")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        (properties, required, allow_extra)
    } else {
        (schema, BTreeSet::new(), false)
    };
    let unknown = requested
        .keys()
        .filter(|key| !properties.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !allow_extra && !unknown.is_empty() {
        return Err(StoreRuleError::invalid(
            "STORE_CONFIG_UNKNOWN",
            format!(
                "config contains undeclared field(s): {}",
                unknown.join(", ")
            ),
        ));
    }
    let mut output = BTreeMap::new();
    let mut secrets = BTreeSet::new();
    for (name, declaration) in properties {
        let declaration = declaration.as_object().ok_or_else(|| {
            StoreRuleError::invalid(
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("config declaration {name} must be an object"),
            )
        })?;
        let kind = declaration
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("string");
        if kind == "secret" {
            secrets.insert(name.clone());
            if requested.contains_key(name) {
                return Err(StoreRuleError::invalid(
                    "STORE_SECRET_VALUE_FORBIDDEN",
                    format!("config field {name} is secret; submit only secret_refs.{name}"),
                ));
            }
            continue;
        }
        let value = requested
            .get(name)
            .cloned()
            .or_else(|| declaration.get("default").cloned());
        let required =
            required.contains(name) || declaration.get("required") == Some(&Value::Bool(true));
        let Some(value) = value else {
            if required {
                return Err(StoreRuleError::invalid(
                    "STORE_CONFIG_REQUIRED",
                    format!("config field {name} is required"),
                ));
            }
            continue;
        };
        validate_config_value(name, kind, declaration, &value)?;
        output.insert(name.clone(), scalar_config_value(name, &value)?);
    }
    if allow_extra {
        for (name, value) in requested {
            if !properties.contains_key(&name) {
                output.insert(name.clone(), scalar_config_value(&name, &value)?);
            }
        }
    }
    Ok(ValidatedReleaseConfig {
        values: output,
        secret_paths: secrets,
        schema_controls_requiredness: false,
    })
}

fn validate_json_schema_config(
    schema: &Value,
    requested: &Value,
    requested_secret_refs: &BTreeMap<String, String>,
) -> Result<(BTreeMap<String, String>, BTreeSet<String>), StoreRuleError> {
    let requested = match requested {
        Value::Null => serde_json::Map::new(),
        Value::Object(requested) => requested.clone(),
        _ => {
            return Err(StoreRuleError::invalid(
                "STORE_CONFIG_INVALID",
                "config must be a JSON object",
            ));
        }
    };
    reject_unsupported_config_schema_keywords(schema)?;
    let mut secret_paths = BTreeSet::new();
    collect_config_secret_paths(schema, "", &mut secret_paths)?;

    for path in &secret_paths {
        if json_path(&requested, path).is_some() {
            return Err(StoreRuleError::invalid(
                "STORE_SECRET_VALUE_FORBIDDEN",
                format!("config field {path} is secret; submit only secret_refs.{path}"),
            ));
        }
    }

    // Secret references participate in conditional validation as opaque
    // placeholders. The reference itself is never placed in the config map or
    // exposed to schema expressions.
    let unknown_secret_refs = requested_secret_refs
        .keys()
        .filter(|path| !secret_paths.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown_secret_refs.is_empty() {
        return Err(StoreRuleError::invalid(
            "STORE_SECRET_REFS_INVALID",
            format!(
                "secret_refs contains undeclared JSON Schema field(s): {}",
                unknown_secret_refs.join(", ")
            ),
        ));
    }
    let mut instance = Value::Object(requested.clone());
    for path in requested_secret_refs.keys() {
        insert_json_path(&mut instance, path, Value::String("opaque".to_string()))?;
    }
    let mut validation_schema = schema.clone();
    relax_config_secret_value_constraints(&mut validation_schema);
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .should_validate_formats(true)
        .build(&validation_schema)
        .map_err(|error| {
            StoreRuleError::invalid(
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("compile signed JSON Schema 2020-12: {error}"),
            )
        })?;
    let errors = validator
        .iter_errors(&instance)
        .take(8)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        return Err(StoreRuleError::invalid(
            "STORE_CONFIG_INVALID",
            format!(
                "config does not satisfy signed JSON Schema: {}",
                errors.join("; ")
            ),
        ));
    }

    let mut output = BTreeMap::new();
    flatten_config_scalars("", &Value::Object(requested), &mut output)?;
    Ok((output, secret_paths))
}

fn relax_config_secret_value_constraints(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            let secret = object.get("writeOnly").and_then(Value::as_bool) == Some(true)
                && object.get("x-ojos-secret").and_then(Value::as_bool) == Some(true);
            if secret {
                object.retain(|key, _| {
                    matches!(
                        key.as_str(),
                        "type" | "writeOnly" | "x-ojos-secret" | "title" | "description"
                    )
                });
                object.insert("type".to_string(), Value::String("string".to_string()));
                return;
            }
            for child in object.values_mut() {
                relax_config_secret_value_constraints(child);
            }
        }
        Value::Array(values) => {
            for value in values {
                relax_config_secret_value_constraints(value);
            }
        }
        _ => {}
    }
}

fn reject_unsupported_config_schema_keywords(schema: &Value) -> Result<(), StoreRuleError> {
    fn visit(value: &Value) -> Result<(), StoreRuleError> {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    if matches!(
                        key.as_str(),
                        "unevaluatedProperties"
                            | "patternProperties"
                            | "propertyNames"
                            | "contains"
                            | "prefixItems"
                    ) {
                        return Err(StoreRuleError::invalid(
                            "STORE_CONFIG_SCHEMA_INVALID",
                            format!(
                                "JSON Schema keyword {key} is outside the supported configuration subset"
                            ),
                        ));
                    }
                    if key == "$ref"
                        && !child.as_str().is_some_and(|reference| {
                            reference.starts_with("#/") || reference.starts_with("sha256:")
                        })
                    {
                        return Err(StoreRuleError::invalid(
                            "STORE_CONFIG_SCHEMA_INVALID",
                            "JSON Schema $ref must be local or digest-pinned",
                        ));
                    }
                    visit(child)?;
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(schema)
}

pub fn collect_config_secret_paths(
    schema: &Value,
    prefix: &str,
    output: &mut BTreeSet<String>,
) -> Result<(), StoreRuleError> {
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (name, declaration) in properties {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            let secret = declaration.get("writeOnly").and_then(Value::as_bool) == Some(true)
                && declaration.get("x-ojos-secret").and_then(Value::as_bool) == Some(true);
            if secret {
                output.insert(path);
            } else {
                collect_config_secret_paths(declaration, &path, output)?;
            }
        }
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                collect_config_secret_paths(branch, prefix, output)?;
            }
        }
    }
    for keyword in ["if", "then", "else", "not"] {
        if let Some(branch) = schema.get(keyword) {
            collect_config_secret_paths(branch, prefix, output)?;
        }
    }
    Ok(())
}

fn json_path<'a>(root: &'a serde_json::Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut value = root.get(path.split('.').next()?)?;
    for segment in path.split('.').skip(1) {
        value = value.as_object()?.get(segment)?;
    }
    Some(value)
}

fn insert_json_path(root: &mut Value, path: &str, value: Value) -> Result<(), StoreRuleError> {
    let mut segments = path.split('.').peekable();
    let mut current = root;
    while let Some(segment) = segments.next() {
        if segments.peek().is_none() {
            current
                .as_object_mut()
                .ok_or_else(|| {
                    StoreRuleError::invalid(
                        "STORE_CONFIG_INVALID",
                        format!("config parent for secret {path} must be an object"),
                    )
                })?
                .entry(segment.to_string())
                .or_insert(value.clone());
            return Ok(());
        }
        let Some(object) = current.as_object_mut() else {
            return Err(StoreRuleError::invalid(
                "STORE_CONFIG_INVALID",
                format!("config parent for secret {path} must be an object"),
            ));
        };
        current = object
            .entry(segment.to_string())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
    }
    Ok(())
}

fn flatten_config_scalars(
    prefix: &str,
    value: &Value,
    output: &mut BTreeMap<String, String>,
) -> Result<(), StoreRuleError> {
    match value {
        Value::Object(object) => {
            for (name, value) in object {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}.{name}")
                };
                flatten_config_scalars(&path, value, output)?;
            }
            Ok(())
        }
        Value::String(_) | Value::Bool(_) | Value::Number(_) => {
            output.insert(prefix.to_string(), scalar_config_value(prefix, value)?);
            Ok(())
        }
        Value::Null => Ok(()),
        _ => Err(StoreRuleError::invalid(
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {prefix} must be a scalar or nested object"),
        )),
    }
}

fn validate_config_value(
    name: &str,
    kind: &str,
    declaration: &serde_json::Map<String, Value>,
    value: &Value,
) -> Result<(), StoreRuleError> {
    let valid = match kind {
        "string" => value.is_string(),
        "boolean" | "bool" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "enum" => declaration
            .get("values")
            .or_else(|| declaration.get("enum"))
            .and_then(Value::as_array)
            .is_some_and(|values| values.contains(value)),
        other => {
            return Err(StoreRuleError::invalid(
                "STORE_CONFIG_SCHEMA_INVALID",
                format!("config field {name} has unsupported type {other}"),
            ));
        }
    };
    if valid {
        Ok(())
    } else {
        Err(StoreRuleError::invalid(
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {name} does not satisfy type {kind}"),
        ))
    }
}

fn scalar_config_value(name: &str, value: &Value) -> Result<String, StoreRuleError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(StoreRuleError::invalid(
            "STORE_CONFIG_TYPE_INVALID",
            format!("config field {name} must be a scalar"),
        )),
    }
}
