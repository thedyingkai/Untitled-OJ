//! Deterministic SDK generation; language renderers own their output syntax.

mod go;
mod rust;
mod typescript;

use go::{go_exported, render_go_client, render_go_events, render_go_mod, render_gozero_api};
use rust::{
    render_rust_client, render_rust_events, render_rust_lib, render_rust_manifest, rust_const,
};
use typescript::{
    render_ts_client, render_ts_config, render_ts_events, render_ts_package, ts_identifier, ts_type,
};

use crate::{ApiOperationV3, EventContractV1, ServiceContractV3, contract_bytes};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub const CODEGEN_REPORT_SCHEMA_VERSION: &str = "ojos.dev/codegen-report/v1";
pub const CODEGEN_REPORT_FILE: &str = ".ojos-codegen.json";

#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("generated identifier collision for {language}: {identifier}")]
    IdentifierCollision {
        language: &'static str,
        identifier: String,
    },
    #[error("generated path is not a safe relative path: {0}")]
    UnsafePath(PathBuf),
    #[error("read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("generated output is not sealed: {0}")]
    Drift(String),
    #[error("serialize code generation report: {0}")]
    Serialize(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, CodegenError>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationReport {
    pub schema_version: String,
    pub service_id: String,
    pub service_version: String,
    pub files: Vec<GeneratedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedFile {
    pub path: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerificationReport {
    pub schema_version: String,
    pub service_id: String,
    pub service_version: String,
    pub files: Vec<GeneratedFile>,
}

/// Renders all compiler-owned files without touching the filesystem.
///
/// Paths are relative to the service's `gen/` directory. A `BTreeMap` and
/// sorted contract inputs make the result byte-for-byte deterministic.
pub fn render(contract: &ServiceContractV3) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    validate_event_payload_schemas(contract)?;
    validate_identifiers(contract)?;
    let mut files = BTreeMap::new();

    files.insert(
        PathBuf::from("service.contract.json"),
        contract_bytes(contract).map_err(|error| CodegenError::Drift(error.to_string()))?,
    );

    files.insert(
        PathBuf::from("go/go.mod"),
        render_go_mod(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("go/client.go"),
        render_go_client(contract).into_bytes(),
    );

    files.insert(
        PathBuf::from("go/events.go"),
        render_go_events(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("rust/Cargo.toml"),
        render_rust_manifest(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/lib.rs"),
        render_rust_lib(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/client.rs"),
        render_rust_client(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("rust/src/events.rs"),
        render_rust_events(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("ts/package.json"),
        render_ts_package(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/tsconfig.json"),
        render_ts_config().into_bytes(),
    );
    files.insert(
        PathBuf::from("ts/src/index.ts"),
        b"export * from './client.js';\nexport * from './events.js';\n".to_vec(),
    );
    files.insert(
        PathBuf::from("ts/src/client.ts"),
        render_ts_client(contract).into_bytes(),
    );

    files.insert(
        PathBuf::from("ts/src/events.ts"),
        render_ts_events(contract)?.into_bytes(),
    );

    files.insert(
        PathBuf::from("gozero/service.api"),
        render_gozero_api(contract).into_bytes(),
    );
    files.insert(
        PathBuf::from("gozero/server-adapter.json"),
        render_server_adapter(contract)?,
    );
    Ok(files)
}

/// Writes generated files below `output_root` and returns the stable report.
///
/// Files listed by the previous report but no longer generated are removed;
/// unrelated files are never deleted.
pub fn generate_to(contract: &ServiceContractV3, output_root: &Path) -> Result<GenerationReport> {
    let rendered = render(contract)?;
    fs::create_dir_all(output_root).map_err(|source| CodegenError::Write {
        path: output_root.to_path_buf(),
        source,
    })?;

    let report_path = output_root.join(CODEGEN_REPORT_FILE);
    remove_stale_files(output_root, &report_path, rendered.keys())?;

    for (relative, bytes) in &rendered {
        ensure_safe_relative(relative)?;
        let destination = output_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| CodegenError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(&destination, bytes).map_err(|source| CodegenError::Write {
            path: destination,
            source,
        })?;
    }

    let report = report_for(contract, &rendered);
    let mut report_bytes = serde_json::to_vec_pretty(&report)?;
    report_bytes.push(b'\n');
    fs::write(&report_path, report_bytes).map_err(|source| CodegenError::Write {
        path: report_path,
        source,
    })?;
    Ok(report)
}

/// Verifies compiler-owned output without mutating the filesystem.
///
/// CI uses this after compilation so a missing file, a hand edit, a stale
/// compiler-owned file, or a forged generation report fails before build or
/// publication. Unrelated files below `gen/` remain developer-owned.
pub fn verify_generated(
    contract: &ServiceContractV3,
    output_root: &Path,
) -> Result<VerificationReport> {
    let rendered = render(contract)?;
    let expected = report_for(contract, &rendered);
    let report_path = output_root.join(CODEGEN_REPORT_FILE);
    let report_bytes = fs::read(&report_path).map_err(|source| CodegenError::Read {
        path: report_path.clone(),
        source,
    })?;
    let recorded: GenerationReport = serde_json::from_slice(&report_bytes)?;
    if recorded != expected {
        return Err(CodegenError::Drift(format!(
            "{} does not match compiler output",
            report_path.display()
        )));
    }

    for (relative, expected_bytes) in &rendered {
        ensure_safe_relative(relative)?;
        let destination = output_root.join(relative);
        let actual = fs::read(&destination).map_err(|source| CodegenError::Read {
            path: destination.clone(),
            source,
        })?;
        if &actual != expected_bytes {
            return Err(CodegenError::Drift(format!(
                "{} differs from deterministic output",
                destination.display()
            )));
        }
    }

    Ok(VerificationReport {
        schema_version: "ojos.dev/codegen-verification/v1".to_string(),
        service_id: expected.service_id,
        service_version: expected.service_version,
        files: expected.files,
    })
}

pub fn report_for(
    contract: &ServiceContractV3,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> GenerationReport {
    GenerationReport {
        schema_version: CODEGEN_REPORT_SCHEMA_VERSION.to_string(),
        service_id: contract.service_id.clone(),
        service_version: contract.service_version.to_string(),
        files: files
            .iter()
            .map(|(path, bytes)| GeneratedFile {
                path: slash_path(path),
                digest: digest(bytes),
                size: bytes.len() as u64,
            })
            .collect(),
    }
}

fn remove_stale_files<'a>(
    output_root: &Path,
    report_path: &Path,
    current: impl Iterator<Item = &'a PathBuf>,
) -> Result<()> {
    let current = current
        .map(|path| slash_path(path))
        .collect::<BTreeSet<_>>();
    let bytes = match fs::read(report_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(CodegenError::Read {
                path: report_path.to_path_buf(),
                source,
            });
        }
    };
    let previous: GenerationReport = serde_json::from_slice(&bytes)?;
    for file in previous.files {
        if current.contains(&file.path) {
            continue;
        }
        let relative = PathBuf::from(&file.path);
        ensure_safe_relative(&relative)?;
        let destination = output_root.join(relative);
        match fs::remove_file(&destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CodegenError::Write {
                    path: destination,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn ensure_safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(CodegenError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn validate_identifiers(contract: &ServiceContractV3) -> Result<()> {
    let mut go = BTreeSet::new();
    let mut rust = BTreeSet::new();
    let mut ts = BTreeSet::new();
    for operation in &contract.operations {
        insert_identifier(&mut go, go_exported(&operation.operation_id), "Go")?;
        insert_identifier(&mut rust, rust_const(&operation.operation_id), "Rust")?;
        insert_identifier(
            &mut ts,
            ts_identifier(&operation.operation_id),
            "TypeScript",
        )?;
    }
    for event in all_events(contract) {
        let identity = format!("{}V{}", event.event_type, event.version);
        insert_identifier(&mut go, go_exported(&identity), "Go event")?;
        insert_identifier(&mut rust, rust_const(&identity), "Rust event")?;
        insert_identifier(&mut ts, ts_type(&identity), "TypeScript event")?;
    }
    Ok(())
}

fn validate_event_payload_schemas(contract: &ServiceContractV3) -> Result<()> {
    for event in all_events(contract) {
        let canonical = serde_json_canonicalizer::to_vec(&event.payload_schema)
            .map_err(CodegenError::Serialize)?;
        if digest(&canonical) != event.schema.digest {
            return Err(CodegenError::Drift(format!(
                "event {} v{} payload schema does not match {}",
                event.event_type, event.version, event.schema.digest
            )));
        }
    }
    Ok(())
}

fn insert_identifier(
    seen: &mut BTreeSet<String>,
    identifier: String,
    language: &'static str,
) -> Result<()> {
    if !seen.insert(identifier.clone()) {
        return Err(CodegenError::IdentifierCollision {
            language,
            identifier,
        });
    }
    Ok(())
}

fn schema_object(schema: &Value) -> Result<&Map<String, Value>> {
    schema.as_object().ok_or_else(|| {
        CodegenError::Drift("event payload schema must be a JSON object".to_string())
    })
}

fn schema_properties(schema: &Value) -> Result<(&Map<String, Value>, BTreeSet<&str>)> {
    let object = schema_object(schema)?;
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| CodegenError::Drift("event object schema needs properties".to_string()))?;
    let required = object
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    Ok((properties, required))
}

fn schema_kind(schema: &Value) -> Result<&str> {
    let object = schema_object(schema)?;
    if object.contains_key("enum") || object.contains_key("const") {
        Ok("string")
    } else {
        object.get("type").and_then(Value::as_str).ok_or_else(|| {
            CodegenError::Drift("event property must declare type, enum, or const".to_string())
        })
    }
}

fn schema_string_values(schema: &Value) -> Result<Vec<&str>> {
    let object = schema_object(schema)?;
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    CodegenError::Drift("event enum values must be strings".to_string())
                })
            })
            .collect();
    }
    if let Some(value) = object.get("const").and_then(Value::as_str) {
        return Ok(vec![value]);
    }
    Err(CodegenError::Drift(
        "event schema does not declare string enum or const".to_string(),
    ))
}

fn render_server_adapter(contract: &ServiceContractV3) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Adapter<'a> {
        schema_version: &'static str,
        service_id: &'a str,
        operations: Vec<AdapterOperation<'a>>,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct AdapterOperation<'a> {
        operation_id: &'a str,
        handler: String,
        method: &'a str,
        path: &'a str,
        audience: &'a str,
        auth: &'a str,
        permission: Option<&'a str>,
        request_schema_digests: Vec<&'a str>,
        response_schema_digests: Vec<&'a str>,
    }
    let operations = sorted_operations(contract)
        .into_iter()
        .map(|operation| AdapterOperation {
            operation_id: &operation.operation_id,
            handler: go_exported(&operation.operation_id),
            method: &operation.method,
            path: &operation.provider_path,
            audience: &operation.audience,
            auth: &operation.auth,
            permission: operation.permission.as_deref(),
            request_schema_digests: operation
                .request_body
                .iter()
                .flat_map(|body| &body.content)
                .filter_map(|content| content.schema_digest.as_deref())
                .collect(),
            response_schema_digests: operation
                .responses
                .iter()
                .flat_map(|response| &response.content)
                .filter_map(|content| content.schema_digest.as_deref())
                .collect(),
        })
        .collect();
    let mut bytes = serde_json::to_vec_pretty(&Adapter {
        schema_version: "ojos.dev/gozero-server-adapter/v1",
        service_id: &contract.service_id,
        operations,
    })?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn sorted_operations(contract: &ServiceContractV3) -> Vec<&ApiOperationV3> {
    let mut operations = contract.operations.iter().collect::<Vec<_>>();
    operations.sort_by(|left, right| {
        left.api_id
            .cmp(&right.api_id)
            .then(left.provider_path.cmp(&right.provider_path))
            .then(left.method.cmp(&right.method))
            .then(left.operation_id.cmp(&right.operation_id))
    });
    operations
}

fn all_events(contract: &ServiceContractV3) -> Vec<&EventContractV1> {
    let mut by_identity = BTreeMap::new();
    for event in contract
        .events
        .publishes
        .iter()
        .chain(contract.events.subscribes.iter())
    {
        by_identity.insert((event.event_type.as_str(), event.version), event);
    }
    by_identity.into_values().collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn words(value: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            current.push(character);
        } else if !current.is_empty() {
            output.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        output.push(current);
    }
    if output.is_empty() {
        output.push("generated".to_string());
    }
    output
}
