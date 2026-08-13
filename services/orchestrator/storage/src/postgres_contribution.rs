use crate::{
    ContributionRepository, ContributionRepositoryError, ContributionRepositoryResult,
    PostgresOrchestratorStore,
    contribution::{
        ActivationMetadata, deserialize_checked, receipt_metadata, revision_status_label,
        serialize_secret_free, subject_kind_label, validate_activation_bundle,
        validate_activation_revision, validate_activation_transition, validate_receipt_metadata,
        validate_receipt_transition, validate_revision_transition, validate_stage_against_state,
        validate_stage_bundle, validate_staged_revision,
    },
};
use orchestrator_core::{
    ContributionActivationV1, ContributionHeadV1, ContributionRevisionStatusV1,
    ContributionRevisionV1, PermissionAssignmentV1, ProjectionReceiptV1,
    clear_initial_contribution_head, compare_and_swap_contribution_head, restore_contribution_head,
};
use r2d2_postgres::postgres::{GenericClient, Transaction, error::SqlState};

impl ContributionRepository for PostgresOrchestratorStore {
    fn insert_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        validate_staged_revision(revision)?;
        let payload = serialize_secret_free(revision)?;
        let generation = pg_generation(revision.generation())?;
        self.pool()
            .with_transaction(|transaction| {
                if let Some(existing) = load_revision(transaction, revision.revision_id(), true)
                    .map_err(to_postgres_error)?
                {
                    if existing == *revision {
                        return Ok(());
                    }
                    return Err(crate::PostgresError::Conflict(
                        "revision identity already contains different content".to_string(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text::jsonb)",
                    &[&revision.revision_id(), &revision.scope_id(), &revision.deployment_id(), &revision.service_id(), &revision.release_digest(), &revision.contract_digest(), &generation, &revision.previous_revision_id(), &revision_status_label(revision.status()), &payload],
                )?;
                Ok(())
            })
            .map_err(pg_error)
    }

    fn contribution_revision(
        &self,
        revision_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionRevisionV1>> {
        self.pool()
            .with_client(|client| {
                load_revision(client, revision_id, false).map_err(to_postgres_error)
            })
            .map_err(pg_error)
    }

    fn contribution_revisions(
        &self,
        scope_id: &str,
        service_id: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<ContributionRevisionV1>> {
        self.pool()
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_contribution_revisions WHERE scope_id = $1 AND ($2::text IS NULL OR service_id = $2) ORDER BY service_id, generation, revision_id",
                        &[&scope_id, &service_id],
                    )?
                    .into_iter()
                    .map(|row| {
                        deserialize_checked(&row.get::<_, String>(0)).map_err(to_postgres_error)
                    })
                    .collect()
            })
            .map_err(pg_error)
    }

    fn stage_contribution_bundle(
        &self,
        revision: &ContributionRevisionV1,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_stage_bundle(revision, activation, receipts)?;
        self.pool()
            .with_transaction(|transaction| {
                // One transaction-scoped advisory lock serializes every stage
                // decision in a scope, including cross-service route claims.
                transaction.query_one(
                    "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                    &[&revision.scope_id()],
                )?;
                if let Some(existing) = load_revision(transaction, revision.revision_id(), true)
                    .map_err(to_postgres_error)?
                {
                    let existing_activation =
                        load_activation(transaction, &metadata.activation_id, true)
                            .map_err(to_postgres_error)?;
                    let mut exact_receipts = true;
                    for receipt in receipts {
                        let receipt_metadata =
                            receipt_metadata(receipt).map_err(to_postgres_error)?;
                        if load_receipt(
                            transaction,
                            &metadata.activation_id,
                            &receipt_metadata.target,
                            true,
                        )
                        .map_err(to_postgres_error)?
                        .as_ref()
                            != Some(receipt)
                        {
                            exact_receipts = false;
                            break;
                        }
                    }
                    if existing == *revision
                        && existing_activation.as_ref() == Some(activation)
                        && count_receipts(transaction, &metadata.activation_id)
                            .map_err(to_postgres_error)?
                            == receipts.len()
                        && exact_receipts
                    {
                        return Ok(());
                    }
                    return Err(crate::PostgresError::Conflict(
                        "staged contribution bundle identity contains different content"
                            .to_string(),
                    ));
                }
                let head = load_head(
                    transaction,
                    revision.scope_id(),
                    revision.service_id(),
                    true,
                )
                .map_err(to_postgres_error)?;
                let live = transaction
                    .query(
                        "SELECT payload::text FROM orchestrator_contribution_revisions WHERE scope_id = $1 ORDER BY service_id, generation, revision_id FOR UPDATE",
                        &[&revision.scope_id()],
                    )?
                    .into_iter()
                    .map(|row| {
                        deserialize_checked(&row.get::<_, String>(0))
                            .map_err(to_postgres_error)
                    })
                    .collect::<Result<Vec<ContributionRevisionV1>, crate::PostgresError>>()?;
                validate_stage_against_state(revision, activation, head.as_ref(), live.iter())
                    .map_err(to_postgres_error)?;
                let revision_payload =
                    serialize_secret_free(revision).map_err(to_postgres_error)?;
                let generation = pg_generation(revision.generation()).map_err(to_postgres_error)?;
                transaction.execute(
                    "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'STAGED', $9::text::jsonb)",
                    &[&revision.revision_id(), &revision.scope_id(), &revision.deployment_id(), &revision.service_id(), &revision.release_digest(), &revision.contract_digest(), &generation, &revision.previous_revision_id(), &revision_payload],
                )?;
                let activation_payload =
                    serialize_secret_free(activation).map_err(to_postgres_error)?;
                transaction.execute(
                    "INSERT INTO orchestrator_contribution_activations(activation_id, scope_id, service_id, candidate_revision_id, previous_revision_id, expected_head_etag, state, payload) VALUES ($1, $2, $3, $4, $5, $6, 'PREPARING', $7::text::jsonb)",
                    &[&metadata.activation_id, &metadata.scope_id, &metadata.service_id, &metadata.candidate_revision_id, &metadata.previous_revision_id, &metadata.expected_head_etag, &activation_payload],
                )?;
                let mut ordered = receipts.iter().collect::<Vec<_>>();
                ordered.sort_by_key(|receipt| receipt.target());
                for receipt in ordered {
                    put_receipt(transaction, &metadata, revision, receipt)
                        .map_err(to_postgres_error)?;
                }
                Ok(())
            })
            .map_err(pg_error)
    }

    fn transition_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        revision.validate().map_err(invalid)?;
        let payload = serialize_secret_free(revision)?;
        self.pool()
            .with_transaction(|transaction| {
                let existing = required_revision(transaction, revision.revision_id())
                    .map_err(to_postgres_error)?;
                validate_revision_transition(&existing, revision).map_err(to_postgres_error)?;
                if revision.status() == ContributionRevisionStatusV1::Retired
                    && transaction
                        .query_opt(
                            "SELECT active_revision_id FROM orchestrator_contribution_heads WHERE active_revision_id = $1 FOR UPDATE",
                            &[&revision.revision_id()],
                        )?
                        .is_some()
                {
                    return Err(crate::PostgresError::Conflict(
                        "cannot retire the revision currently referenced by a head".to_string(),
                    ));
                }
                let changed = transaction.execute(
                    "UPDATE orchestrator_contribution_revisions SET status = $2, payload = $3::text::jsonb, updated_at = clock_timestamp() WHERE revision_id = $1 AND status = $4",
                    &[&revision.revision_id(), &revision_status_label(revision.status()), &payload, &revision_status_label(existing.status())],
                )?;
                if changed != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "revision transition raced with another writer".to_string(),
                    ));
                }
                Ok(())
            })
            .map_err(pg_error)
    }

    fn contribution_head(
        &self,
        scope_id: &str,
        service_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionHeadV1>> {
        self.pool()
            .with_client(|client| {
                load_head(client, scope_id, service_id, false).map_err(to_postgres_error)
            })
            .map_err(pg_error)
    }

    fn compare_and_swap_contribution_head(
        &self,
        expected_etag: Option<&str>,
        active_revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        active_revision.validate().map_err(invalid)?;
        if active_revision.status() != ContributionRevisionStatusV1::Active {
            return conflict("head CAS requires an ACTIVE revision");
        }
        self.pool()
            .with_transaction(|transaction| {
                let current = load_head(
                    transaction,
                    active_revision.scope_id(),
                    active_revision.service_id(),
                    true,
                )
                .map_err(to_postgres_error)?;
                let next = compare_and_swap_contribution_head(
                    current.as_ref(),
                    expected_etag,
                    active_revision,
                )
                .map_err(|error| crate::PostgresError::Conflict(error.to_string()))?;
                let stored = required_revision(transaction, active_revision.revision_id())
                    .map_err(to_postgres_error)?;
                validate_revision_transition(&stored, active_revision)
                    .map_err(to_postgres_error)?;
                let revision_payload = serialize_secret_free(active_revision)
                    .map_err(to_postgres_error)?;
                let changed = transaction.execute(
                    "UPDATE orchestrator_contribution_revisions SET status = 'ACTIVE', payload = $2::text::jsonb, updated_at = clock_timestamp() WHERE revision_id = $1 AND status = $3",
                    &[&active_revision.revision_id(), &revision_payload, &revision_status_label(stored.status())],
                )?;
                if changed != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "candidate revision activation raced with another writer".to_string(),
                    ));
                }
                let payload = serialize_secret_free(&next).map_err(to_postgres_error)?;
                let generation = pg_generation(next.generation()).map_err(to_postgres_error)?;
                match current {
                    None => {
                        transaction.execute(
                            "INSERT INTO orchestrator_contribution_heads(scope_id, service_id, active_revision_id, generation, etag, payload) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)",
                            &[&next.scope_id(), &next.service_id(), &next.active_revision_id(), &generation, &next.etag(), &payload],
                        )?;
                    }
                    Some(current) => {
                        let changed = transaction.execute(
                            "UPDATE orchestrator_contribution_heads SET active_revision_id = $3, generation = $4, etag = $5, payload = $6::text::jsonb, updated_at = clock_timestamp() WHERE scope_id = $1 AND service_id = $2 AND etag = $7",
                            &[&next.scope_id(), &next.service_id(), &next.active_revision_id(), &generation, &next.etag(), &payload, &current.etag()],
                        )?;
                        if changed != 1 {
                            return Err(crate::PostgresError::Conflict(
                                "contribution head CAS raced with another writer".to_string(),
                            ));
                        }
                    }
                }
                Ok(next)
            })
            .map_err(pg_error)
    }

    fn restore_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        self.pool()
            .with_transaction(|transaction| {
                // Identity fields are immutable. This first read discovers the
                // head key; the rows are re-read under locks below.
                let preview = load_revision(transaction, candidate_revision_id, false)
                    .map_err(to_postgres_error)?
                    .ok_or_else(|| {
                        crate::PostgresError::Invariant(format!(
                            "revision {candidate_revision_id}"
                        ))
                    })?;
                let current = load_head(
                    transaction,
                    preview.scope_id(),
                    preview.service_id(),
                    true,
                )
                .map_err(to_postgres_error)?
                .ok_or_else(|| {
                    crate::PostgresError::Invariant(format!(
                        "head {}/{}",
                        preview.scope_id(),
                        preview.service_id()
                    ))
                })?;

                // All writers lock the head before revision status rows. Sort
                // revision locks so concurrent compensation cannot deadlock.
                let (first_id, second_id) = if candidate_revision_id < previous_revision_id {
                    (candidate_revision_id, previous_revision_id)
                } else {
                    (previous_revision_id, candidate_revision_id)
                };
                let first = required_revision(transaction, first_id).map_err(to_postgres_error)?;
                let second =
                    required_revision(transaction, second_id).map_err(to_postgres_error)?;
                let (candidate, previous) = if first.revision_id() == candidate_revision_id {
                    (first, second)
                } else {
                    (second, first)
                };
                let restored = restore_contribution_head(
                    &current,
                    expected_candidate_etag,
                    &candidate,
                    &previous,
                )
                .map_err(|error| crate::PostgresError::Conflict(error.to_string()))?;

                let candidate_payload = serialize_secret_free(&restored.retired_candidate)
                    .map_err(to_postgres_error)?;
                if transaction.execute(
                    "UPDATE orchestrator_contribution_revisions SET status = 'RETIRED', payload = $2::text::jsonb, updated_at = clock_timestamp() WHERE revision_id = $1 AND status = 'ACTIVE'",
                    &[&candidate_revision_id, &candidate_payload],
                )? != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "candidate revision restore raced with another writer".to_string(),
                    ));
                }
                let previous_payload = serialize_secret_free(&restored.restored_revision)
                    .map_err(to_postgres_error)?;
                let previous_status = revision_status_label(previous.status());
                if transaction.execute(
                    "UPDATE orchestrator_contribution_revisions SET status = 'ACTIVE', payload = $2::text::jsonb, updated_at = clock_timestamp() WHERE revision_id = $1 AND status = $3",
                    &[&previous_revision_id, &previous_payload, &previous_status],
                )? != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "previous revision restore raced with another writer".to_string(),
                    ));
                }
                let payload =
                    serialize_secret_free(&restored.head).map_err(to_postgres_error)?;
                let generation =
                    pg_generation(restored.head.generation()).map_err(to_postgres_error)?;
                if transaction.execute(
                    "UPDATE orchestrator_contribution_heads SET active_revision_id = $3, generation = $4, etag = $5, payload = $6::text::jsonb, updated_at = clock_timestamp() WHERE scope_id = $1 AND service_id = $2 AND etag = $7 AND active_revision_id = $8",
                    &[&restored.head.scope_id(), &restored.head.service_id(), &restored.head.active_revision_id(), &generation, &restored.head.etag(), &payload, &expected_candidate_etag, &candidate_revision_id],
                )? != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "contribution head restore raced with another writer".to_string(),
                    ));
                }
                Ok(restored.head)
            })
            .map_err(pg_error)
    }

    fn clear_initial_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        self.pool()
            .with_transaction(|transaction| {
                // Follow the repository-wide lock order: head before revision
                // status rows. The preview reads immutable identity only.
                let preview = load_revision(transaction, candidate_revision_id, false)
                    .map_err(to_postgres_error)?;
                let preview = preview.ok_or_else(|| {
                    crate::PostgresError::Invariant(format!(
                        "revision {candidate_revision_id}"
                    ))
                })?;
                let current = load_head(
                    transaction,
                    preview.scope_id(),
                    preview.service_id(),
                    true,
                )
                .map_err(to_postgres_error)?
                .ok_or_else(|| {
                    crate::PostgresError::Invariant(format!(
                        "head {}/{}",
                        preview.scope_id(),
                        preview.service_id()
                    ))
                })?;
                let candidate = required_revision(transaction, candidate_revision_id)
                    .map_err(to_postgres_error)?;
                let cleared = clear_initial_contribution_head(
                    &current,
                    expected_candidate_etag,
                    &candidate,
                )
                .map_err(|error| crate::PostgresError::Conflict(error.to_string()))?;
                let tombstone_payload = serialize_secret_free(&cleared.tombstone_revision)
                    .map_err(to_postgres_error)?;
                let tombstone_generation = pg_generation(cleared.tombstone_revision.generation())
                    .map_err(to_postgres_error)?;
                transaction.execute(
                    "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'ACTIVE', $9::text::jsonb)",
                    &[&cleared.tombstone_revision.revision_id(), &cleared.tombstone_revision.scope_id(), &cleared.tombstone_revision.deployment_id(), &cleared.tombstone_revision.service_id(), &cleared.tombstone_revision.release_digest(), &cleared.tombstone_revision.contract_digest(), &tombstone_generation, &cleared.tombstone_revision.previous_revision_id(), &tombstone_payload],
                )?;
                let retired_payload = serialize_secret_free(&cleared.retired_candidate)
                    .map_err(to_postgres_error)?;
                if transaction.execute(
                    "UPDATE orchestrator_contribution_revisions SET status = 'RETIRED', payload = $2::text::jsonb, updated_at = clock_timestamp() WHERE revision_id = $1 AND status = 'ACTIVE'",
                    &[&candidate_revision_id, &retired_payload],
                )? != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "initial candidate retirement raced with another writer".to_string(),
                    ));
                }
                let payload =
                    serialize_secret_free(&cleared.head).map_err(to_postgres_error)?;
                let generation =
                    pg_generation(cleared.head.generation()).map_err(to_postgres_error)?;
                if transaction.execute(
                    "UPDATE orchestrator_contribution_heads SET active_revision_id = $3, generation = $4, etag = $5, payload = $6::text::jsonb, updated_at = clock_timestamp() WHERE scope_id = $1 AND service_id = $2 AND etag = $7 AND active_revision_id = $8",
                    &[&cleared.head.scope_id(), &cleared.head.service_id(), &cleared.head.active_revision_id(), &generation, &cleared.head.etag(), &payload, &expected_candidate_etag, &candidate_revision_id],
                )? != 1 {
                    return Err(crate::PostgresError::Conflict(
                        "initial contribution head clear raced with another writer".to_string(),
                    ));
                }
                Ok(cleared.head)
            })
            .map_err(pg_error)
    }

    fn put_contribution_activation_bundle(
        &self,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_activation_bundle(activation, receipts)?;
        self.pool()
            .with_transaction(|transaction| {
                let revision = required_revision(transaction, &metadata.candidate_revision_id)
                    .map_err(to_postgres_error)?;
                validate_activation_revision(&metadata, &revision).map_err(to_postgres_error)?;
                let existing = load_activation(transaction, &metadata.activation_id, true)
                    .map_err(to_postgres_error)?;
                if let Some(existing) = existing.as_ref() {
                    validate_activation_transition(existing, activation)
                        .map_err(to_postgres_error)?;
                }
                let payload = serialize_secret_free(activation).map_err(to_postgres_error)?;
                if existing.is_some() {
                    transaction.execute(
                        "UPDATE orchestrator_contribution_activations SET state = $2, payload = $3::text::jsonb, updated_at = clock_timestamp() WHERE activation_id = $1",
                        &[&metadata.activation_id, &metadata.state, &payload],
                    )?;
                } else {
                    transaction.execute(
                        "INSERT INTO orchestrator_contribution_activations(activation_id, scope_id, service_id, candidate_revision_id, previous_revision_id, expected_head_etag, state, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::jsonb)",
                        &[&metadata.activation_id, &metadata.scope_id, &metadata.service_id, &metadata.candidate_revision_id, &metadata.previous_revision_id, &metadata.expected_head_etag, &metadata.state, &payload],
                    )?;
                }
                let mut ordered_receipts = receipts.iter().collect::<Vec<_>>();
                ordered_receipts.sort_by_key(|receipt| receipt.target());
                for receipt in ordered_receipts {
                    put_receipt(transaction, &metadata, &revision, receipt)
                        .map_err(to_postgres_error)?;
                }
                Ok(())
            })
            .map_err(pg_error)
    }

    fn contribution_activation(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionActivationV1>> {
        self.pool()
            .with_client(|client| {
                load_activation(client, activation_id, false).map_err(to_postgres_error)
            })
            .map_err(pg_error)
    }

    fn contribution_activations(
        &self,
        scope_id: &str,
    ) -> ContributionRepositoryResult<Vec<ContributionActivationV1>> {
        self.pool()
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_contribution_activations WHERE scope_id = $1 ORDER BY activation_id",
                        &[&scope_id],
                    )?
                    .into_iter()
                    .map(|row| {
                        deserialize_checked(&row.get::<_, String>(0)).map_err(to_postgres_error)
                    })
                    .collect()
            })
            .map_err(pg_error)
    }

    fn contribution_projection_receipts(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Vec<ProjectionReceiptV1>> {
        self.pool()
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_contribution_projection_receipts WHERE activation_id = $1 ORDER BY target",
                        &[&activation_id],
                    )?
                    .into_iter()
                    .map(|row| {
                        deserialize_checked(&row.get::<_, String>(0)).map_err(to_postgres_error)
                    })
                    .collect()
            })
            .map_err(pg_error)
    }

    fn compare_and_swap_contribution_projection_receipt(
        &self,
        expected: &ProjectionReceiptV1,
        observed: &ProjectionReceiptV1,
    ) -> ContributionRepositoryResult<ProjectionReceiptV1> {
        validate_receipt_transition(expected, observed)?;
        self.pool()
            .with_transaction(|transaction| {
                let current = load_receipt(
                    transaction,
                    expected.activation_id(),
                    expected.target().as_str(),
                    true,
                )
                .map_err(to_postgres_error)?
                .ok_or_else(|| {
                    crate::PostgresError::Invariant("projection receipt is missing".to_string())
                })?;
                if current == *observed {
                    return Ok(current);
                }
                if current != *expected {
                    return Err(crate::PostgresError::Conflict(
                        "projection receipt changed before consumer acknowledgement".to_string(),
                    ));
                }
                let activation = load_activation(transaction, expected.activation_id(), true)
                    .map_err(to_postgres_error)?
                    .ok_or_else(|| {
                        crate::PostgresError::Invariant(
                            "contribution activation is missing".to_string(),
                        )
                    })?;
                let metadata = crate::contribution::activation_metadata(&activation)
                    .map_err(to_postgres_error)?;
                let revision = required_revision(transaction, expected.candidate_revision_id())
                    .map_err(to_postgres_error)?;
                put_receipt(transaction, &metadata, &revision, observed)
                    .map_err(to_postgres_error)?;
                Ok(observed.clone())
            })
            .map_err(pg_error)
    }

    fn insert_permission_assignment(
        &self,
        assignment: &PermissionAssignmentV1,
    ) -> ContributionRepositoryResult<()> {
        assignment.validate().map_err(invalid)?;
        let payload = serialize_secret_free(assignment)?;
        self.pool()
            .with_transaction(|transaction| {
                if let Some(row) = transaction.query_opt(
                    "SELECT payload::text FROM orchestrator_permission_assignments_v1 WHERE assignment_id = $1 FOR UPDATE",
                    &[&assignment.assignment_id],
                )? {
                    let existing: PermissionAssignmentV1 =
                        deserialize_checked(&row.get::<_, String>(0))
                            .map_err(to_postgres_error)?;
                    if existing == *assignment {
                        return Ok(());
                    }
                    return Err(crate::PostgresError::Conflict(
                        "permission assignment identity contains different content".to_string(),
                    ));
                }
                transaction.execute(
                    "INSERT INTO orchestrator_permission_assignments_v1(assignment_id, scope_id, permission_key, subject_kind, subject_id, payload) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb)",
                    &[&assignment.assignment_id, &assignment.scope_id, &assignment.permission_key, &subject_kind_label(assignment.subject_kind), &assignment.subject_id, &payload],
                )?;
                Ok(())
            })
            .map_err(pg_error)
    }

    fn delete_permission_assignment(
        &self,
        assignment_id: &str,
    ) -> ContributionRepositoryResult<bool> {
        self.pool()
            .with_client(|client| {
                Ok(client.execute(
                    "DELETE FROM orchestrator_permission_assignments_v1 WHERE assignment_id = $1",
                    &[&assignment_id],
                )? > 0)
            })
            .map_err(pg_error)
    }

    fn permission_assignments(
        &self,
        scope_id: &str,
        permission_key: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<PermissionAssignmentV1>> {
        self.pool()
            .with_client(|client| {
                client
                    .query(
                        "SELECT payload::text FROM orchestrator_permission_assignments_v1 WHERE scope_id = $1 AND ($2::text IS NULL OR permission_key = $2) ORDER BY permission_key, subject_kind, subject_id, assignment_id",
                        &[&scope_id, &permission_key],
                    )?
                    .into_iter()
                    .map(|row| {
                        deserialize_checked(&row.get::<_, String>(0)).map_err(to_postgres_error)
                    })
                    .collect()
            })
            .map_err(pg_error)
    }
}

fn put_receipt(
    transaction: &mut Transaction<'_>,
    activation: &ActivationMetadata,
    revision: &ContributionRevisionV1,
    receipt: &ProjectionReceiptV1,
) -> ContributionRepositoryResult<()> {
    let metadata = receipt_metadata(receipt)?;
    validate_receipt_metadata(activation, &metadata, revision)?;
    let existing = load_receipt(transaction, &metadata.activation_id, &metadata.target, true)?;
    if let Some(existing) = existing.as_ref() {
        validate_receipt_transition(existing, receipt)?;
    }
    let payload = serialize_secret_free(receipt)?;
    let generation = pg_generation(metadata.candidate_generation)?;
    let observed = metadata
        .observed_generation
        .map(pg_generation)
        .transpose()?;
    if existing.is_some() {
        transaction
            .execute(
                "UPDATE orchestrator_contribution_projection_receipts SET observed_generation = $3, state = $4, payload = $5::text::jsonb, updated_at = clock_timestamp() WHERE activation_id = $1 AND target = $2",
                &[&metadata.activation_id, &metadata.target, &observed, &metadata.state, &payload],
            )
            .map_err(pg_driver_error)?;
    } else {
        transaction
            .execute(
                "INSERT INTO orchestrator_contribution_projection_receipts(activation_id, target, candidate_revision_id, previous_revision_id, candidate_generation, observed_generation, state, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::jsonb)",
                &[&metadata.activation_id, &metadata.target, &metadata.candidate_revision_id, &metadata.previous_revision_id, &generation, &observed, &metadata.state, &payload],
            )
            .map_err(pg_driver_error)?;
    }
    Ok(())
}

fn load_revision(
    client: &mut impl GenericClient,
    revision_id: &str,
    for_update: bool,
) -> ContributionRepositoryResult<Option<ContributionRevisionV1>> {
    let sql = if for_update {
        "SELECT payload::text FROM orchestrator_contribution_revisions WHERE revision_id = $1 FOR UPDATE"
    } else {
        "SELECT payload::text FROM orchestrator_contribution_revisions WHERE revision_id = $1"
    };
    client
        .query_opt(sql, &[&revision_id])
        .map_err(pg_driver_error)?
        .map(|row| deserialize_checked(&row.get::<_, String>(0)))
        .transpose()
}

fn required_revision(
    client: &mut impl GenericClient,
    revision_id: &str,
) -> ContributionRepositoryResult<ContributionRevisionV1> {
    load_revision(client, revision_id, true)?
        .ok_or_else(|| ContributionRepositoryError::NotFound(format!("revision {revision_id}")))
}

fn load_head(
    client: &mut impl GenericClient,
    scope_id: &str,
    service_id: &str,
    for_update: bool,
) -> ContributionRepositoryResult<Option<ContributionHeadV1>> {
    let sql = if for_update {
        "SELECT payload::text FROM orchestrator_contribution_heads WHERE scope_id = $1 AND service_id = $2 FOR UPDATE"
    } else {
        "SELECT payload::text FROM orchestrator_contribution_heads WHERE scope_id = $1 AND service_id = $2"
    };
    client
        .query_opt(sql, &[&scope_id, &service_id])
        .map_err(pg_driver_error)?
        .map(|row| deserialize_checked(&row.get::<_, String>(0)))
        .transpose()
}

fn load_activation(
    client: &mut impl GenericClient,
    activation_id: &str,
    for_update: bool,
) -> ContributionRepositoryResult<Option<ContributionActivationV1>> {
    let sql = if for_update {
        "SELECT payload::text FROM orchestrator_contribution_activations WHERE activation_id = $1 FOR UPDATE"
    } else {
        "SELECT payload::text FROM orchestrator_contribution_activations WHERE activation_id = $1"
    };
    client
        .query_opt(sql, &[&activation_id])
        .map_err(pg_driver_error)?
        .map(|row| deserialize_checked(&row.get::<_, String>(0)))
        .transpose()
}

fn load_receipt(
    client: &mut impl GenericClient,
    activation_id: &str,
    target: &str,
    for_update: bool,
) -> ContributionRepositoryResult<Option<ProjectionReceiptV1>> {
    let sql = if for_update {
        "SELECT payload::text FROM orchestrator_contribution_projection_receipts WHERE activation_id = $1 AND target = $2 FOR UPDATE"
    } else {
        "SELECT payload::text FROM orchestrator_contribution_projection_receipts WHERE activation_id = $1 AND target = $2"
    };
    client
        .query_opt(sql, &[&activation_id, &target])
        .map_err(pg_driver_error)?
        .map(|row| deserialize_checked(&row.get::<_, String>(0)))
        .transpose()
}

fn count_receipts(
    client: &mut impl GenericClient,
    activation_id: &str,
) -> ContributionRepositoryResult<usize> {
    let count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM orchestrator_contribution_projection_receipts WHERE activation_id = $1",
            &[&activation_id],
        )
        .map_err(pg_driver_error)?
        .get(0);
    usize::try_from(count).map_err(|_| {
        ContributionRepositoryError::Persistence("negative projection receipt count".to_string())
    })
}

fn pg_generation(value: u64) -> ContributionRepositoryResult<i64> {
    i64::try_from(value).map_err(|_| invalid("generation exceeds PostgreSQL BIGINT"))
}

fn invalid(error: impl std::fmt::Display) -> ContributionRepositoryError {
    ContributionRepositoryError::Invalid(error.to_string())
}

fn conflict<T>(error: impl std::fmt::Display) -> ContributionRepositoryResult<T> {
    Err(ContributionRepositoryError::Conflict(error.to_string()))
}

fn pg_driver_error(error: r2d2_postgres::postgres::Error) -> ContributionRepositoryError {
    if let Some(database) = error.as_db_error()
        && matches!(
            database.code(),
            &SqlState::UNIQUE_VIOLATION
                | &SqlState::FOREIGN_KEY_VIOLATION
                | &SqlState::CHECK_VIOLATION
                | &SqlState::T_R_SERIALIZATION_FAILURE
        )
    {
        return ContributionRepositoryError::Conflict(database.message().to_string());
    }
    ContributionRepositoryError::Persistence(error.to_string())
}

fn pg_error(error: crate::PostgresError) -> ContributionRepositoryError {
    match error {
        crate::PostgresError::Conflict(message) => ContributionRepositoryError::Conflict(message),
        crate::PostgresError::Invariant(message)
        | crate::PostgresError::Domain(message)
        | crate::PostgresError::InvalidConfiguration(message) => {
            ContributionRepositoryError::Invalid(message)
        }
        crate::PostgresError::Database(error) => pg_driver_error(error),
        other => ContributionRepositoryError::Persistence(other.to_string()),
    }
}

fn to_postgres_error(error: ContributionRepositoryError) -> crate::PostgresError {
    match error {
        ContributionRepositoryError::Invalid(message) => crate::PostgresError::Domain(message),
        ContributionRepositoryError::Conflict(message) => crate::PostgresError::Conflict(message),
        ContributionRepositoryError::NotFound(message) => crate::PostgresError::Invariant(message),
        ContributionRepositoryError::Persistence(message) => {
            crate::PostgresError::Invariant(message)
        }
    }
}
