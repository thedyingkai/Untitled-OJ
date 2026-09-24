//! Store boundary errors and infrastructure error mapping. Pure rules use StoreRuleError instead.
use crate::catalog_registry::CatalogRegistryError;
use crate::durable::DurableError;
use orchestrator_manager::store::StoreRuleError;
use orchestrator_manager::store::StoreRuleErrorKind;

#[derive(Debug)]
pub(crate) struct StoreError {
    pub(crate) status: u16,
    pub(crate) code: &'static str,
    pub(crate) detail: String,
    pub(crate) operation_id: Option<String>,
}

impl StoreError {
    pub(crate) fn new(status: u16, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code,
            detail: detail.into(),
            operation_id: None,
        }
    }
}

impl From<StoreRuleError> for StoreError {
    fn from(error: StoreRuleError) -> Self {
        let status = match error.kind {
            StoreRuleErrorKind::InvalidInput => 422,
            StoreRuleErrorKind::Conflict => 409,
        };
        Self::new(status, error.code, error.detail)
    }
}

pub(crate) fn catalog_registry_error(error: CatalogRegistryError) -> StoreError {
    StoreError::new(error.status(), error.code(), error.detail())
}

pub(crate) fn storage_error(error: DurableError) -> StoreError {
    let status = match &error {
        DurableError::Conflict(_) => 409,
        DurableError::Invariant(_) | DurableError::Domain(_) => 422,
        DurableError::Storage(_) => 500,
    };
    StoreError::new(status, "STORE_STORAGE_ERROR", error.to_string())
}

pub(crate) fn contribution_storage_error(
    error: orchestrator_storage::ContributionRepositoryError,
) -> StoreError {
    let status = match &error {
        orchestrator_storage::ContributionRepositoryError::Conflict(_) => 409,
        orchestrator_storage::ContributionRepositoryError::Invalid(_) => 422,
        orchestrator_storage::ContributionRepositoryError::NotFound(_) => 404,
        orchestrator_storage::ContributionRepositoryError::Persistence(_) => 500,
    };
    StoreError::new(
        status,
        "STORE_CONTRIBUTION_STORAGE_ERROR",
        error.to_string(),
    )
}

pub(crate) fn contribution_controller_error(
    error: crate::contribution_controller::ContributionControllerError,
) -> StoreError {
    let status = match &error {
        crate::contribution_controller::ContributionControllerError::Conflict(_) => 409,
        crate::contribution_controller::ContributionControllerError::NotFound(_) => 404,
        crate::contribution_controller::ContributionControllerError::NeedsAttention(_) => 409,
        crate::contribution_controller::ContributionControllerError::Retryable(_)
        | crate::contribution_controller::ContributionControllerError::RetryableCompensation(_) => {
            409
        }
        crate::contribution_controller::ContributionControllerError::Invalid(_) => 422,
        crate::contribution_controller::ContributionControllerError::Persistence(_) => 500,
    };
    StoreError::new(status, error.code(), error.to_string())
}

pub(crate) fn core_error(error: orchestrator_core::OrchestratorError) -> StoreError {
    StoreError::new(422, "STORE_RELEASE_INVALID", error.to_string())
}

pub(crate) fn operation_error(error: orchestrator_control_plane::OperationError) -> StoreError {
    let operation_id = match &error {
        orchestrator_control_plane::OperationError::NotFound(operation_id) => {
            Some(operation_id.clone())
        }
        _ => None,
    };
    StoreError {
        status: match &error {
            orchestrator_control_plane::OperationError::NotFound(_) => 404,
            orchestrator_control_plane::OperationError::InvalidPlan(_) => 422,
            orchestrator_control_plane::OperationError::IdempotencyConflict
            | orchestrator_control_plane::OperationError::InvalidTransition { .. } => 409,
            orchestrator_control_plane::OperationError::Store(_)
            | orchestrator_control_plane::OperationError::Job(_) => 500,
        },
        code: "STORE_OPERATION_ERROR",
        detail: error.to_string(),
        operation_id,
    }
}
