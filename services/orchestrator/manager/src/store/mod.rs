//! Store application rules. HTTP status mapping, persistence and runtime execution live in adapters.
pub mod composition;
pub mod config;
pub mod validation;

use crate::catalog_v2::{InstallPlanV2, ResolvedReleaseV2};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreRuleErrorKind {
    InvalidInput,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct StoreRuleError {
    pub kind: StoreRuleErrorKind,
    pub code: &'static str,
    pub detail: String,
}

impl StoreRuleError {
    pub fn invalid(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind: StoreRuleErrorKind::InvalidInput,
            code,
            detail: detail.into(),
        }
    }
    pub fn conflict(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            kind: StoreRuleErrorKind::Conflict,
            code,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResolvedCatalogPlan {
    pub source_id: String,
    pub catalog_id: String,
    pub verified_key_ids: Vec<String>,
    pub plan: InstallPlanV2,
}

#[derive(Debug, Clone)]
pub struct VerifiedReleaseDocument {
    pub selection: ResolvedReleaseV2,
    pub source_url: String,
    pub checksum: String,
    pub bytes: Vec<u8>,
    pub offline_oci_layout: Option<PathBuf>,
}

/// This identity deliberately excludes release, deployment, and Node so a
/// retained resource survives upgrades, rollbacks, and rescheduling. Explicit
/// multi-instance support must add a persisted slot id instead of changing this
/// derivation implicitly.
pub fn stable_service_instance_id(service_id: &str) -> String {
    let digest = Sha256::digest(format!("default\0{service_id}").as_bytes());
    format!("service-instance-{digest:x}")
}

pub fn deployment_id(service_id: &str, version: &semver::Version, node_id: &str) -> String {
    let digest = Sha256::digest(format!("{service_id}\0{version}\0{node_id}").as_bytes());
    format!("deployment-{service_id}-{:x}", digest)[..56].to_string()
}
