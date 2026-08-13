use regex::Regex;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;

pub const CONTRIBUTION_REVISION_SCHEMA_VERSION: &str = "ojos.dev/contribution-revision/v1";

pub const RESERVED_CONTRIBUTION_PATH_PREFIXES: &[&str] = &[
    "/_ojos",
    "/debug",
    "/health",
    "/healthz",
    "/internal",
    "/live",
    "/livez",
    "/metrics",
    "/ready",
    "/readyz",
];

const MAX_IDENTIFIER_LEN: usize = 128;
const MAX_PATH_LEN: usize = 512;
const MAX_TEXT_LEN: usize = 512;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContributionDomainError {
    #[error("invalid contribution: {0}")]
    Invalid(String),
    #[error(
        "contribution head compare-and-swap conflict (expected {expected:?}, actual {actual:?})"
    )]
    CasConflict {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("invalid {entity} transition from {from} to {to}")]
    InvalidTransition {
        entity: &'static str,
        from: String,
        to: String,
    },
    #[error("contribution routes conflict: {0:?}")]
    RouteConflict(Vec<ContributionRouteCollisionV1>),
}

pub type ContributionResult<T> = std::result::Result<T, ContributionDomainError>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContributionRevisionStatusV1 {
    Staged,
    Active,
    Retired,
    Aborted,
}

impl ContributionRevisionStatusV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "STAGED",
            Self::Active => "ACTIVE",
            Self::Retired => "RETIRED",
            Self::Aborted => "ABORTED",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContributionAudienceV1 {
    Internal,
    User,
    Public,
    Admin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "UPPERCASE")]
pub enum ContributionHttpMethodV1 {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
}

impl ContributionHttpMethodV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContributionRouteAuthV1 {
    Anonymous,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ContributionSystemPermissionScopeV1 {
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContributionPathPermissionScopeV1 {
    #[serde(rename = "type")]
    pub scope_type: String,
    pub path_parameter: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
pub enum ContributionPermissionScopeV1 {
    System(ContributionSystemPermissionScopeV1),
    PathParameter(ContributionPathPermissionScopeV1),
}

impl ContributionPermissionScopeV1 {
    pub const fn system() -> Self {
        Self::System(ContributionSystemPermissionScopeV1::System)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ContributionApiSurfaceV1 {
    pub api_id: String,
    pub api_version: String,
    pub protocol: String,
    pub base_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ContributionOperationRouteV1 {
    pub audience: ContributionAudienceV1,
    pub method: ContributionHttpMethodV1,
    pub path: String,
    pub api_id: String,
    pub operation_id: String,
    pub provider_path: String,
    pub auth: ContributionRouteAuthV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_scope: Option<ContributionPermissionScopeV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ContributionPermissionDefinitionV1 {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ContributionFrontendModuleV1 {
    pub module_id: String,
    pub surface_id: String,
    pub route: String,
    pub menu_label: String,
    #[serde(default)]
    pub menu: bool,
    #[serde(default)]
    pub order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    pub artifact: String,
    pub host_api_range: String,
    pub manifest_digest: String,
    pub manifest_reference: String,
    pub bundle_digest: String,
    pub bundle_reference: String,
}

/// Permission grants deliberately live outside contribution revisions. A
/// revision owns permission definitions, never user/role/service assignments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionAssignmentV1 {
    pub assignment_id: String,
    pub scope_id: String,
    pub permission_key: String,
    pub subject_kind: PermissionSubjectKindV1,
    pub subject_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PermissionSubjectKindV1 {
    User,
    Role,
    Service,
}

impl PermissionAssignmentV1 {
    pub fn validate(&self) -> ContributionResult<()> {
        validate_stable_identifier("assignment_id", &self.assignment_id)?;
        validate_stable_identifier("scope_id", &self.scope_id)?;
        validate_permission_key(&self.permission_key)?;
        validate_stable_identifier("subject_id", &self.subject_id)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContributionRevisionV1 {
    schema_version: String,
    revision_id: String,
    scope_id: String,
    deployment_id: String,
    service_id: String,
    release_digest: String,
    contract_digest: String,
    generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_revision_id: Option<String>,
    status: ContributionRevisionStatusV1,
    api_surfaces: Vec<ContributionApiSurfaceV1>,
    operation_routes: Vec<ContributionOperationRouteV1>,
    permission_definitions: Vec<ContributionPermissionDefinitionV1>,
    user_frontend_modules: Vec<ContributionFrontendModuleV1>,
    admin_frontend_modules: Vec<ContributionFrontendModuleV1>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributionRevisionWireV1 {
    schema_version: String,
    revision_id: String,
    scope_id: String,
    deployment_id: String,
    service_id: String,
    release_digest: String,
    contract_digest: String,
    generation: u64,
    #[serde(default)]
    previous_revision_id: Option<String>,
    status: ContributionRevisionStatusV1,
    #[serde(default)]
    api_surfaces: Vec<ContributionApiSurfaceV1>,
    #[serde(default)]
    operation_routes: Vec<ContributionOperationRouteV1>,
    #[serde(default)]
    permission_definitions: Vec<ContributionPermissionDefinitionV1>,
    #[serde(default)]
    user_frontend_modules: Vec<ContributionFrontendModuleV1>,
    #[serde(default)]
    admin_frontend_modules: Vec<ContributionFrontendModuleV1>,
}

impl<'de> Deserialize<'de> for ContributionRevisionV1 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let wire = ContributionRevisionWireV1::deserialize(deserializer)?;
        let revision = Self {
            schema_version: wire.schema_version,
            revision_id: wire.revision_id,
            scope_id: wire.scope_id,
            deployment_id: wire.deployment_id,
            service_id: wire.service_id,
            release_digest: wire.release_digest,
            contract_digest: wire.contract_digest,
            generation: wire.generation,
            previous_revision_id: wire.previous_revision_id,
            status: wire.status,
            api_surfaces: wire.api_surfaces,
            operation_routes: wire.operation_routes,
            permission_definitions: wire.permission_definitions,
            user_frontend_modules: wire.user_frontend_modules,
            admin_frontend_modules: wire.admin_frontend_modules,
        };
        revision.validate().map_err(D::Error::custom)?;
        Ok(revision)
    }
}

#[derive(Serialize)]
struct RevisionHashMaterial<'a> {
    schema_version: &'static str,
    scope_id: &'a str,
    deployment_id: &'a str,
    service_id: &'a str,
    release_digest: &'a str,
    contract_digest: &'a str,
    generation: u64,
    previous_revision_id: Option<&'a str>,
    api_surfaces: &'a [ContributionApiSurfaceV1],
    operation_routes: &'a [ContributionOperationRouteV1],
    permission_definitions: &'a [ContributionPermissionDefinitionV1],
    user_frontend_modules: &'a [ContributionFrontendModuleV1],
    admin_frontend_modules: &'a [ContributionFrontendModuleV1],
}

impl ContributionRevisionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn stage(
        scope_id: impl Into<String>,
        deployment_id: impl Into<String>,
        service_id: impl Into<String>,
        release_digest: impl Into<String>,
        contract_digest: impl Into<String>,
        generation: u64,
        previous_revision_id: Option<String>,
        api_surfaces: Vec<ContributionApiSurfaceV1>,
        operation_routes: Vec<ContributionOperationRouteV1>,
        permission_definitions: Vec<ContributionPermissionDefinitionV1>,
        user_frontend_modules: Vec<ContributionFrontendModuleV1>,
        admin_frontend_modules: Vec<ContributionFrontendModuleV1>,
    ) -> ContributionResult<Self> {
        let mut revision = Self {
            schema_version: CONTRIBUTION_REVISION_SCHEMA_VERSION.to_string(),
            revision_id: String::new(),
            scope_id: scope_id.into(),
            deployment_id: deployment_id.into(),
            service_id: service_id.into(),
            release_digest: release_digest.into(),
            contract_digest: contract_digest.into(),
            generation,
            previous_revision_id,
            status: ContributionRevisionStatusV1::Staged,
            api_surfaces,
            operation_routes,
            permission_definitions,
            user_frontend_modules,
            admin_frontend_modules,
        };
        revision.canonicalize_in_place();
        revision.validate_content()?;
        revision.revision_id = revision.calculate_revision_id()?;
        revision.validate()?;
        Ok(revision)
    }

    pub fn validate(&self) -> ContributionResult<()> {
        if self.schema_version != CONTRIBUTION_REVISION_SCHEMA_VERSION {
            return invalid(format!(
                "schema_version must be {CONTRIBUTION_REVISION_SCHEMA_VERSION}"
            ));
        }
        self.validate_content()?;
        if !is_canonical_sha256(&self.revision_id) {
            return invalid("revision_id must be sha256:<64 lowercase hex>");
        }
        if self.revision_id != self.calculate_revision_id()? {
            return invalid("revision_id does not match immutable revision content");
        }
        let mut canonical = self.clone();
        canonical.canonicalize_in_place();
        if canonical.api_surfaces != self.api_surfaces
            || canonical.operation_routes != self.operation_routes
            || canonical.permission_definitions != self.permission_definitions
            || canonical.user_frontend_modules != self.user_frontend_modules
            || canonical.admin_frontend_modules != self.admin_frontend_modules
        {
            return invalid("revision child contributions must use canonical ordering");
        }
        Ok(())
    }

    fn validate_content(&self) -> ContributionResult<()> {
        validate_stable_identifier("scope_id", &self.scope_id)?;
        validate_stable_identifier("deployment_id", &self.deployment_id)?;
        validate_service_id(&self.service_id)?;
        validate_sha256("release_digest", &self.release_digest)?;
        validate_sha256("contract_digest", &self.contract_digest)?;
        if self.generation == 0 {
            return invalid("generation must be greater than zero");
        }
        match (self.generation, self.previous_revision_id.as_deref()) {
            (1, None) => {}
            (1, Some(_)) => return invalid("generation 1 cannot have previous_revision_id"),
            (_, None) => {
                // A first activation may follow durable ABORTED headless
                // revisions. The repository enforces exact historical
                // monotonicity before it accepts this form.
            }
            (_, Some(previous)) => validate_sha256("previous_revision_id", previous)?,
        }

        let mut api_ids = BTreeSet::new();
        for surface in &self.api_surfaces {
            validate_api_surface(surface)?;
            if !api_ids.insert(surface.api_id.as_str()) {
                return invalid(format!("duplicate API surface {}", surface.api_id));
            }
        }

        let mut permission_keys = BTreeSet::new();
        for permission in &self.permission_definitions {
            validate_permission_definition(permission, &self.service_id)?;
            if !permission_keys.insert(permission.key.as_str()) {
                return invalid(format!(
                    "duplicate permission definition {}",
                    permission.key
                ));
            }
        }

        for route in &self.operation_routes {
            validate_operation_route(route)?;
            if !api_ids.contains(route.api_id.as_str()) {
                return invalid(format!(
                    "route operation {} references undeclared API surface {}",
                    route.operation_id, route.api_id
                ));
            }
        }
        validate_routes_do_not_overlap(&self.operation_routes)?;

        validate_frontend_modules("user", &self.user_frontend_modules)?;
        validate_frontend_modules("admin", &self.admin_frontend_modules)?;
        Ok(())
    }

    fn canonicalize_in_place(&mut self) {
        self.api_surfaces.sort();
        self.operation_routes.sort();
        self.permission_definitions.sort();
        self.user_frontend_modules.sort();
        self.admin_frontend_modules.sort();
    }

    fn calculate_revision_id(&self) -> ContributionResult<String> {
        let material = RevisionHashMaterial {
            schema_version: CONTRIBUTION_REVISION_SCHEMA_VERSION,
            scope_id: &self.scope_id,
            deployment_id: &self.deployment_id,
            service_id: &self.service_id,
            release_digest: &self.release_digest,
            contract_digest: &self.contract_digest,
            generation: self.generation,
            previous_revision_id: self.previous_revision_id.as_deref(),
            api_surfaces: &self.api_surfaces,
            operation_routes: &self.operation_routes,
            permission_definitions: &self.permission_definitions,
            user_frontend_modules: &self.user_frontend_modules,
            admin_frontend_modules: &self.admin_frontend_modules,
        };
        canonical_sha256(&material)
    }

    fn transition_to(&self, to: ContributionRevisionStatusV1) -> ContributionResult<Self> {
        self.validate()?;
        let valid = matches!(
            (self.status, to),
            (
                ContributionRevisionStatusV1::Staged,
                ContributionRevisionStatusV1::Active | ContributionRevisionStatusV1::Aborted
            ) | (
                ContributionRevisionStatusV1::Active,
                ContributionRevisionStatusV1::Retired
            )
        );
        if !valid {
            return Err(ContributionDomainError::InvalidTransition {
                entity: "contribution revision",
                from: self.status.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
        let mut next = self.clone();
        next.status = to;
        next.validate()?;
        debug_assert_eq!(self.revision_id, next.revision_id);
        Ok(next)
    }

    pub fn activate(&self) -> ContributionResult<Self> {
        self.transition_to(ContributionRevisionStatusV1::Active)
    }

    pub fn retire(&self) -> ContributionResult<Self> {
        self.transition_to(ContributionRevisionStatusV1::Retired)
    }

    pub fn abort(&self) -> ContributionResult<Self> {
        self.transition_to(ContributionRevisionStatusV1::Aborted)
    }

    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }

    pub fn revision_id(&self) -> &str {
        &self.revision_id
    }

    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    pub fn deployment_id(&self) -> &str {
        &self.deployment_id
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    pub fn release_digest(&self) -> &str {
        &self.release_digest
    }

    pub fn contract_digest(&self) -> &str {
        &self.contract_digest
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn previous_revision_id(&self) -> Option<&str> {
        self.previous_revision_id.as_deref()
    }

    pub fn status(&self) -> ContributionRevisionStatusV1 {
        self.status
    }

    pub fn api_surfaces(&self) -> &[ContributionApiSurfaceV1] {
        &self.api_surfaces
    }

    pub fn operation_routes(&self) -> &[ContributionOperationRouteV1] {
        &self.operation_routes
    }

    pub fn permission_definitions(&self) -> &[ContributionPermissionDefinitionV1] {
        &self.permission_definitions
    }

    pub fn user_frontend_modules(&self) -> &[ContributionFrontendModuleV1] {
        &self.user_frontend_modules
    }

    pub fn admin_frontend_modules(&self) -> &[ContributionFrontendModuleV1] {
        &self.admin_frontend_modules
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContributionHeadV1 {
    scope_id: String,
    service_id: String,
    active_revision_id: String,
    generation: u64,
    etag: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributionHeadWireV1 {
    scope_id: String,
    service_id: String,
    active_revision_id: String,
    generation: u64,
    etag: String,
}

impl<'de> Deserialize<'de> for ContributionHeadV1 {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;

        let wire = ContributionHeadWireV1::deserialize(deserializer)?;
        let head = Self {
            scope_id: wire.scope_id,
            service_id: wire.service_id,
            active_revision_id: wire.active_revision_id,
            generation: wire.generation,
            etag: wire.etag,
        };
        head.validate().map_err(D::Error::custom)?;
        Ok(head)
    }
}

#[derive(Serialize)]
struct HeadHashMaterial<'a> {
    schema_version: &'static str,
    scope_id: &'a str,
    service_id: &'a str,
    active_revision_id: &'a str,
    generation: u64,
}

impl ContributionHeadV1 {
    fn from_active_revision(revision: &ContributionRevisionV1) -> ContributionResult<Self> {
        revision.validate()?;
        if revision.status != ContributionRevisionStatusV1::Active {
            return invalid("contribution head requires an ACTIVE revision");
        }
        let mut head = Self {
            scope_id: revision.scope_id.clone(),
            service_id: revision.service_id.clone(),
            active_revision_id: revision.revision_id.clone(),
            generation: revision.generation,
            etag: String::new(),
        };
        head.etag = head.calculate_etag()?;
        head.validate()?;
        Ok(head)
    }

    fn restored_to(revision: &ContributionRevisionV1, generation: u64) -> ContributionResult<Self> {
        revision.validate()?;
        if revision.status != ContributionRevisionStatusV1::Active {
            return invalid("restored contribution head requires an ACTIVE revision");
        }
        if generation < revision.generation {
            return invalid("restored head generation cannot precede the revision generation");
        }
        let mut head = Self {
            scope_id: revision.scope_id.clone(),
            service_id: revision.service_id.clone(),
            active_revision_id: revision.revision_id.clone(),
            generation,
            etag: String::new(),
        };
        head.etag = head.calculate_etag()?;
        head.validate()?;
        Ok(head)
    }

    pub fn validate(&self) -> ContributionResult<()> {
        validate_stable_identifier("head scope_id", &self.scope_id)?;
        validate_service_id(&self.service_id)?;
        validate_sha256("head active_revision_id", &self.active_revision_id)?;
        if self.generation == 0 {
            return invalid("head generation must be greater than zero");
        }
        validate_sha256("head etag", &self.etag)?;
        if self.etag != self.calculate_etag()? {
            return invalid("head etag does not match immutable head content");
        }
        Ok(())
    }

    fn calculate_etag(&self) -> ContributionResult<String> {
        canonical_sha256(&HeadHashMaterial {
            schema_version: "ojos.dev/contribution-head/v1",
            scope_id: &self.scope_id,
            service_id: &self.service_id,
            active_revision_id: &self.active_revision_id,
            generation: self.generation,
        })
    }

    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    pub fn active_revision_id(&self) -> &str {
        &self.active_revision_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn etag(&self) -> &str {
        &self.etag
    }
}

/// Pure compare-and-swap. The caller persists the returned value atomically;
/// this function performs no storage or network I/O.
pub fn compare_and_swap_contribution_head(
    current: Option<&ContributionHeadV1>,
    expected_etag: Option<&str>,
    active_revision: &ContributionRevisionV1,
) -> ContributionResult<ContributionHeadV1> {
    active_revision.validate()?;
    if active_revision.status != ContributionRevisionStatusV1::Active {
        return invalid("head CAS requires an ACTIVE candidate revision");
    }

    let actual_etag = current.map(|head| head.etag.clone());
    if expected_etag != actual_etag.as_deref() {
        return Err(ContributionDomainError::CasConflict {
            expected: expected_etag.map(str::to_string),
            actual: actual_etag,
        });
    }

    match current {
        None => {
            if active_revision.previous_revision_id.is_some() {
                return invalid("initial head CAS cannot name a previous revision");
            }
        }
        Some(head) => {
            head.validate()?;
            if head.scope_id != active_revision.scope_id
                || head.service_id != active_revision.service_id
            {
                return invalid("head and candidate revision identities do not match");
            }
            if active_revision.generation <= head.generation {
                return invalid(format!(
                    "candidate generation must be greater than {}",
                    head.generation
                ));
            }
            if active_revision.previous_revision_id.as_deref()
                != Some(head.active_revision_id.as_str())
            {
                return invalid("candidate previous_revision_id must match the current head");
            }
        }
    }
    ContributionHeadV1::from_active_revision(active_revision)
}

/// Result of compensating a head activation. Revision identifiers remain
/// immutable: the failed candidate is retired, the previous revision is made
/// active again, and the head's generation is deliberately not decremented.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContributionHeadRestoreV1 {
    pub head: ContributionHeadV1,
    pub restored_revision: ContributionRevisionV1,
    pub retired_candidate: ContributionRevisionV1,
}

/// Result of compensating a first-ever activation. The active successor is an
/// empty tombstone, so no route, permission definition, or frontend module
/// remains published, while generation and ETag continue monotonically.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContributionHeadClearV1 {
    pub head: ContributionHeadV1,
    pub tombstone_revision: ContributionRevisionV1,
    pub retired_candidate: ContributionRevisionV1,
}

/// Pure compensating CAS for an activation that already moved the head.
///
/// The current head ETag must be the exact candidate ETag. The returned head
/// points at the previous revision while retaining the candidate generation,
/// so a later activation cannot observe the pre-activation ETag (ABA) and must
/// use the next monotonically increasing generation.
pub fn restore_contribution_head(
    current: &ContributionHeadV1,
    expected_candidate_etag: &str,
    candidate: &ContributionRevisionV1,
    previous: &ContributionRevisionV1,
) -> ContributionResult<ContributionHeadRestoreV1> {
    current.validate()?;
    candidate.validate()?;
    previous.validate()?;

    if current.etag != expected_candidate_etag {
        return Err(ContributionDomainError::CasConflict {
            expected: Some(expected_candidate_etag.to_string()),
            actual: Some(current.etag.clone()),
        });
    }
    if candidate.status != ContributionRevisionStatusV1::Active {
        return invalid("head restore requires the candidate revision to be ACTIVE");
    }
    if !matches!(
        previous.status,
        ContributionRevisionStatusV1::Active | ContributionRevisionStatusV1::Retired
    ) {
        return invalid("head restore requires the previous revision to be ACTIVE or RETIRED");
    }
    if current.scope_id != candidate.scope_id
        || current.service_id != candidate.service_id
        || current.scope_id != previous.scope_id
        || current.service_id != previous.service_id
    {
        return invalid("head restore identities do not match");
    }
    if current.active_revision_id != candidate.revision_id
        || current.generation != candidate.generation
    {
        return invalid("current head must reference the exact ACTIVE candidate generation");
    }
    if candidate.previous_revision_id.as_deref() != Some(previous.revision_id.as_str()) {
        return invalid("candidate previous_revision_id must identify the restored revision");
    }
    if previous.generation >= candidate.generation {
        return invalid("restored revision generation must precede the candidate generation");
    }

    let retired_candidate = candidate.retire()?;
    let mut restored_revision = previous.clone();
    restored_revision.status = ContributionRevisionStatusV1::Active;
    restored_revision.validate()?;
    let head = ContributionHeadV1::restored_to(&restored_revision, current.generation)?;
    debug_assert_ne!(head.etag, current.etag);

    Ok(ContributionHeadRestoreV1 {
        head,
        restored_revision,
        retired_candidate,
    })
}

pub fn clear_initial_contribution_head(
    current: &ContributionHeadV1,
    expected_candidate_etag: &str,
    candidate: &ContributionRevisionV1,
) -> ContributionResult<ContributionHeadClearV1> {
    current.validate()?;
    candidate.validate()?;
    if current.etag != expected_candidate_etag {
        return Err(ContributionDomainError::CasConflict {
            expected: Some(expected_candidate_etag.to_string()),
            actual: Some(current.etag.clone()),
        });
    }
    if candidate.status != ContributionRevisionStatusV1::Active
        || candidate.previous_revision_id.is_some()
        || current.active_revision_id != candidate.revision_id
        || current.scope_id != candidate.scope_id
        || current.service_id != candidate.service_id
        || current.generation != candidate.generation
    {
        return invalid("initial head clear requires the exact first ACTIVE candidate head");
    }
    let generation = current
        .generation
        .checked_add(1)
        .ok_or_else(|| ContributionDomainError::Invalid("head generation overflow".into()))?;
    let tombstone_revision = ContributionRevisionV1::stage(
        candidate.scope_id.clone(),
        candidate.deployment_id.clone(),
        candidate.service_id.clone(),
        candidate.release_digest.clone(),
        candidate.contract_digest.clone(),
        generation,
        Some(candidate.revision_id.clone()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )?
    .activate()?;
    let head = compare_and_swap_contribution_head(
        Some(current),
        Some(expected_candidate_etag),
        &tombstone_revision,
    )?;
    let retired_candidate = candidate.retire()?;
    Ok(ContributionHeadClearV1 {
        head,
        tombstone_revision,
        retired_candidate,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContributionRouteCollisionV1 {
    pub candidate_revision_id: String,
    pub existing_revision_id: String,
    pub audience: ContributionAudienceV1,
    pub candidate_method: ContributionHttpMethodV1,
    pub existing_method: ContributionHttpMethodV1,
    pub candidate_path: String,
    pub existing_path: String,
    pub candidate_operation_id: String,
    pub existing_operation_id: String,
}

/// Returns collisions against live revisions in the same scope. A candidate's
/// explicitly named previous revision is excluded because activation replaces
/// that revision. Retired and aborted revisions cannot own live routes.
pub fn stage_route_collisions(
    candidate: &ContributionRevisionV1,
    existing: &[ContributionRevisionV1],
) -> ContributionResult<Vec<ContributionRouteCollisionV1>> {
    candidate.validate()?;
    if candidate.status != ContributionRevisionStatusV1::Staged {
        return invalid("stage collision detection requires a STAGED candidate");
    }
    let mut collisions = Vec::new();
    for revision in existing {
        revision.validate()?;
        if revision.scope_id != candidate.scope_id
            || revision.revision_id == candidate.revision_id
            || candidate.previous_revision_id.as_deref() == Some(revision.revision_id.as_str())
            || matches!(
                revision.status,
                ContributionRevisionStatusV1::Retired | ContributionRevisionStatusV1::Aborted
            )
        {
            continue;
        }
        for candidate_route in &candidate.operation_routes {
            for existing_route in &revision.operation_routes {
                if candidate_route.audience == existing_route.audience
                    && methods_overlap(candidate_route.method, existing_route.method)
                    && route_templates_overlap(&candidate_route.path, &existing_route.path)?
                {
                    collisions.push(ContributionRouteCollisionV1 {
                        candidate_revision_id: candidate.revision_id.clone(),
                        existing_revision_id: revision.revision_id.clone(),
                        audience: candidate_route.audience,
                        candidate_method: candidate_route.method,
                        existing_method: existing_route.method,
                        candidate_path: candidate_route.path.clone(),
                        existing_path: existing_route.path.clone(),
                        candidate_operation_id: candidate_route.operation_id.clone(),
                        existing_operation_id: existing_route.operation_id.clone(),
                    });
                }
            }
        }
    }
    collisions.sort_by(|left, right| {
        (
            &left.existing_revision_id,
            left.audience,
            left.candidate_method,
            &left.candidate_path,
            &left.existing_path,
        )
            .cmp(&(
                &right.existing_revision_id,
                right.audience,
                right.candidate_method,
                &right.candidate_path,
                &right.existing_path,
            ))
    });
    Ok(collisions)
}

pub fn validate_stage_route_collisions(
    candidate: &ContributionRevisionV1,
    existing: &[ContributionRevisionV1],
) -> ContributionResult<()> {
    let collisions = stage_route_collisions(candidate, existing)?;
    if collisions.is_empty() {
        Ok(())
    } else {
        Err(ContributionDomainError::RouteConflict(collisions))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContributionActivationStateV1 {
    Preparing,
    Committing,
    Compensating,
    Succeeded,
    Aborted,
    NeedsAttention,
}

impl ContributionActivationStateV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "PREPARING",
            Self::Committing => "COMMITTING",
            Self::Compensating => "COMPENSATING",
            Self::Succeeded => "SUCCEEDED",
            Self::Aborted => "ABORTED",
            Self::NeedsAttention => "NEEDS_ATTENTION",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContributionTerminationIntentV1 {
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContributionActivationV1 {
    activation_id: String,
    scope_id: String,
    service_id: String,
    candidate_revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_revision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_head_etag: Option<String>,
    state: ContributionActivationStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    termination_intent: Option<ContributionTerminationIntentV1>,
}

impl ContributionActivationV1 {
    pub fn prepare(
        activation_id: impl Into<String>,
        candidate: &ContributionRevisionV1,
        expected_head_etag: Option<String>,
    ) -> ContributionResult<Self> {
        candidate.validate()?;
        if candidate.status != ContributionRevisionStatusV1::Staged {
            return invalid("activation preparation requires a STAGED revision");
        }
        let activation = Self {
            activation_id: activation_id.into(),
            scope_id: candidate.scope_id.clone(),
            service_id: candidate.service_id.clone(),
            candidate_revision_id: candidate.revision_id.clone(),
            previous_revision_id: candidate.previous_revision_id.clone(),
            expected_head_etag,
            state: ContributionActivationStateV1::Preparing,
            termination_intent: None,
        };
        activation.validate()?;
        Ok(activation)
    }

    pub fn validate(&self) -> ContributionResult<()> {
        validate_stable_identifier("activation_id", &self.activation_id)?;
        validate_stable_identifier("activation scope_id", &self.scope_id)?;
        validate_service_id(&self.service_id)?;
        validate_sha256("candidate_revision_id", &self.candidate_revision_id)?;
        if let Some(previous) = self.previous_revision_id.as_deref() {
            validate_sha256("activation previous_revision_id", previous)?;
        }
        if let Some(etag) = self.expected_head_etag.as_deref() {
            validate_sha256("expected_head_etag", etag)?;
        }
        match self.state {
            ContributionActivationStateV1::Preparing
            | ContributionActivationStateV1::Committing
            | ContributionActivationStateV1::Succeeded
                if self.termination_intent.is_some() =>
            {
                return invalid("normal activation states cannot have termination_intent");
            }
            ContributionActivationStateV1::Compensating
            | ContributionActivationStateV1::Aborted
                if self.termination_intent.is_none() =>
            {
                return invalid("compensating and aborted activations require termination_intent");
            }
            _ => {}
        }
        Ok(())
    }

    fn transition(
        &self,
        state: ContributionActivationStateV1,
        termination_intent: Option<ContributionTerminationIntentV1>,
    ) -> ContributionResult<Self> {
        self.validate()?;
        let valid = matches!(
            (self.state, state),
            (
                ContributionActivationStateV1::Preparing,
                ContributionActivationStateV1::Committing
                    | ContributionActivationStateV1::Compensating
                    | ContributionActivationStateV1::NeedsAttention
            ) | (
                ContributionActivationStateV1::Committing,
                ContributionActivationStateV1::Succeeded
                    | ContributionActivationStateV1::Compensating
                    | ContributionActivationStateV1::NeedsAttention
            ) | (
                ContributionActivationStateV1::Succeeded,
                ContributionActivationStateV1::Compensating
            ) | (
                ContributionActivationStateV1::Compensating,
                ContributionActivationStateV1::Aborted
                    | ContributionActivationStateV1::NeedsAttention
            )
        );
        if !valid {
            return Err(ContributionDomainError::InvalidTransition {
                entity: "contribution activation",
                from: self.state.as_str().to_string(),
                to: state.as_str().to_string(),
            });
        }
        let mut next = self.clone();
        next.state = state;
        next.termination_intent = termination_intent.or(self.termination_intent);
        next.validate()?;
        Ok(next)
    }

    pub fn begin_commit(&self) -> ContributionResult<Self> {
        self.transition(ContributionActivationStateV1::Committing, None)
    }

    pub fn begin_compensation(
        &self,
        intent: ContributionTerminationIntentV1,
    ) -> ContributionResult<Self> {
        self.transition(ContributionActivationStateV1::Compensating, Some(intent))
    }

    pub fn succeed(&self) -> ContributionResult<Self> {
        self.transition(ContributionActivationStateV1::Succeeded, None)
    }

    pub fn finish_abort(&self) -> ContributionResult<Self> {
        self.transition(
            ContributionActivationStateV1::Aborted,
            self.termination_intent,
        )
    }

    pub fn needs_attention(&self) -> ContributionResult<Self> {
        self.transition(
            ContributionActivationStateV1::NeedsAttention,
            self.termination_intent,
        )
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    pub fn candidate_revision_id(&self) -> &str {
        &self.candidate_revision_id
    }

    pub fn previous_revision_id(&self) -> Option<&str> {
        self.previous_revision_id.as_deref()
    }

    pub fn expected_head_etag(&self) -> Option<&str> {
        self.expected_head_etag.as_deref()
    }

    pub fn state(&self) -> ContributionActivationStateV1 {
        self.state
    }

    pub fn termination_intent(&self) -> Option<ContributionTerminationIntentV1> {
        self.termination_intent
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectionTargetV1 {
    ApiRegistry,
    Auth,
    Gateway,
    UserShell,
    AdminShell,
}

impl ProjectionTargetV1 {
    pub const ALL: [Self; 5] = [
        Self::ApiRegistry,
        Self::Auth,
        Self::Gateway,
        Self::UserShell,
        Self::AdminShell,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiRegistry => "API_REGISTRY",
            Self::Auth => "AUTH",
            Self::Gateway => "GATEWAY",
            Self::UserShell => "USER_SHELL",
            Self::AdminShell => "ADMIN_SHELL",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProjectionReceiptStateV1 {
    Pending,
    Staged,
    Active,
    Restored,
    Failed,
    Unknown,
}

impl ProjectionReceiptStateV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Staged => "STAGED",
            Self::Active => "ACTIVE",
            Self::Restored => "RESTORED",
            Self::Failed => "FAILED",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionReceiptV1 {
    activation_id: String,
    target: ProjectionTargetV1,
    candidate_revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_revision_id: Option<String>,
    candidate_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_generation: Option<u64>,
    state: ProjectionReceiptStateV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    staged_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

impl ProjectionReceiptV1 {
    pub fn pending(
        activation_id: impl Into<String>,
        target: ProjectionTargetV1,
        candidate: &ContributionRevisionV1,
    ) -> ContributionResult<Self> {
        candidate.validate()?;
        let receipt = Self {
            activation_id: activation_id.into(),
            target,
            candidate_revision_id: candidate.revision_id.clone(),
            previous_revision_id: candidate.previous_revision_id.clone(),
            candidate_generation: candidate.generation,
            observed_generation: None,
            state: ProjectionReceiptStateV1::Pending,
            staged_digest: None,
            active_digest: None,
            last_error: None,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> ContributionResult<()> {
        validate_stable_identifier("receipt activation_id", &self.activation_id)?;
        validate_sha256("receipt candidate_revision_id", &self.candidate_revision_id)?;
        if let Some(previous) = self.previous_revision_id.as_deref() {
            validate_sha256("receipt previous_revision_id", previous)?;
        }
        if self.candidate_generation == 0 || self.observed_generation == Some(0) {
            return invalid("receipt generations must be greater than zero");
        }
        if let Some(digest) = self.staged_digest.as_deref() {
            validate_sha256("receipt staged_digest", digest)?;
        }
        if let Some(digest) = self.active_digest.as_deref() {
            validate_sha256("receipt active_digest", digest)?;
        }
        match self.state {
            ProjectionReceiptStateV1::Pending
                if self.observed_generation.is_some()
                    || self.staged_digest.is_some()
                    || self.active_digest.is_some()
                    || self.last_error.is_some() =>
            {
                return invalid("pending receipt cannot contain observations");
            }
            ProjectionReceiptStateV1::Staged if self.staged_digest.is_none() => {
                return invalid("staged receipt requires staged_digest");
            }
            ProjectionReceiptStateV1::Active
                if self.active_digest.is_none()
                    || self.observed_generation != Some(self.candidate_generation) =>
            {
                return invalid(
                    "active receipt requires active_digest and candidate observed generation",
                );
            }
            ProjectionReceiptStateV1::Restored
                if self.active_digest.is_none() || self.observed_generation.is_none() =>
            {
                return invalid(
                    "restored receipt requires the applied snapshot digest and observed generation",
                );
            }
            ProjectionReceiptStateV1::Failed | ProjectionReceiptStateV1::Unknown
                if self
                    .last_error
                    .as_deref()
                    .is_none_or(|error| error.trim().is_empty()) =>
            {
                return invalid("failed and unknown receipts require last_error");
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        state: ProjectionReceiptStateV1,
        observed_generation: Option<u64>,
        staged_digest: Option<String>,
        active_digest: Option<String>,
        last_error: Option<String>,
    ) -> ContributionResult<Self> {
        self.validate()?;
        let valid = matches!(
            (self.state, state),
            (
                ProjectionReceiptStateV1::Pending,
                ProjectionReceiptStateV1::Staged
            ) | (
                ProjectionReceiptStateV1::Pending,
                ProjectionReceiptStateV1::Failed | ProjectionReceiptStateV1::Unknown
            ) | (
                ProjectionReceiptStateV1::Staged,
                ProjectionReceiptStateV1::Active
                    | ProjectionReceiptStateV1::Restored
                    | ProjectionReceiptStateV1::Failed
                    | ProjectionReceiptStateV1::Unknown
            ) | (
                ProjectionReceiptStateV1::Active,
                ProjectionReceiptStateV1::Restored
                    | ProjectionReceiptStateV1::Failed
                    | ProjectionReceiptStateV1::Unknown
            ) | (
                ProjectionReceiptStateV1::Unknown,
                ProjectionReceiptStateV1::Staged
                    | ProjectionReceiptStateV1::Active
                    | ProjectionReceiptStateV1::Restored
                    | ProjectionReceiptStateV1::Failed
            ) | (
                ProjectionReceiptStateV1::Failed,
                ProjectionReceiptStateV1::Restored | ProjectionReceiptStateV1::Unknown
            ) | (
                ProjectionReceiptStateV1::Active,
                ProjectionReceiptStateV1::Active
            ) | (
                ProjectionReceiptStateV1::Restored,
                ProjectionReceiptStateV1::Restored
            )
        );
        if !valid {
            return Err(ContributionDomainError::InvalidTransition {
                entity: "projection receipt",
                from: self.state.as_str().to_string(),
                to: state.as_str().to_string(),
            });
        }
        let mut next = self.clone();
        next.state = state;
        next.observed_generation = observed_generation;
        next.staged_digest = staged_digest;
        next.active_digest = active_digest;
        next.last_error = last_error;
        next.validate()?;
        Ok(next)
    }

    pub fn target(&self) -> ProjectionTargetV1 {
        self.target
    }

    pub fn state(&self) -> ProjectionReceiptStateV1 {
        self.state
    }

    pub fn candidate_generation(&self) -> u64 {
        self.candidate_generation
    }

    pub fn activation_id(&self) -> &str {
        &self.activation_id
    }

    pub fn candidate_revision_id(&self) -> &str {
        &self.candidate_revision_id
    }

    pub fn observed_generation(&self) -> Option<u64> {
        self.observed_generation
    }

    pub fn staged_digest(&self) -> Option<&str> {
        self.staged_digest.as_deref()
    }

    pub fn active_digest(&self) -> Option<&str> {
        self.active_digest.as_deref()
    }
}

pub fn route_templates_overlap(left: &str, right: &str) -> ContributionResult<bool> {
    let left = parse_route_template(left)?;
    let right = parse_route_template(right)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    Ok(left.iter().zip(right.iter()).all(|(left, right)| {
        matches!(left, RouteSegment::Parameter)
            || matches!(right, RouteSegment::Parameter)
            || left == right
    }))
}

fn validate_api_surface(surface: &ContributionApiSurfaceV1) -> ContributionResult<()> {
    validate_stable_identifier("api_id", &surface.api_id)?;
    Version::parse(&surface.api_version).map_err(|error| {
        ContributionDomainError::Invalid(format!("invalid API version: {error}"))
    })?;
    if !matches!(surface.protocol.as_str(), "http" | "https") {
        return invalid("API surface protocol must be http or https");
    }
    validate_route_path("API surface base_path", &surface.base_path, true)?;
    Ok(())
}

fn validate_operation_route(route: &ContributionOperationRouteV1) -> ContributionResult<()> {
    validate_route_path("route path", &route.path, false)?;
    if is_reserved_contribution_path(&route.path) {
        return invalid(format!(
            "route path {} is reserved by the platform",
            route.path
        ));
    }
    validate_stable_identifier("route api_id", &route.api_id)?;
    validate_stable_identifier("route operation_id", &route.operation_id)?;
    validate_route_path("route provider_path", &route.provider_path, false)?;
    if let Some(permission) = route.permission.as_deref() {
        validate_permission_key(permission)?;
        if route.auth == ContributionRouteAuthV1::Anonymous {
            return invalid("anonymous routes cannot require a permission");
        }
    }
    match (&route.permission, &route.permission_scope) {
        (None, Some(_)) => return invalid("route permission_scope requires a permission"),
        (Some(_), Some(ContributionPermissionScopeV1::PathParameter(scope))) => {
            validate_permission_scope_type(&scope.scope_type)?;
            if scope.scope_type == "system" {
                return invalid("resource permission scope type cannot be system");
            }
            validate_stable_identifier(
                "route permission scope path_parameter",
                &scope.path_parameter,
            )?;
            let external_parameters = route_template_parameter_names(&route.path)?;
            let provider_parameters = route_template_parameter_names(&route.provider_path)?;
            if !external_parameters.contains(&scope.path_parameter)
                || !provider_parameters.contains(&scope.path_parameter)
            {
                return invalid(format!(
                    "route permission scope path_parameter {} must exist in external and provider templates",
                    scope.path_parameter
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_permission_definition(
    permission: &ContributionPermissionDefinitionV1,
    service_id: &str,
) -> ContributionResult<()> {
    validate_permission_key(&permission.key)?;
    let legacy_namespace = service_id
        .strip_suffix("-service")
        .or_else(|| service_id.strip_suffix("-api"));
    if !permission.key.starts_with(&format!("{service_id}."))
        && !legacy_namespace
            .is_some_and(|namespace| permission.key.starts_with(&format!("{namespace}.")))
    {
        return invalid(format!(
            "permission {} must be namespaced by service {service_id}",
            permission.key
        ));
    }
    validate_required_text("permission title", &permission.title, MAX_TEXT_LEN)?;
    validate_optional_text(
        "permission description",
        &permission.description,
        MAX_TEXT_LEN,
    )?;
    Ok(())
}

fn validate_frontend_modules(
    target: &str,
    modules: &[ContributionFrontendModuleV1],
) -> ContributionResult<()> {
    let mut surface_ids = BTreeSet::new();
    for module in modules {
        validate_stable_identifier("frontend module_id", &module.module_id)?;
        validate_stable_identifier("frontend surface_id", &module.surface_id)?;
        if !surface_ids.insert((module.module_id.as_str(), module.surface_id.as_str())) {
            return invalid(format!(
                "duplicate {target} frontend surface {} for module {}",
                module.surface_id, module.module_id
            ));
        }
        validate_route_path("frontend route", &module.route, false)?;
        if is_reserved_contribution_path(&module.route) {
            return invalid(format!(
                "frontend route {} is reserved by the platform",
                module.route
            ));
        }
        validate_required_text("frontend menu_label", &module.menu_label, MAX_TEXT_LEN)?;
        validate_artifact_path(&module.artifact)?;
        semver::VersionReq::parse(&module.host_api_range).map_err(|error| {
            ContributionDomainError::Invalid(format!(
                "frontend host_api_range for {} is invalid: {error}",
                module.module_id
            ))
        })?;
        validate_sha256("frontend manifest_digest", &module.manifest_digest)?;
        validate_content_addressed_reference(
            "frontend manifest_reference",
            &module.manifest_reference,
            &module.manifest_digest,
        )?;
        validate_sha256("frontend bundle_digest", &module.bundle_digest)?;
        validate_content_addressed_reference(
            "frontend bundle_reference",
            &module.bundle_reference,
            &module.bundle_digest,
        )?;
        if let Some(permission) = module.permission.as_deref() {
            validate_permission_key(permission)?;
        }
    }
    for left in 0..modules.len() {
        for right in (left + 1)..modules.len() {
            if route_templates_overlap(&modules[left].route, &modules[right].route)? {
                return invalid(format!(
                    "{target} frontend routes {} and {} overlap",
                    modules[left].route, modules[right].route
                ));
            }
        }
    }
    Ok(())
}

fn validate_artifact_path(value: &str) -> ContributionResult<()> {
    if value.is_empty()
        || value.len() > MAX_PATH_LEN
        || value.starts_with('/')
        || value.contains(['\\', '?', '#'])
        || value.split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || !segment.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
                })
        })
    {
        return invalid("frontend artifact must be a safe relative artifact path");
    }
    Ok(())
}

fn validate_content_addressed_reference(
    field: &str,
    reference: &str,
    digest: &str,
) -> ContributionResult<()> {
    let digest_hex = digest.strip_prefix("sha256:").unwrap_or_default();
    let valid = reference.starts_with("https://")
        && !reference.chars().any(char::is_whitespace)
        && reference.contains(digest_hex);
    if !valid {
        return invalid(format!(
            "{field} must be an HTTPS content-addressed reference containing its digest"
        ));
    }
    Ok(())
}

fn validate_routes_do_not_overlap(
    routes: &[ContributionOperationRouteV1],
) -> ContributionResult<()> {
    for left in 0..routes.len() {
        for right in (left + 1)..routes.len() {
            if routes[left].audience == routes[right].audience
                && methods_overlap(routes[left].method, routes[right].method)
                && route_templates_overlap(&routes[left].path, &routes[right].path)?
            {
                return invalid(format!(
                    "routes {} {} and {} {} overlap for {:?}",
                    routes[left].method.as_str(),
                    routes[left].path,
                    routes[right].method.as_str(),
                    routes[right].path,
                    routes[left].audience
                ));
            }
        }
    }
    Ok(())
}

fn methods_overlap(left: ContributionHttpMethodV1, right: ContributionHttpMethodV1) -> bool {
    left == right
        || matches!(
            (left, right),
            (
                ContributionHttpMethodV1::Get,
                ContributionHttpMethodV1::Head
            ) | (
                ContributionHttpMethodV1::Head,
                ContributionHttpMethodV1::Get
            )
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RouteSegment {
    Literal(String),
    Parameter,
}

fn parse_route_template(path: &str) -> ContributionResult<Vec<RouteSegment>> {
    validate_route_path("route template", path, true)?;
    if path == "/" {
        return Ok(Vec::new());
    }
    path[1..]
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') && segment.ends_with('}') {
                Ok(RouteSegment::Parameter)
            } else {
                Ok(RouteSegment::Literal(segment.to_string()))
            }
        })
        .collect()
}

fn route_template_parameter_names(path: &str) -> ContributionResult<BTreeSet<String>> {
    validate_route_path("route template", path, true)?;
    Ok(path
        .trim_start_matches('/')
        .split('/')
        .filter_map(|segment| {
            segment
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
                .map(str::to_string)
        })
        .collect())
}

fn validate_route_path(name: &str, path: &str, allow_root: bool) -> ContributionResult<()> {
    if path.is_empty() || path.len() > MAX_PATH_LEN || !path.starts_with('/') {
        return invalid(format!(
            "{name} must be an absolute path no longer than {MAX_PATH_LEN} bytes"
        ));
    }
    if !allow_root && path == "/" {
        return invalid(format!("{name} cannot claim the root path"));
    }
    if (path.len() > 1 && path.ends_with('/'))
        || path.contains("//")
        || path.contains('?')
        || path.contains('#')
        || path.contains('%')
    {
        return invalid(format!("{name} is not canonical"));
    }
    let literal = Regex::new(r"^[A-Za-z0-9._~-]+$").expect("valid path literal regex");
    let parameter =
        Regex::new(r"^\{[A-Za-z_][A-Za-z0-9_]*\}$").expect("valid path parameter regex");
    for segment in path.trim_start_matches('/').split('/') {
        if segment.is_empty() && path == "/" {
            continue;
        }
        if !literal.is_match(segment) && !parameter.is_match(segment) {
            return invalid(format!("{name} contains invalid segment {segment:?}"));
        }
        if matches!(segment, "." | "..") {
            return invalid(format!("{name} cannot contain dot segments"));
        }
    }
    Ok(())
}

fn is_reserved_contribution_path(path: &str) -> bool {
    RESERVED_CONTRIBUTION_PATH_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

fn validate_service_id(service_id: &str) -> ContributionResult<()> {
    let dns_label =
        Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$").expect("valid service id regex");
    if !dns_label.is_match(service_id) {
        return invalid("service_id must be a lowercase DNS label");
    }
    Ok(())
}

fn validate_stable_identifier(name: &str, value: &str) -> ContributionResult<()> {
    let stable =
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:-]*$").expect("valid stable identifier regex");
    if value.len() > MAX_IDENTIFIER_LEN || !stable.is_match(value) {
        return invalid(format!("{name} is not a stable identifier"));
    }
    Ok(())
}

fn validate_permission_key(value: &str) -> ContributionResult<()> {
    let permission = Regex::new(r"^[a-z0-9][a-z0-9.-]*$").expect("valid permission key regex");
    if value.len() > MAX_IDENTIFIER_LEN || !permission.is_match(value) || !value.contains('.') {
        return invalid("permission key must be a lowercase namespaced identifier");
    }
    Ok(())
}

fn validate_permission_scope_type(value: &str) -> ContributionResult<()> {
    let scope_type =
        Regex::new(r"^[a-z][a-z0-9]*(?:[._-][a-z0-9]+)*$").expect("valid scope type regex");
    if value.len() > MAX_IDENTIFIER_LEN || !scope_type.is_match(value) {
        return invalid("permission scope type must be a stable lowercase identifier");
    }
    Ok(())
}

fn validate_required_text(name: &str, value: &str, max: usize) -> ContributionResult<()> {
    if value.trim().is_empty() || value.len() > max {
        return invalid(format!("{name} must be non-empty and at most {max} bytes"));
    }
    Ok(())
}

fn validate_optional_text(name: &str, value: &str, max: usize) -> ContributionResult<()> {
    if value.len() > max {
        return invalid(format!("{name} must be at most {max} bytes"));
    }
    Ok(())
}

fn validate_sha256(name: &str, value: &str) -> ContributionResult<()> {
    if !is_canonical_sha256(value) {
        return invalid(format!("{name} must be sha256:<64 lowercase hex>"));
    }
    Ok(())
}

fn is_canonical_sha256(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_sha256<T: Serialize>(value: &T) -> ContributionResult<String> {
    let bytes = serde_json::to_vec(value).map_err(|error| {
        ContributionDomainError::Invalid(format!("cannot canonicalize contribution: {error}"))
    })?;
    let digest = Sha256::digest(bytes);
    Ok(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn invalid<T>(message: impl Into<String>) -> ContributionResult<T> {
    Err(ContributionDomainError::Invalid(message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn digest(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
    }

    fn surface(api_id: &str) -> ContributionApiSurfaceV1 {
        ContributionApiSurfaceV1 {
            api_id: api_id.to_string(),
            api_version: "1.0.0".to_string(),
            protocol: "http".to_string(),
            base_path: "/v1".to_string(),
        }
    }

    fn permission(service_id: &str) -> ContributionPermissionDefinitionV1 {
        ContributionPermissionDefinitionV1 {
            key: format!("{service_id}.read"),
            title: "Read".to_string(),
            description: String::new(),
        }
    }

    fn route(
        api_id: &str,
        operation_id: &str,
        method: ContributionHttpMethodV1,
        path: &str,
    ) -> ContributionOperationRouteV1 {
        ContributionOperationRouteV1 {
            audience: ContributionAudienceV1::User,
            method,
            path: path.to_string(),
            api_id: api_id.to_string(),
            operation_id: operation_id.to_string(),
            provider_path: path.to_string(),
            auth: ContributionRouteAuthV1::Required,
            permission: None,
            permission_scope: None,
        }
    }

    fn stage(
        scope: &str,
        deployment: &str,
        service: &str,
        generation: u64,
        previous: Option<String>,
        routes: Vec<ContributionOperationRouteV1>,
    ) -> ContributionRevisionV1 {
        ContributionRevisionV1::stage(
            scope,
            deployment,
            service,
            digest('a'),
            digest('b'),
            generation,
            previous,
            vec![surface(&format!("{service}.api"))],
            routes,
            vec![permission(service)],
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn revision_id_is_deterministic_across_child_input_order() {
        let api_id = "contest.api";
        let forward = stage(
            "default",
            "contest-blue",
            "contest",
            1,
            None,
            vec![
                route(
                    api_id,
                    "listContests",
                    ContributionHttpMethodV1::Get,
                    "/api/contests",
                ),
                route(
                    api_id,
                    "createContest",
                    ContributionHttpMethodV1::Post,
                    "/api/contests",
                ),
            ],
        );
        let reverse = stage(
            "default",
            "contest-blue",
            "contest",
            1,
            None,
            vec![
                route(
                    api_id,
                    "createContest",
                    ContributionHttpMethodV1::Post,
                    "/api/contests",
                ),
                route(
                    api_id,
                    "listContests",
                    ContributionHttpMethodV1::Get,
                    "/api/contests",
                ),
            ],
        );
        assert_eq!(forward.revision_id(), reverse.revision_id());
        assert_eq!(
            serde_json::to_vec(&forward).unwrap(),
            serde_json::to_vec(&reverse).unwrap()
        );
    }

    #[test]
    fn legacy_permission_namespace_is_limited_to_service_suffix() {
        let legacy = ContributionRevisionV1::stage(
            "default",
            "user-blue",
            "user-service",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![surface("user-service.api")],
            Vec::new(),
            vec![ContributionPermissionDefinitionV1 {
                key: "user.profile.read.self".to_string(),
                title: "Read own profile".to_string(),
                description: String::new(),
            }],
            Vec::new(),
            Vec::new(),
        );
        assert!(legacy.is_ok());

        let unrelated = ContributionRevisionV1::stage(
            "default",
            "user-blue",
            "user-service",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![surface("user-service.api")],
            Vec::new(),
            vec![ContributionPermissionDefinitionV1 {
                key: "account.profile.read".to_string(),
                title: "Read profile".to_string(),
                description: String::new(),
            }],
            Vec::new(),
            Vec::new(),
        );
        assert!(unrelated.is_err());
    }

    #[test]
    fn routes_and_frontends_may_reference_external_permissions_without_owning_them() {
        let mut admin_route = route(
            "auth-service.api",
            "listAuthUsers",
            ContributionHttpMethodV1::Get,
            "/auth/admin/users",
        );
        admin_route.audience = ContributionAudienceV1::Admin;
        admin_route.permission = Some("system.admin".to_string());
        admin_route.permission_scope = Some(ContributionPermissionScopeV1::system());

        let manifest_digest = digest('c');
        let bundle_digest = digest('d');
        let admin_frontend = ContributionFrontendModuleV1 {
            module_id: "auth.admin".to_string(),
            surface_id: "users".to_string(),
            route: "/auth/admin/users".to_string(),
            menu_label: "Users".to_string(),
            menu: true,
            order: 1,
            permission: Some("system.admin".to_string()),
            artifact: "frontend/auth-admin.mjs".to_string(),
            host_api_range: "^1".to_string(),
            manifest_reference: format!(
                "https://artifacts.example/sha256/{}/manifest.json",
                manifest_digest.trim_start_matches("sha256:")
            ),
            manifest_digest,
            bundle_reference: format!(
                "https://artifacts.example/sha256/{}/bundle.mjs",
                bundle_digest.trim_start_matches("sha256:")
            ),
            bundle_digest,
        };

        let revision = ContributionRevisionV1::stage(
            "default",
            "auth-blue",
            "auth-service",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![surface("auth-service.api")],
            vec![admin_route],
            vec![ContributionPermissionDefinitionV1 {
                key: "auth.control".to_string(),
                title: "Operate Auth".to_string(),
                description: String::new(),
            }],
            Vec::new(),
            vec![admin_frontend],
        )
        .unwrap();

        assert_eq!(revision.permission_definitions().len(), 1);
        assert_eq!(
            revision.operation_routes()[0].permission.as_deref(),
            Some("system.admin")
        );
        assert_eq!(
            revision.admin_frontend_modules()[0].permission.as_deref(),
            Some("system.admin")
        );
    }

    #[test]
    fn external_permission_references_still_require_valid_permission_syntax() {
        let mut admin_route = route(
            "auth-service.api",
            "listAuthUsers",
            ContributionHttpMethodV1::Get,
            "/auth/admin/users",
        );
        admin_route.permission = Some("System Admin".to_string());
        assert!(
            ContributionRevisionV1::stage(
                "default",
                "auth-blue",
                "auth-service",
                digest('a'),
                digest('b'),
                1,
                None,
                vec![surface("auth-service.api")],
                vec![admin_route],
                vec![ContributionPermissionDefinitionV1 {
                    key: "auth.control".to_string(),
                    title: "Operate Auth".to_string(),
                    description: String::new(),
                }],
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .to_string()
            .contains("lowercase namespaced identifier")
        );
    }

    #[test]
    fn deserialization_rejects_forged_immutable_identity() {
        let revision = stage("default", "contest-blue", "contest", 1, None, Vec::new());
        let mut value = serde_json::to_value(revision).unwrap();
        value["deployment_id"] = Value::String("contest-red".to_string());
        assert!(serde_json::from_value::<ContributionRevisionV1>(value).is_err());
    }

    #[test]
    fn cas_requires_matching_etag_and_exact_lineage() {
        let first_staged = stage("default", "contest-blue", "contest", 1, None, Vec::new());
        let first = first_staged.activate().unwrap();
        let first_head = compare_and_swap_contribution_head(None, None, &first).unwrap();

        let second_staged = stage(
            "default",
            "contest-green",
            "contest",
            2,
            Some(first.revision_id().to_string()),
            Vec::new(),
        );
        let second = second_staged.activate().unwrap();
        let conflict =
            compare_and_swap_contribution_head(Some(&first_head), Some(&digest('f')), &second)
                .unwrap_err();
        assert!(matches!(
            conflict,
            ContributionDomainError::CasConflict { .. }
        ));

        let second_head =
            compare_and_swap_contribution_head(Some(&first_head), Some(first_head.etag()), &second)
                .unwrap();
        assert_eq!(second_head.active_revision_id(), second.revision_id());
        assert_eq!(second_head.generation(), 2);
        assert_ne!(first_head.etag(), second_head.etag());
    }

    #[test]
    fn restore_cas_preserves_generation_and_prevents_aba() {
        let first = stage("default", "contest-blue", "contest", 1, None, Vec::new())
            .activate()
            .unwrap();
        let first_head = compare_and_swap_contribution_head(None, None, &first).unwrap();
        let second = stage(
            "default",
            "contest-green",
            "contest",
            2,
            Some(first.revision_id().to_string()),
            Vec::new(),
        )
        .activate()
        .unwrap();
        let second_head =
            compare_and_swap_contribution_head(Some(&first_head), Some(first_head.etag()), &second)
                .unwrap();
        let first_retired = first.retire().unwrap();

        let restored =
            restore_contribution_head(&second_head, second_head.etag(), &second, &first_retired)
                .unwrap();
        assert_eq!(restored.head.active_revision_id(), first.revision_id());
        assert_eq!(restored.head.generation(), 2);
        assert_ne!(restored.head.etag(), first_head.etag());
        assert_ne!(restored.head.etag(), second_head.etag());
        assert_eq!(
            restored.restored_revision.status(),
            ContributionRevisionStatusV1::Active
        );
        assert_eq!(
            restored.retired_candidate.status(),
            ContributionRevisionStatusV1::Retired
        );

        let third = stage(
            "default",
            "contest-red",
            "contest",
            3,
            Some(first.revision_id().to_string()),
            Vec::new(),
        )
        .activate()
        .unwrap();
        let third_head = compare_and_swap_contribution_head(
            Some(&restored.head),
            Some(restored.head.etag()),
            &third,
        )
        .unwrap();
        assert_eq!(third_head.generation(), 3);
    }

    #[test]
    fn restore_cas_rejects_stale_candidate_etag() {
        let first = stage("default", "contest-blue", "contest", 1, None, Vec::new())
            .activate()
            .unwrap();
        let first_head = compare_and_swap_contribution_head(None, None, &first).unwrap();
        let second = stage(
            "default",
            "contest-green",
            "contest",
            2,
            Some(first.revision_id().to_string()),
            Vec::new(),
        )
        .activate()
        .unwrap();
        let second_head =
            compare_and_swap_contribution_head(Some(&first_head), Some(first_head.etag()), &second)
                .unwrap();
        let error = restore_contribution_head(&second_head, first_head.etag(), &second, &first)
            .unwrap_err();
        assert!(matches!(error, ContributionDomainError::CasConflict { .. }));
    }

    #[test]
    fn initial_clear_publishes_empty_monotonic_tombstone_and_rejects_stale_etag() {
        let candidate = stage(
            "default",
            "contest-blue",
            "contest",
            1,
            None,
            vec![route(
                "contest.api",
                "listContests",
                ContributionHttpMethodV1::Get,
                "/api/contests",
            )],
        )
        .activate()
        .unwrap();
        let candidate_head = compare_and_swap_contribution_head(None, None, &candidate).unwrap();

        let stale =
            clear_initial_contribution_head(&candidate_head, &digest('f'), &candidate).unwrap_err();
        assert!(matches!(stale, ContributionDomainError::CasConflict { .. }));

        let cleared =
            clear_initial_contribution_head(&candidate_head, candidate_head.etag(), &candidate)
                .unwrap();
        assert_eq!(cleared.head.generation(), 2);
        assert_ne!(cleared.head.etag(), candidate_head.etag());
        assert_eq!(
            cleared.head.active_revision_id(),
            cleared.tombstone_revision.revision_id()
        );
        assert_eq!(
            cleared.retired_candidate.status(),
            ContributionRevisionStatusV1::Retired
        );
        assert!(cleared.tombstone_revision.api_surfaces().is_empty());
        assert!(cleared.tombstone_revision.operation_routes().is_empty());
        assert!(
            cleared
                .tombstone_revision
                .permission_definitions()
                .is_empty()
        );
        assert!(
            cleared
                .tombstone_revision
                .user_frontend_modules()
                .is_empty()
        );
        assert!(
            cleared
                .tombstone_revision
                .admin_frontend_modules()
                .is_empty()
        );

        let next = stage(
            "default",
            "contest-green",
            "contest",
            3,
            Some(cleared.tombstone_revision.revision_id().to_string()),
            Vec::new(),
        )
        .activate()
        .unwrap();
        assert!(
            compare_and_swap_contribution_head(
                Some(&cleared.head),
                Some(cleared.head.etag()),
                &next,
            )
            .is_ok()
        );
    }

    #[test]
    fn route_templates_detect_parameter_and_get_head_overlap() {
        assert!(route_templates_overlap("/api/users/{id}", "/api/users/me").unwrap());
        assert!(!route_templates_overlap("/api/users/{id}", "/api/roles/{id}/grants").unwrap());

        let result = ContributionRevisionV1::stage(
            "default",
            "user-blue",
            "user",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![surface("user.api")],
            vec![
                route(
                    "user.api",
                    "getUser",
                    ContributionHttpMethodV1::Get,
                    "/api/users/{id}",
                ),
                route(
                    "user.api",
                    "headMe",
                    ContributionHttpMethodV1::Head,
                    "/api/users/me",
                ),
            ],
            vec![permission("user")],
            Vec::new(),
            Vec::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn reserved_platform_paths_are_rejected() {
        let result = ContributionRevisionV1::stage(
            "default",
            "contest-blue",
            "contest",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![surface("contest.api")],
            vec![route(
                "contest.api",
                "metrics",
                ContributionHttpMethodV1::Get,
                "/metrics/private",
            )],
            vec![permission("contest")],
            Vec::new(),
            Vec::new(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn stage_collision_only_considers_live_same_scope_non_previous_revisions() {
        let owner = stage(
            "default",
            "user-blue",
            "user",
            1,
            None,
            vec![route(
                "user.api",
                "getUser",
                ContributionHttpMethodV1::Get,
                "/api/users/{id}",
            )],
        )
        .activate()
        .unwrap();
        let candidate = stage(
            "default",
            "profile-blue",
            "profile",
            1,
            None,
            vec![route(
                "profile.api",
                "getMe",
                ContributionHttpMethodV1::Head,
                "/api/users/me",
            )],
        );
        assert_eq!(
            stage_route_collisions(&candidate, std::slice::from_ref(&owner))
                .unwrap()
                .len(),
            1
        );

        let other_scope = stage(
            "tenant-b",
            "user-blue",
            "user",
            1,
            None,
            vec![route(
                "user.api",
                "getUser",
                ContributionHttpMethodV1::Get,
                "/api/users/{id}",
            )],
        )
        .activate()
        .unwrap();
        assert!(
            stage_route_collisions(&candidate, &[other_scope])
                .unwrap()
                .is_empty()
        );

        let upgrade = stage(
            "default",
            "user-green",
            "user",
            2,
            Some(owner.revision_id().to_string()),
            vec![route(
                "user.api",
                "getUser",
                ContributionHttpMethodV1::Get,
                "/api/users/{id}",
            )],
        );
        assert!(
            stage_route_collisions(&upgrade, &[owner])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn revision_state_machine_rejects_illegal_transitions_and_preserves_identity() {
        let staged = stage("default", "contest-blue", "contest", 1, None, Vec::new());
        let id = staged.revision_id().to_string();
        assert!(staged.retire().is_err());
        let active = staged.activate().unwrap();
        assert_eq!(active.revision_id(), id);
        assert!(active.abort().is_err());
        let retired = active.retire().unwrap();
        assert_eq!(retired.revision_id(), id);
        assert!(retired.activate().is_err());
    }

    #[test]
    fn assignments_are_not_revision_children_and_survive_upgrade_transitions() {
        let assignment = PermissionAssignmentV1 {
            assignment_id: "assignment-1".to_string(),
            scope_id: "default".to_string(),
            permission_key: "contest.read".to_string(),
            subject_kind: PermissionSubjectKindV1::Role,
            subject_id: "judge".to_string(),
        };
        assignment.validate().unwrap();
        let assignments = vec![assignment.clone()];

        let first = stage("default", "contest-blue", "contest", 1, None, Vec::new())
            .activate()
            .unwrap();
        let head = compare_and_swap_contribution_head(None, None, &first).unwrap();
        let second = stage(
            "default",
            "contest-green",
            "contest",
            2,
            Some(first.revision_id().to_string()),
            Vec::new(),
        )
        .activate()
        .unwrap();
        let _next_head =
            compare_and_swap_contribution_head(Some(&head), Some(head.etag()), &second).unwrap();
        let _retired = first.retire().unwrap();

        assert_eq!(assignments, vec![assignment]);
        let revision_json = serde_json::to_value(second).unwrap();
        assert!(revision_json.get("assignments").is_none());
    }

    #[test]
    fn permission_scope_must_reference_both_route_templates() {
        let mut scoped = route(
            "contest.api",
            "getContest",
            ContributionHttpMethodV1::Get,
            "/api/contests/{contestId}",
        );
        scoped.provider_path = "/contests/{contestId}".to_string();
        scoped.permission = Some("contest.read".to_string());
        scoped.permission_scope = Some(ContributionPermissionScopeV1::PathParameter(
            ContributionPathPermissionScopeV1 {
                scope_type: "contest".to_string(),
                path_parameter: "contestId".to_string(),
            },
        ));
        assert!(
            stage(
                "default",
                "contest-blue",
                "contest",
                1,
                None,
                vec![scoped.clone()]
            )
            .validate()
            .is_ok()
        );

        scoped.permission_scope = Some(ContributionPermissionScopeV1::PathParameter(
            ContributionPathPermissionScopeV1 {
                scope_type: "contest".to_string(),
                path_parameter: "missing".to_string(),
            },
        ));
        assert!(
            ContributionRevisionV1::stage(
                "default",
                "contest-blue",
                "contest",
                digest('a'),
                digest('b'),
                1,
                None,
                vec![surface("contest.api")],
                vec![scoped],
                vec![permission("contest")],
                Vec::new(),
                Vec::new(),
            )
            .unwrap_err()
            .to_string()
            .contains("must exist in external and provider templates")
        );
    }

    #[test]
    fn activation_and_receipt_states_are_explicit_and_checked() {
        let staged = stage("default", "contest-blue", "contest", 1, None, Vec::new());
        let activation = ContributionActivationV1::prepare("activation-1", &staged, None).unwrap();
        assert!(activation.succeed().is_err());
        let committed = activation.begin_commit().unwrap().succeed().unwrap();
        assert_eq!(committed.state(), ContributionActivationStateV1::Succeeded);
        let compensating = committed
            .begin_compensation(ContributionTerminationIntentV1::Failed)
            .unwrap();
        assert_eq!(
            compensating.finish_abort().unwrap().state(),
            ContributionActivationStateV1::Aborted
        );

        let receipt =
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Gateway, &staged)
                .unwrap();
        assert!(
            receipt
                .record(
                    ProjectionReceiptStateV1::Active,
                    Some(1),
                    None,
                    Some(digest('c')),
                    None,
                )
                .is_err()
        );
        let staged_receipt = receipt
            .record(
                ProjectionReceiptStateV1::Staged,
                None,
                Some(digest('c')),
                None,
                None,
            )
            .unwrap();
        let active_receipt = staged_receipt
            .record(
                ProjectionReceiptStateV1::Active,
                Some(1),
                Some(digest('c')),
                Some(digest('c')),
                None,
            )
            .unwrap();
        assert_eq!(active_receipt.state(), ProjectionReceiptStateV1::Active);
    }
}
