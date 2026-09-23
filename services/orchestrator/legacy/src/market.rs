//! 外部 Service 市场导入：把 GitHub Release / URL / 本地包里的 release.yaml
//! 转换为可注册进 Orchestrator store 的 ServiceManifest + ServiceRelease。
//!
//! 这是「插件商店」的核心能力：编排器本体不内置任何业务模块，
//! 模块以 release 包（zip/tar/release.yaml）形式从外部源导入。

use crate::service_io::legacy_release_manifest_from_contract;
use crate::{
    EndpointDecl, OrchestratorError, OrchestratorStore, Result, RuntimeMode, ServiceHealthDecl,
    ServiceManifest, ServiceRelease, ServiceReleaseContract, ServiceReleaseManifest,
    ServiceRequires, ServiceRuntimeDecl, ServiceUiDecl, SourceDecl, validate_service_manifest,
    validate_service_release,
};
use serde::{Deserialize, Serialize};

/// 一次外部 release 导入的结果：合成的 Service 契约 + 注册的 Release 契约。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalReleaseImport {
    pub service: ServiceManifest,
    pub release: ServiceReleaseManifest,
    pub source_url: String,
    pub checksum: String,
    /// store 中已存在同名 Service（本次导入是覆盖更新）。
    pub replaced_existing: bool,
    /// Exact versioned contract persisted in `ServiceRelease.manifest`.
    ///
    /// The public compatibility response continues to expose `release` as the
    /// normalized v1 projection, while registration must retain v2-only API
    /// requirements, events and runtime-contract declarations.
    #[serde(skip)]
    stored_manifest: serde_json::Value,
}

/// 根据下载地址推断 release source.kind。
pub fn release_source_kind_for_url(source_url: &str) -> String {
    let url = source_url.trim();
    let lowered = url.to_ascii_lowercase();
    if lowered.starts_with("http://") || lowered.starts_with("https://") {
        if lowered.contains("github.com") && lowered.contains("/releases/download/") {
            "github-release".to_string()
        } else {
            "url".to_string()
        }
    } else {
        "local".to_string()
    }
}

/// 从 release.yaml 文本构建导入对象：解析、改写 source（指向真实下载地址与校验和）、
/// 校验 release 契约，并合成一份满足全部交叉校验约束的 ServiceManifest。
pub fn external_release_import_from_yaml(
    release_yaml: &str,
    source_url: &str,
    checksum: &str,
) -> Result<ExternalReleaseImport> {
    let mut contract = ServiceReleaseContract::from_yaml_str(release_yaml).map_err(|err| {
        OrchestratorError::InvalidManifest(format!("imported release.yaml is invalid: {err}"))
    })?;
    let mut release = legacy_release_manifest_from_contract(contract.clone()).map_err(|err| {
        OrchestratorError::InvalidManifest(format!("imported release.yaml is invalid: {err}"))
    })?;
    release.source.kind = release_source_kind_for_url(source_url);
    release.source.url = source_url.trim().to_string();
    release.source.checksum = checksum.trim().to_string();
    validate_service_release(&release)?;
    let service = service_manifest_from_release(&release, source_url)?;
    validate_service_manifest(&service)?;
    // Keep the projected legacy fields in the versioned document as well so
    // old runtime code sees the same API/event resources after reparsing.
    contract.release = release.clone();
    let stored_manifest = contract.to_json_value()?;
    Ok(ExternalReleaseImport {
        service,
        release,
        source_url: source_url.trim().to_string(),
        checksum: checksum.trim().to_string(),
        replaced_existing: false,
        stored_manifest,
    })
}

/// 由 release 契约合成 Service 契约。
///
/// release.install 的交叉校验要求（service.rs）：
/// - `release.service_name == manifest.id`
/// - `release.version == manifest.version`
/// - `release.service_type == manifest.kind`
/// - `release.backend.protocol == manifest.endpoint.protocol`
/// - `release.backend.port == manifest.endpoint.default_port`
/// - `release.backend.health_path == manifest.endpoint.health_path`
pub fn service_manifest_from_release(
    release: &ServiceReleaseManifest,
    source_url: &str,
) -> Result<ServiceManifest> {
    let runtime_kind = release.runtime.kind.trim().to_ascii_lowercase();
    let mode = match runtime_kind.as_str() {
        "image" => RuntimeMode::Container,
        "external" => RuntimeMode::External,
        _ => RuntimeMode::LocalProcess,
    };
    let description = if release.description.trim().is_empty() {
        format!("Imported service {}", release.service_name)
    } else {
        release.description.clone()
    };
    Ok(ServiceManifest {
        schema_version: 1,
        id: release.service_name.clone(),
        name: release.service_name.clone(),
        version: release.version.clone(),
        kind: release.service_type.clone(),
        description,
        endpoint: EndpointDecl {
            protocol: release.backend.protocol.clone(),
            default_port: release.backend.port,
            health_path: release.backend.health_path.clone(),
            expose: false,
            routes: Vec::new(),
        },
        runtime: ServiceRuntimeDecl {
            mode,
            driver: runtime_kind,
            root_allowed: false,
            non_root_allowed: true,
            start_policy: String::new(),
            restart_policy: String::new(),
        },
        config_schema: release.config_schema.clone(),
        requires: ServiceRequires {
            services: release.dependencies.clone(),
            secrets: release.secrets.clone(),
            ..ServiceRequires::default()
        },
        // Runtime capabilities are release/version scoped. Projecting them to
        // this service-global record would let a later import overwrite the
        // capability of an already-running different version.
        provides: Default::default(),
        ui: ServiceUiDecl {
            enabled: release.frontend.enabled,
            ..ServiceUiDecl::default()
        },
        permissions: release.permissions.clone(),
        security: Default::default(),
        source: SourceDecl {
            r#type: "release".to_string(),
            reference: source_url.trim().to_string(),
            ..SourceDecl::default()
        },
        health: ServiceHealthDecl {
            checks: vec!["http".to_string()],
            timeout_seconds: 5,
            interval_seconds: 30,
        },
        resources: Default::default(),
    })
}

/// 把导入结果注册进 store：Service 清单 + Release 记录两步都必须写，
/// 否则后续 release.install 计划阶段会因缺少 Release 记录而失败。
pub fn register_external_release_into_store<S: OrchestratorStore>(
    store: &mut S,
    import: &mut ExternalReleaseImport,
) -> Result<()> {
    import.replaced_existing = store.get_service(&import.service.id)?.is_some();
    let release = ServiceRelease {
        service_name: import.release.service_name.clone(),
        version: import.release.version.clone(),
        release_url: import.source_url.clone(),
        manifest: if import.stored_manifest.is_object() {
            import.stored_manifest.clone()
        } else {
            // Backward compatibility for a deserialized 0.2 import result.
            serde_json::to_value(&import.release)?
        },
        checksum: import.checksum.clone(),
        created_at: String::new(),
    };
    store.register_service_release_atomic(import.service.clone(), release)?;
    Ok(())
}
