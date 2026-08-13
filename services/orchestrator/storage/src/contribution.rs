use orchestrator_core::{
    ContributionActivationStateV1, ContributionActivationV1, ContributionHeadV1,
    ContributionRevisionStatusV1, ContributionRevisionV1, PermissionAssignmentV1,
    ProjectionReceiptStateV1, ProjectionReceiptV1, ProjectionTargetV1,
    clear_initial_contribution_head, compare_and_swap_contribution_head, restore_contribution_head,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ContributionRepositoryError {
    #[error("invalid contribution repository request: {0}")]
    Invalid(String),
    #[error("contribution repository conflict: {0}")]
    Conflict(String),
    #[error("contribution repository record not found: {0}")]
    NotFound(String),
    #[error("contribution repository persistence failure: {0}")]
    Persistence(String),
}

pub type ContributionRepositoryResult<T> = std::result::Result<T, ContributionRepositoryError>;

/// Durable boundary for deployment-scoped contributions. Implementations must
/// make head CAS + revision activation atomic, and activation + receipt writes
/// atomic. Permission assignments are deliberately a separate aggregate.
pub trait ContributionRepository {
    fn insert_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()>;

    fn contribution_revision(
        &self,
        revision_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionRevisionV1>>;

    fn contribution_revisions(
        &self,
        scope_id: &str,
        service_id: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<ContributionRevisionV1>>;

    /// Atomically stages one immutable revision with its activation and
    /// projection receipts. Implementations serialize this mutation per scope
    /// and re-check live route collisions and head lineage inside the write
    /// transaction.
    fn stage_contribution_bundle(
        &self,
        revision: &ContributionRevisionV1,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()>;

    fn transition_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()>;

    fn contribution_head(
        &self,
        scope_id: &str,
        service_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionHeadV1>>;

    fn compare_and_swap_contribution_head(
        &self,
        expected_etag: Option<&str>,
        active_revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<ContributionHeadV1>;

    /// Atomically restores the previous active revision after a candidate head
    /// was published. The expected ETag is always the candidate head ETag;
    /// implementations must preserve the head generation to prevent ABA.
    fn restore_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1>;

    /// Atomically replaces a first active candidate with an empty tombstone
    /// successor, retiring the candidate without deleting monotonic head
    /// history. Used only by activation compensation when there is no prior
    /// revision to restore.
    fn clear_initial_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1>;

    fn put_contribution_activation_bundle(
        &self,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()>;

    fn contribution_activation(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionActivationV1>>;

    /// Lists durable activations in stable identity order. Snapshot compilers
    /// use this to publish consumer acknowledgement obligations; callers must
    /// still re-read an activation before mutating its receipts.
    fn contribution_activations(
        &self,
        scope_id: &str,
    ) -> ContributionRepositoryResult<Vec<ContributionActivationV1>>;

    fn contribution_projection_receipts(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Vec<ProjectionReceiptV1>>;

    /// Compare-and-swap one consumer observation without rewriting sibling
    /// receipts. Implementations must serialize by `(activation_id, target)`
    /// and reject an observation whose expected receipt no longer matches.
    fn compare_and_swap_contribution_projection_receipt(
        &self,
        expected: &ProjectionReceiptV1,
        observed: &ProjectionReceiptV1,
    ) -> ContributionRepositoryResult<ProjectionReceiptV1>;

    fn insert_permission_assignment(
        &self,
        assignment: &PermissionAssignmentV1,
    ) -> ContributionRepositoryResult<()>;

    fn delete_permission_assignment(
        &self,
        assignment_id: &str,
    ) -> ContributionRepositoryResult<bool>;

    fn permission_assignments(
        &self,
        scope_id: &str,
        permission_key: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<PermissionAssignmentV1>>;
}

#[derive(Debug, Default)]
struct MemoryContributionState {
    revisions: BTreeMap<String, ContributionRevisionV1>,
    heads: BTreeMap<(String, String), ContributionHeadV1>,
    activations: BTreeMap<String, ContributionActivationV1>,
    receipts: BTreeMap<(String, ProjectionTargetV1), ProjectionReceiptV1>,
    assignments: BTreeMap<String, PermissionAssignmentV1>,
}

/// Thread-safe reference implementation used by contract tests and callers
/// that need the same atomic aggregate semantics without a database.
#[derive(Debug, Default)]
pub struct MemoryContributionStore {
    state: Mutex<MemoryContributionState>,
}

impl MemoryContributionStore {
    fn state(&self) -> ContributionRepositoryResult<MutexGuard<'_, MemoryContributionState>> {
        self.state.lock().map_err(|_| {
            ContributionRepositoryError::Persistence(
                "memory contribution store lock is poisoned".to_string(),
            )
        })
    }
}

impl ContributionRepository for MemoryContributionStore {
    fn insert_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        validate_staged_revision(revision)?;
        let mut state = self.state()?;
        if let Some(existing) = state.revisions.get(revision.revision_id()) {
            return exact_or_conflict("revision", existing, revision);
        }
        if state.revisions.values().any(|existing| {
            existing.scope_id() == revision.scope_id()
                && existing.service_id() == revision.service_id()
                && existing.generation() == revision.generation()
        }) {
            return conflict(format!(
                "scope {} service {} generation {} already exists",
                revision.scope_id(),
                revision.service_id(),
                revision.generation()
            ));
        }
        state
            .revisions
            .insert(revision.revision_id().to_string(), revision.clone());
        Ok(())
    }

    fn contribution_revision(
        &self,
        revision_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionRevisionV1>> {
        Ok(self.state()?.revisions.get(revision_id).cloned())
    }

    fn contribution_revisions(
        &self,
        scope_id: &str,
        service_id: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<ContributionRevisionV1>> {
        let state = self.state()?;
        let mut revisions = state
            .revisions
            .values()
            .filter(|revision| {
                revision.scope_id() == scope_id
                    && service_id.is_none_or(|service| revision.service_id() == service)
            })
            .cloned()
            .collect::<Vec<_>>();
        revisions.sort_by(|left, right| {
            (left.service_id(), left.generation(), left.revision_id()).cmp(&(
                right.service_id(),
                right.generation(),
                right.revision_id(),
            ))
        });
        Ok(revisions)
    }

    fn stage_contribution_bundle(
        &self,
        revision: &ContributionRevisionV1,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_stage_bundle(revision, activation, receipts)?;
        let mut state = self.state()?;
        if let Some(existing) = state.revisions.get(revision.revision_id()) {
            if existing == revision
                && state.activations.get(&metadata.activation_id) == Some(activation)
                && state
                    .receipts
                    .keys()
                    .filter(|(activation_id, _)| activation_id == &metadata.activation_id)
                    .count()
                    == receipts.len()
                && receipts.iter().all(|receipt| {
                    state
                        .receipts
                        .get(&(metadata.activation_id.clone(), receipt.target()))
                        == Some(receipt)
                })
            {
                return Ok(());
            }
            return conflict("staged contribution bundle identity contains different content");
        }
        validate_stage_against_state(
            revision,
            activation,
            state.heads.get(&(
                revision.scope_id().to_string(),
                revision.service_id().to_string(),
            )),
            state.revisions.values(),
        )?;
        if state.revisions.values().any(|existing| {
            existing.scope_id() == revision.scope_id()
                && existing.service_id() == revision.service_id()
                && existing.generation() == revision.generation()
        }) {
            return conflict("scope/service contribution generation already exists");
        }
        state
            .revisions
            .insert(revision.revision_id().to_string(), revision.clone());
        state
            .activations
            .insert(metadata.activation_id.clone(), activation.clone());
        for receipt in receipts {
            state.receipts.insert(
                (metadata.activation_id.clone(), receipt.target()),
                receipt.clone(),
            );
        }
        Ok(())
    }

    fn transition_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        revision.validate().map_err(domain_error)?;
        let mut state = self.state()?;
        let existing = state
            .revisions
            .get(revision.revision_id())
            .ok_or_else(|| not_found(format!("revision {}", revision.revision_id())))?;
        validate_revision_transition(existing, revision)?;
        if revision.status() == ContributionRevisionStatusV1::Retired
            && state
                .heads
                .values()
                .any(|head| head.active_revision_id() == revision.revision_id())
        {
            return conflict("cannot retire the revision currently referenced by a head");
        }
        state
            .revisions
            .insert(revision.revision_id().to_string(), revision.clone());
        Ok(())
    }

    fn contribution_head(
        &self,
        scope_id: &str,
        service_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionHeadV1>> {
        Ok(self
            .state()?
            .heads
            .get(&(scope_id.to_string(), service_id.to_string()))
            .cloned())
    }

    fn compare_and_swap_contribution_head(
        &self,
        expected_etag: Option<&str>,
        active_revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        active_revision.validate().map_err(domain_error)?;
        let mut state = self.state()?;
        let key = (
            active_revision.scope_id().to_string(),
            active_revision.service_id().to_string(),
        );
        let current = state.heads.get(&key);
        let next = compare_and_swap_contribution_head(current, expected_etag, active_revision)
            .map_err(domain_error)?;
        let stored = state
            .revisions
            .get(active_revision.revision_id())
            .ok_or_else(|| not_found(format!("revision {}", active_revision.revision_id())))?;
        validate_revision_transition(stored, active_revision)?;
        state.revisions.insert(
            active_revision.revision_id().to_string(),
            active_revision.clone(),
        );
        state.heads.insert(key, next.clone());
        Ok(next)
    }

    fn restore_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        let mut state = self.state()?;
        let candidate = state
            .revisions
            .get(candidate_revision_id)
            .cloned()
            .ok_or_else(|| not_found(format!("revision {candidate_revision_id}")))?;
        let previous = state
            .revisions
            .get(previous_revision_id)
            .cloned()
            .ok_or_else(|| not_found(format!("revision {previous_revision_id}")))?;
        let key = (
            candidate.scope_id().to_string(),
            candidate.service_id().to_string(),
        );
        let current = state.heads.get(&key).ok_or_else(|| {
            not_found(format!(
                "head {}/{}",
                candidate.scope_id(),
                candidate.service_id()
            ))
        })?;
        let restored =
            restore_contribution_head(current, expected_candidate_etag, &candidate, &previous)
                .map_err(domain_error)?;
        state.revisions.insert(
            restored.retired_candidate.revision_id().to_string(),
            restored.retired_candidate,
        );
        state.revisions.insert(
            restored.restored_revision.revision_id().to_string(),
            restored.restored_revision,
        );
        state.heads.insert(key, restored.head.clone());
        Ok(restored.head)
    }

    fn clear_initial_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        let mut state = self.state()?;
        let candidate = state
            .revisions
            .get(candidate_revision_id)
            .cloned()
            .ok_or_else(|| not_found(format!("revision {candidate_revision_id}")))?;
        let key = (
            candidate.scope_id().to_string(),
            candidate.service_id().to_string(),
        );
        let current = state.heads.get(&key).ok_or_else(|| {
            not_found(format!(
                "head {}/{}",
                candidate.scope_id(),
                candidate.service_id()
            ))
        })?;
        let cleared = clear_initial_contribution_head(current, expected_candidate_etag, &candidate)
            .map_err(domain_error)?;
        if state.revisions.values().any(|existing| {
            existing.scope_id() == cleared.tombstone_revision.scope_id()
                && existing.service_id() == cleared.tombstone_revision.service_id()
                && existing.generation() == cleared.tombstone_revision.generation()
                && existing.revision_id() != cleared.tombstone_revision.revision_id()
        }) {
            return conflict("tombstone contribution generation already exists");
        }
        state.revisions.insert(
            cleared.retired_candidate.revision_id().to_string(),
            cleared.retired_candidate,
        );
        state.revisions.insert(
            cleared.tombstone_revision.revision_id().to_string(),
            cleared.tombstone_revision,
        );
        state.heads.insert(key, cleared.head.clone());
        Ok(cleared.head)
    }

    fn put_contribution_activation_bundle(
        &self,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_activation_bundle(activation, receipts)?;
        let mut state = self.state()?;
        let revision = state
            .revisions
            .get(&metadata.candidate_revision_id)
            .ok_or_else(|| not_found(format!("revision {}", metadata.candidate_revision_id)))?;
        validate_activation_revision(&metadata, revision)?;
        if let Some(existing) = state.activations.get(&metadata.activation_id) {
            validate_activation_transition(existing, activation)?;
        }
        for receipt in receipts {
            let receipt_metadata = receipt_metadata(receipt)?;
            if let Some(existing) = state
                .receipts
                .get(&(metadata.activation_id.clone(), receipt.target()))
            {
                validate_receipt_transition(existing, receipt)?;
            }
            validate_receipt_metadata(&metadata, &receipt_metadata, revision)?;
        }
        state
            .activations
            .insert(metadata.activation_id.clone(), activation.clone());
        for receipt in receipts {
            state.receipts.insert(
                (metadata.activation_id.clone(), receipt.target()),
                receipt.clone(),
            );
        }
        Ok(())
    }

    fn contribution_activation(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionActivationV1>> {
        Ok(self.state()?.activations.get(activation_id).cloned())
    }

    fn contribution_activations(
        &self,
        scope_id: &str,
    ) -> ContributionRepositoryResult<Vec<ContributionActivationV1>> {
        Ok(self
            .state()?
            .activations
            .values()
            .filter(|activation| activation.scope_id() == scope_id)
            .cloned()
            .collect())
    }

    fn contribution_projection_receipts(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Vec<ProjectionReceiptV1>> {
        Ok(self
            .state()?
            .receipts
            .iter()
            .filter(|((id, _), _)| id == activation_id)
            .map(|(_, receipt)| receipt.clone())
            .collect())
    }

    fn compare_and_swap_contribution_projection_receipt(
        &self,
        expected: &ProjectionReceiptV1,
        observed: &ProjectionReceiptV1,
    ) -> ContributionRepositoryResult<ProjectionReceiptV1> {
        validate_receipt_transition(expected, observed)?;
        let mut state = self.state()?;
        let key = (expected.activation_id().to_string(), expected.target());
        let current = state
            .receipts
            .get(&key)
            .ok_or_else(|| not_found("projection receipt"))?;
        if current == observed {
            return Ok(current.clone());
        }
        if current != expected {
            return conflict("projection receipt changed before consumer acknowledgement");
        }
        state.receipts.insert(key, observed.clone());
        Ok(observed.clone())
    }

    fn insert_permission_assignment(
        &self,
        assignment: &PermissionAssignmentV1,
    ) -> ContributionRepositoryResult<()> {
        assignment.validate().map_err(domain_error)?;
        ensure_secret_free_payload(assignment)?;
        let mut state = self.state()?;
        if let Some(existing) = state.assignments.get(&assignment.assignment_id) {
            return exact_or_conflict("permission assignment", existing, assignment);
        }
        if state.assignments.values().any(|existing| {
            existing.scope_id == assignment.scope_id
                && existing.permission_key == assignment.permission_key
                && existing.subject_kind == assignment.subject_kind
                && existing.subject_id == assignment.subject_id
        }) {
            return conflict("permission assignment tuple already exists");
        }
        state
            .assignments
            .insert(assignment.assignment_id.clone(), assignment.clone());
        Ok(())
    }

    fn delete_permission_assignment(
        &self,
        assignment_id: &str,
    ) -> ContributionRepositoryResult<bool> {
        Ok(self.state()?.assignments.remove(assignment_id).is_some())
    }

    fn permission_assignments(
        &self,
        scope_id: &str,
        permission_key: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<PermissionAssignmentV1>> {
        Ok(self
            .state()?
            .assignments
            .values()
            .filter(|assignment| {
                assignment.scope_id == scope_id
                    && permission_key
                        .is_none_or(|permission| assignment.permission_key == permission)
            })
            .cloned()
            .collect())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActivationMetadata {
    pub activation_id: String,
    pub scope_id: String,
    pub service_id: String,
    pub candidate_revision_id: String,
    pub previous_revision_id: Option<String>,
    pub expected_head_etag: Option<String>,
    pub state: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ReceiptMetadata {
    pub activation_id: String,
    pub target: String,
    pub candidate_revision_id: String,
    pub previous_revision_id: Option<String>,
    pub candidate_generation: u64,
    pub observed_generation: Option<u64>,
    pub state: String,
}

pub(crate) fn validate_staged_revision(
    revision: &ContributionRevisionV1,
) -> ContributionRepositoryResult<()> {
    revision.validate().map_err(domain_error)?;
    if revision.status() != ContributionRevisionStatusV1::Staged {
        return invalid("new contribution revisions must be STAGED");
    }
    ensure_secret_free_payload(revision)
}

pub(crate) fn validate_stage_bundle(
    revision: &ContributionRevisionV1,
    activation: &ContributionActivationV1,
    receipts: &[ProjectionReceiptV1],
) -> ContributionRepositoryResult<ActivationMetadata> {
    validate_staged_revision(revision)?;
    let metadata = validate_activation_bundle(activation, receipts)?;
    validate_activation_revision(&metadata, revision)?;
    if metadata.state != "PREPARING" {
        return invalid("staged contribution activation must be PREPARING");
    }
    for receipt in receipts {
        let receipt = receipt_metadata(receipt)?;
        validate_receipt_metadata(&metadata, &receipt, revision)?;
        if receipt.state != "PENDING" {
            return invalid("staged contribution receipts must be PENDING");
        }
    }
    Ok(metadata)
}

pub(crate) fn validate_stage_against_state<'a>(
    revision: &ContributionRevisionV1,
    activation: &ContributionActivationV1,
    head: Option<&ContributionHeadV1>,
    live: impl IntoIterator<Item = &'a ContributionRevisionV1>,
) -> ContributionRepositoryResult<()> {
    let live = live.into_iter().cloned().collect::<Vec<_>>();
    let historical_generation = live
        .iter()
        .filter(|existing| existing.service_id() == revision.service_id())
        .map(ContributionRevisionV1::generation)
        .max()
        .unwrap_or(0);
    let expected_generation = historical_generation.checked_add(1).ok_or_else(|| {
        ContributionRepositoryError::Conflict("contribution generation is exhausted".to_string())
    })?;
    match head {
        None if revision.generation() == expected_generation
            && revision.previous_revision_id().is_none() => {}
        Some(head)
            if revision.generation() == expected_generation
                && revision.generation() > head.generation()
                && revision.previous_revision_id() == Some(head.active_revision_id()) => {}
        _ => return conflict("candidate does not extend the current contribution head"),
    }
    if activation.expected_head_etag() != head.map(ContributionHeadV1::etag) {
        return conflict("activation expected_head_etag does not match the current head");
    }
    let collisions =
        orchestrator_core::stage_route_collisions(revision, &live).map_err(domain_error)?;
    if !collisions.is_empty() {
        return conflict(format!(
            "{} live route collision(s) in scope {}",
            collisions.len(),
            revision.scope_id()
        ));
    }
    Ok(())
}

pub(crate) fn validate_revision_transition(
    existing: &ContributionRevisionV1,
    candidate: &ContributionRevisionV1,
) -> ContributionRepositoryResult<()> {
    existing.validate().map_err(domain_error)?;
    candidate.validate().map_err(domain_error)?;
    if existing == candidate {
        return Ok(());
    }
    if existing.revision_id() != candidate.revision_id() {
        return conflict("revision identity cannot change");
    }
    let expected = match (existing.status(), candidate.status()) {
        (ContributionRevisionStatusV1::Staged, ContributionRevisionStatusV1::Active) => {
            existing.activate()
        }
        (ContributionRevisionStatusV1::Staged, ContributionRevisionStatusV1::Aborted) => {
            existing.abort()
        }
        (ContributionRevisionStatusV1::Active, ContributionRevisionStatusV1::Retired) => {
            existing.retire()
        }
        _ => return conflict("illegal or non-idempotent revision transition"),
    }
    .map_err(domain_error)?;
    if expected != *candidate {
        return conflict("revision immutable content changed during transition");
    }
    ensure_secret_free_payload(candidate)
}

pub(crate) fn activation_metadata(
    activation: &ContributionActivationV1,
) -> ContributionRepositoryResult<ActivationMetadata> {
    activation.validate().map_err(domain_error)?;
    ensure_secret_free_payload(activation)?;
    let value = serde_json::to_value(activation).map_err(json_error)?;
    Ok(ActivationMetadata {
        activation_id: required_string(&value, "activation_id")?,
        scope_id: required_string(&value, "scope_id")?,
        service_id: required_string(&value, "service_id")?,
        candidate_revision_id: required_string(&value, "candidate_revision_id")?,
        previous_revision_id: optional_string(&value, "previous_revision_id")?,
        expected_head_etag: optional_string(&value, "expected_head_etag")?,
        state: enum_json_label(&value, "state")?,
    })
}

pub(crate) fn receipt_metadata(
    receipt: &ProjectionReceiptV1,
) -> ContributionRepositoryResult<ReceiptMetadata> {
    receipt.validate().map_err(domain_error)?;
    ensure_secret_free_payload(receipt)?;
    let value = serde_json::to_value(receipt).map_err(json_error)?;
    Ok(ReceiptMetadata {
        activation_id: required_string(&value, "activation_id")?,
        target: enum_json_label(&value, "target")?,
        candidate_revision_id: required_string(&value, "candidate_revision_id")?,
        previous_revision_id: optional_string(&value, "previous_revision_id")?,
        candidate_generation: required_u64(&value, "candidate_generation")?,
        observed_generation: optional_u64(&value, "observed_generation")?,
        state: enum_json_label(&value, "state")?,
    })
}

pub(crate) fn validate_activation_bundle(
    activation: &ContributionActivationV1,
    receipts: &[ProjectionReceiptV1],
) -> ContributionRepositoryResult<ActivationMetadata> {
    let metadata = activation_metadata(activation)?;
    let mut targets = BTreeMap::new();
    for receipt in receipts {
        let receipt_metadata = receipt_metadata(receipt)?;
        if targets
            .insert(receipt_metadata.target.clone(), ())
            .is_some()
        {
            return invalid(format!(
                "activation bundle contains duplicate projection target {}",
                receipt_metadata.target
            ));
        }
        if receipt_metadata.activation_id != metadata.activation_id
            || receipt_metadata.candidate_revision_id != metadata.candidate_revision_id
            || receipt_metadata.previous_revision_id != metadata.previous_revision_id
        {
            return invalid("projection receipt identity does not match activation");
        }
    }
    Ok(metadata)
}

pub(crate) fn validate_activation_revision(
    activation: &ActivationMetadata,
    revision: &ContributionRevisionV1,
) -> ContributionRepositoryResult<()> {
    if activation.scope_id != revision.scope_id()
        || activation.service_id != revision.service_id()
        || activation.candidate_revision_id != revision.revision_id()
        || activation.previous_revision_id.as_deref() != revision.previous_revision_id()
    {
        return invalid("activation identity does not match candidate revision");
    }
    Ok(())
}

pub(crate) fn validate_receipt_metadata(
    activation: &ActivationMetadata,
    receipt: &ReceiptMetadata,
    revision: &ContributionRevisionV1,
) -> ContributionRepositoryResult<()> {
    if receipt.activation_id != activation.activation_id
        || receipt.candidate_revision_id != activation.candidate_revision_id
        || receipt.previous_revision_id != activation.previous_revision_id
        || receipt.candidate_generation != revision.generation()
    {
        return invalid("projection receipt does not match activation candidate revision");
    }
    Ok(())
}

pub(crate) fn validate_activation_transition(
    existing: &ContributionActivationV1,
    candidate: &ContributionActivationV1,
) -> ContributionRepositoryResult<()> {
    if existing == candidate {
        return Ok(());
    }
    let old = activation_metadata(existing)?;
    let new = activation_metadata(candidate)?;
    let mut old_value = serde_json::to_value(existing).map_err(json_error)?;
    let mut new_value = serde_json::to_value(candidate).map_err(json_error)?;
    remove_object_fields(&mut old_value, &["state", "termination_intent"])?;
    remove_object_fields(&mut new_value, &["state", "termination_intent"])?;
    if old_value != new_value || !activation_state_transition(existing.state(), candidate.state()) {
        return conflict(format!(
            "illegal activation transition {} -> {}",
            old.state, new.state
        ));
    }
    Ok(())
}

pub(crate) fn validate_receipt_transition(
    existing: &ProjectionReceiptV1,
    candidate: &ProjectionReceiptV1,
) -> ContributionRepositoryResult<()> {
    if existing == candidate {
        return Ok(());
    }
    let old = receipt_metadata(existing)?;
    let new = receipt_metadata(candidate)?;
    let mut old_value = serde_json::to_value(existing).map_err(json_error)?;
    let mut new_value = serde_json::to_value(candidate).map_err(json_error)?;
    remove_object_fields(
        &mut old_value,
        &[
            "state",
            "observed_generation",
            "staged_digest",
            "active_digest",
            "last_error",
        ],
    )?;
    remove_object_fields(
        &mut new_value,
        &[
            "state",
            "observed_generation",
            "staged_digest",
            "active_digest",
            "last_error",
        ],
    )?;
    if old_value != new_value || !receipt_state_transition(existing.state(), candidate.state()) {
        return conflict(format!(
            "illegal projection receipt transition {} -> {}",
            old.state, new.state
        ));
    }
    Ok(())
}

pub(crate) fn revision_status_label(status: ContributionRevisionStatusV1) -> &'static str {
    match status {
        ContributionRevisionStatusV1::Staged => "STAGED",
        ContributionRevisionStatusV1::Active => "ACTIVE",
        ContributionRevisionStatusV1::Retired => "RETIRED",
        ContributionRevisionStatusV1::Aborted => "ABORTED",
    }
}

pub(crate) fn subject_kind_label(kind: orchestrator_core::PermissionSubjectKindV1) -> &'static str {
    match kind {
        orchestrator_core::PermissionSubjectKindV1::User => "USER",
        orchestrator_core::PermissionSubjectKindV1::Role => "ROLE",
        orchestrator_core::PermissionSubjectKindV1::Service => "SERVICE",
    }
}

pub(crate) fn serialize_secret_free<T: Serialize>(
    value: &T,
) -> ContributionRepositoryResult<String> {
    ensure_secret_free_payload(value)?;
    serde_json::to_string(value).map_err(json_error)
}

pub(crate) fn deserialize_checked<T: serde::de::DeserializeOwned>(
    payload: &str,
) -> ContributionRepositoryResult<T> {
    serde_json::from_str(payload).map_err(json_error)
}

fn ensure_secret_free_payload<T: Serialize>(value: &T) -> ContributionRepositoryResult<()> {
    let value = serde_json::to_value(value).map_err(json_error)?;
    fn walk(value: &Value) -> ContributionRepositoryResult<()> {
        match value {
            Value::Object(object) => {
                for (key, value) in object {
                    let key = key.to_ascii_lowercase();
                    if [
                        "secret",
                        "password",
                        "credential",
                        "private_key",
                        "access_token",
                    ]
                    .iter()
                    .any(|forbidden| key.contains(forbidden))
                    {
                        return invalid(format!(
                            "field {key} is forbidden in contribution persistence"
                        ));
                    }
                    walk(value)?;
                }
                Ok(())
            }
            Value::Array(values) => {
                for value in values {
                    walk(value)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(&value)
}

fn exact_or_conflict<T: PartialEq>(
    kind: &str,
    existing: &T,
    candidate: &T,
) -> ContributionRepositoryResult<()> {
    if existing == candidate {
        Ok(())
    } else {
        conflict(format!(
            "{kind} identity already contains different content"
        ))
    }
}

fn activation_state_transition(
    old: ContributionActivationStateV1,
    new: ContributionActivationStateV1,
) -> bool {
    matches!(
        (old, new),
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
            ContributionActivationStateV1::Aborted | ContributionActivationStateV1::NeedsAttention
        )
    )
}

fn receipt_state_transition(old: ProjectionReceiptStateV1, new: ProjectionReceiptStateV1) -> bool {
    matches!(
        (old, new),
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
    )
}

fn required_string(value: &Value, field: &str) -> ContributionRepositoryResult<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ContributionRepositoryError::Invalid(format!("missing {field}")))
}

fn optional_string(value: &Value, field: &str) -> ContributionRepositoryResult<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => invalid(format!("{field} must be a string")),
    }
}

fn required_u64(value: &Value, field: &str) -> ContributionRepositoryResult<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| ContributionRepositoryError::Invalid(format!("missing {field}")))
}

fn optional_u64(value: &Value, field: &str) -> ContributionRepositoryResult<Option<u64>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| ContributionRepositoryError::Invalid(format!("invalid {field}"))),
    }
}

fn enum_json_label(value: &Value, field: &str) -> ContributionRepositoryResult<String> {
    required_string(value, field)
}

fn remove_object_fields(value: &mut Value, fields: &[&str]) -> ContributionRepositoryResult<()> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| ContributionRepositoryError::Invalid("payload is not an object".into()))?;
    for field in fields {
        object.remove(*field);
    }
    Ok(())
}

pub(crate) fn domain_error(error: impl std::fmt::Display) -> ContributionRepositoryError {
    ContributionRepositoryError::Invalid(error.to_string())
}

pub(crate) fn json_error(error: serde_json::Error) -> ContributionRepositoryError {
    ContributionRepositoryError::Persistence(format!("JSON payload: {error}"))
}

pub(crate) fn invalid<T>(message: impl Into<String>) -> ContributionRepositoryResult<T> {
    Err(ContributionRepositoryError::Invalid(message.into()))
}

pub(crate) fn conflict<T>(message: impl Into<String>) -> ContributionRepositoryResult<T> {
    Err(ContributionRepositoryError::Conflict(message.into()))
}

pub(crate) fn not_found(message: impl Into<String>) -> ContributionRepositoryError {
    ContributionRepositoryError::NotFound(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::{
        ContributionApiSurfaceV1, ContributionPermissionDefinitionV1, PermissionSubjectKindV1,
        ProjectionTargetV1,
    };

    fn digest(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
    }

    fn revision(generation: u64, previous: Option<String>) -> ContributionRevisionV1 {
        ContributionRevisionV1::stage(
            "default",
            format!("contest-{generation}"),
            "contest",
            digest('a'),
            digest('b'),
            generation,
            previous,
            vec![ContributionApiSurfaceV1 {
                api_id: "contest.api".into(),
                api_version: "1.0.0".into(),
                protocol: "http".into(),
                base_path: "/v1".into(),
            }],
            Vec::new(),
            vec![ContributionPermissionDefinitionV1 {
                key: "contest.read".into(),
                title: "Read contests".into(),
                description: String::new(),
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    fn repository_contract(repository: &impl ContributionRepository) {
        let first = revision(1, None);
        repository.insert_contribution_revision(&first).unwrap();
        repository.insert_contribution_revision(&first).unwrap();
        let activation = ContributionActivationV1::prepare("activation-1", &first, None).unwrap();
        let receipts = vec![
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Auth, &first).unwrap(),
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Gateway, &first)
                .unwrap(),
        ];
        repository
            .put_contribution_activation_bundle(&activation, &receipts)
            .unwrap();

        let first_active = first.activate().unwrap();
        let head = repository
            .compare_and_swap_contribution_head(None, &first_active)
            .unwrap();
        assert!(
            repository
                .compare_and_swap_contribution_head(Some(&digest('f')), &first_active)
                .is_err()
        );

        let assignment = PermissionAssignmentV1 {
            assignment_id: "assignment-1".into(),
            scope_id: "default".into(),
            permission_key: "contest.read".into(),
            subject_kind: PermissionSubjectKindV1::Role,
            subject_id: "judge".into(),
        };
        repository
            .insert_permission_assignment(&assignment)
            .unwrap();

        let second = revision(2, Some(first.revision_id().to_string()));
        repository.insert_contribution_revision(&second).unwrap();
        let second_active = second.activate().unwrap();
        let next = repository
            .compare_and_swap_contribution_head(Some(head.etag()), &second_active)
            .unwrap();
        assert_eq!(next.generation(), 2);
        repository
            .transition_contribution_revision(&first_active.retire().unwrap())
            .unwrap();
        let restored = repository
            .restore_contribution_head(next.etag(), second.revision_id(), first.revision_id())
            .unwrap();
        assert_eq!(restored.active_revision_id(), first.revision_id());
        assert_eq!(restored.generation(), 2);
        assert_ne!(restored.etag(), head.etag());
        assert_eq!(
            repository
                .contribution_revision(second.revision_id())
                .unwrap()
                .unwrap()
                .status(),
            ContributionRevisionStatusV1::Retired
        );
        assert_eq!(
            repository
                .contribution_revision(first.revision_id())
                .unwrap()
                .unwrap()
                .status(),
            ContributionRevisionStatusV1::Active
        );
        assert_eq!(
            repository
                .permission_assignments("default", Some("contest.read"))
                .unwrap(),
            vec![assignment]
        );
    }

    #[test]
    fn memory_repository_obeys_atomic_domain_contract() {
        repository_contract(&MemoryContributionStore::default());
    }

    #[test]
    fn activation_bundle_rejects_mixed_receipts_without_partial_write() {
        let store = MemoryContributionStore::default();
        let candidate = revision(1, None);
        store.insert_contribution_revision(&candidate).unwrap();
        let activation =
            ContributionActivationV1::prepare("activation-1", &candidate, None).unwrap();
        let unrelated = ContributionRevisionV1::stage(
            "default",
            "user-1",
            "user",
            digest('c'),
            digest('d'),
            1,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let receipt =
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Gateway, &unrelated)
                .unwrap();
        assert!(
            store
                .put_contribution_activation_bundle(&activation, &[receipt])
                .is_err()
        );
        assert!(
            store
                .contribution_activation("activation-1")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn atomic_stage_bundle_rejects_collision_without_orphan_records() {
        let store = MemoryContributionStore::default();
        let owner = ContributionRevisionV1::stage(
            "default",
            "owner-1",
            "owner",
            digest('a'),
            digest('b'),
            1,
            None,
            vec![ContributionApiSurfaceV1 {
                api_id: "owner.api".into(),
                api_version: "1.0.0".into(),
                protocol: "http".into(),
                base_path: "/api".into(),
            }],
            vec![orchestrator_core::ContributionOperationRouteV1 {
                audience: orchestrator_core::ContributionAudienceV1::User,
                method: orchestrator_core::ContributionHttpMethodV1::Get,
                path: "/api/items/{id}".into(),
                api_id: "owner.api".into(),
                operation_id: "owner.get".into(),
                provider_path: "/api/items/{id}".into(),
                auth: orchestrator_core::ContributionRouteAuthV1::Required,
                permission: None,
                permission_scope: None,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        store.insert_contribution_revision(&owner).unwrap();
        store
            .compare_and_swap_contribution_head(None, &owner.activate().unwrap())
            .unwrap();

        let candidate = ContributionRevisionV1::stage(
            "default",
            "viewer-1",
            "viewer",
            digest('c'),
            digest('d'),
            1,
            None,
            vec![ContributionApiSurfaceV1 {
                api_id: "viewer.api".into(),
                api_version: "1.0.0".into(),
                protocol: "http".into(),
                base_path: "/api".into(),
            }],
            vec![orchestrator_core::ContributionOperationRouteV1 {
                audience: orchestrator_core::ContributionAudienceV1::User,
                method: orchestrator_core::ContributionHttpMethodV1::Head,
                path: "/api/items/me".into(),
                api_id: "viewer.api".into(),
                operation_id: "viewer.me".into(),
                provider_path: "/api/items/me".into(),
                auth: orchestrator_core::ContributionRouteAuthV1::Required,
                permission: None,
                permission_scope: None,
            }],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .unwrap();
        let activation =
            ContributionActivationV1::prepare("activation-viewer", &candidate, None).unwrap();
        let receipts = ProjectionTargetV1::ALL
            .into_iter()
            .map(|target| ProjectionReceiptV1::pending("activation-viewer", target, &candidate))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            store
                .stage_contribution_bundle(&candidate, &activation, &receipts)
                .is_err()
        );
        assert!(
            store
                .contribution_revision(candidate.revision_id())
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .contribution_activation("activation-viewer")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .contribution_projection_receipts("activation-viewer")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn initial_head_clear_is_monotonic_and_empty() {
        let store = MemoryContributionStore::default();
        let first = revision(1, None);
        store.insert_contribution_revision(&first).unwrap();
        let head = store
            .compare_and_swap_contribution_head(None, &first.activate().unwrap())
            .unwrap();
        let cleared = store
            .clear_initial_contribution_head(head.etag(), first.revision_id())
            .unwrap();
        let tombstone = store
            .contribution_revision(cleared.active_revision_id())
            .unwrap()
            .unwrap();
        assert_eq!(cleared.generation(), 2);
        assert_ne!(cleared.etag(), head.etag());
        assert!(tombstone.api_surfaces().is_empty());
        assert!(tombstone.operation_routes().is_empty());
        assert!(tombstone.permission_definitions().is_empty());
    }
}
