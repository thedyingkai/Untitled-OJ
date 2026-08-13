use crate::{
    ContributionRepository, ContributionRepositoryError, ContributionRepositoryResult,
    SqliteOrchestratorStore,
    contribution::{
        activation_metadata, deserialize_checked, not_found, receipt_metadata,
        revision_status_label, serialize_secret_free, subject_kind_label,
        validate_activation_bundle, validate_activation_revision, validate_activation_transition,
        validate_receipt_metadata, validate_receipt_transition, validate_revision_transition,
        validate_stage_against_state, validate_stage_bundle, validate_staged_revision,
    },
};
use orchestrator_core::{
    ContributionActivationV1, ContributionHeadV1, ContributionRevisionStatusV1,
    ContributionRevisionV1, PermissionAssignmentV1, ProjectionReceiptV1,
    clear_initial_contribution_head, compare_and_swap_contribution_head, restore_contribution_head,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

impl ContributionRepository for SqliteOrchestratorStore {
    fn insert_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        validate_staged_revision(revision)?;
        let payload = serialize_secret_free(revision)?;
        let generation = sqlite_generation(revision.generation())?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        if let Some(existing) = load_revision(&transaction, revision.revision_id())? {
            if existing == *revision {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return conflict("revision identity already contains different content");
        }
        transaction
            .execute(
                "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![revision.revision_id(), revision.scope_id(), revision.deployment_id(), revision.service_id(), revision.release_digest(), revision.contract_digest(), generation, revision.previous_revision_id(), revision_status_label(revision.status()), payload],
            )
            .map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn contribution_revision(
        &self,
        revision_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionRevisionV1>> {
        let connection = self.connection().map_err(persistence)?;
        load_revision(&connection, revision_id)
    }

    fn contribution_revisions(
        &self,
        scope_id: &str,
        service_id: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<ContributionRevisionV1>> {
        let connection = self.connection().map_err(persistence)?;
        let mut statement = connection
            .prepare("SELECT payload FROM orchestrator_contribution_revisions WHERE scope_id = ?1 AND (?2 IS NULL OR service_id = ?2) ORDER BY service_id, generation, revision_id")
            .map_err(sqlite_error)?;
        collect_payloads(
            statement
                .query_map(params![scope_id, service_id], payload_row)
                .map_err(sqlite_error)?,
        )
    }

    fn stage_contribution_bundle(
        &self,
        revision: &ContributionRevisionV1,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_stage_bundle(revision, activation, receipts)?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        if let Some(existing) = load_revision(&transaction, revision.revision_id())? {
            let existing_activation = load_activation(&transaction, &metadata.activation_id)?;
            let mut exact_receipts = true;
            for receipt in receipts {
                let receipt_metadata = receipt_metadata(receipt)?;
                if load_receipt(
                    &transaction,
                    &metadata.activation_id,
                    &receipt_metadata.target,
                )?
                .as_ref()
                    != Some(receipt)
                {
                    exact_receipts = false;
                    break;
                }
            }
            if existing == *revision
                && existing_activation.as_ref() == Some(activation)
                && count_receipts(&transaction, &metadata.activation_id)? == receipts.len()
                && exact_receipts
            {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return conflict("staged contribution bundle identity contains different content");
        }
        let head = load_head(&transaction, revision.scope_id(), revision.service_id())?;
        let mut statement = transaction
            .prepare("SELECT payload FROM orchestrator_contribution_revisions WHERE scope_id = ?1 ORDER BY service_id, generation, revision_id")
            .map_err(sqlite_error)?;
        let live: Vec<ContributionRevisionV1> = collect_payloads(
            statement
                .query_map([revision.scope_id()], payload_row)
                .map_err(sqlite_error)?,
        )?;
        drop(statement);
        validate_stage_against_state(revision, activation, head.as_ref(), live.iter())?;
        let revision_payload = serialize_secret_free(revision)?;
        transaction.execute(
            "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'STAGED', ?9)",
            params![revision.revision_id(), revision.scope_id(), revision.deployment_id(), revision.service_id(), revision.release_digest(), revision.contract_digest(), sqlite_generation(revision.generation())?, revision.previous_revision_id(), revision_payload],
        ).map_err(sqlite_error)?;
        let activation_payload = serialize_secret_free(activation)?;
        transaction.execute(
            "INSERT INTO orchestrator_contribution_activations(activation_id, scope_id, service_id, candidate_revision_id, previous_revision_id, expected_head_etag, state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'PREPARING', ?7)",
            params![metadata.activation_id, metadata.scope_id, metadata.service_id, metadata.candidate_revision_id, metadata.previous_revision_id, metadata.expected_head_etag, activation_payload],
        ).map_err(sqlite_error)?;
        for receipt in receipts {
            put_receipt(&transaction, &metadata, revision, receipt)?;
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn transition_contribution_revision(
        &self,
        revision: &ContributionRevisionV1,
    ) -> ContributionRepositoryResult<()> {
        revision.validate().map_err(invalid)?;
        let payload = serialize_secret_free(revision)?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let existing = required_revision(&transaction, revision.revision_id())?;
        validate_revision_transition(&existing, revision)?;
        if revision.status() == ContributionRevisionStatusV1::Retired
            && transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM orchestrator_contribution_heads WHERE active_revision_id = ?1)",
                    [revision.revision_id()],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sqlite_error)?
        {
            return conflict("cannot retire the revision currently referenced by a head");
        }
        let changed = transaction
            .execute(
                "UPDATE orchestrator_contribution_revisions SET status = ?2, payload = ?3, updated_at = unixepoch() WHERE revision_id = ?1 AND status = ?4",
                params![revision.revision_id(), revision_status_label(revision.status()), payload, revision_status_label(existing.status())],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            return conflict("revision transition raced with another writer");
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn contribution_head(
        &self,
        scope_id: &str,
        service_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionHeadV1>> {
        let connection = self.connection().map_err(persistence)?;
        load_head(&connection, scope_id, service_id)
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
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let current = load_head(
            &transaction,
            active_revision.scope_id(),
            active_revision.service_id(),
        )?;
        let next =
            compare_and_swap_contribution_head(current.as_ref(), expected_etag, active_revision)
                .map_err(|error| ContributionRepositoryError::Conflict(error.to_string()))?;
        let stored = required_revision(&transaction, active_revision.revision_id())?;
        validate_revision_transition(&stored, active_revision)?;
        let revision_payload = serialize_secret_free(active_revision)?;
        let changed = transaction
            .execute(
                "UPDATE orchestrator_contribution_revisions SET status = 'ACTIVE', payload = ?2, updated_at = unixepoch() WHERE revision_id = ?1 AND status = ?3",
                params![active_revision.revision_id(), revision_payload, revision_status_label(stored.status())],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            return conflict("candidate revision activation raced with another writer");
        }
        let head_payload = serialize_secret_free(&next)?;
        let generation = sqlite_generation(next.generation())?;
        match current {
            None => {
                transaction.execute(
                    "INSERT INTO orchestrator_contribution_heads(scope_id, service_id, active_revision_id, generation, etag, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![next.scope_id(), next.service_id(), next.active_revision_id(), generation, next.etag(), head_payload],
                ).map_err(sqlite_error)?;
            }
            Some(current) => {
                if transaction.execute(
                    "UPDATE orchestrator_contribution_heads SET active_revision_id = ?3, generation = ?4, etag = ?5, payload = ?6, updated_at = unixepoch() WHERE scope_id = ?1 AND service_id = ?2 AND etag = ?7",
                    params![next.scope_id(), next.service_id(), next.active_revision_id(), generation, next.etag(), head_payload, current.etag()],
                ).map_err(sqlite_error)? != 1 {
                    return conflict("contribution head CAS raced with another writer");
                }
            }
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(next)
    }

    fn restore_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
        previous_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let candidate = required_revision(&transaction, candidate_revision_id)?;
        let previous = required_revision(&transaction, previous_revision_id)?;
        let current = load_head(&transaction, candidate.scope_id(), candidate.service_id())?
            .ok_or_else(|| {
                ContributionRepositoryError::NotFound(format!(
                    "head {}/{}",
                    candidate.scope_id(),
                    candidate.service_id()
                ))
            })?;
        let restored =
            restore_contribution_head(&current, expected_candidate_etag, &candidate, &previous)
                .map_err(|error| ContributionRepositoryError::Conflict(error.to_string()))?;

        let candidate_payload = serialize_secret_free(&restored.retired_candidate)?;
        if transaction
            .execute(
                "UPDATE orchestrator_contribution_revisions SET status = 'RETIRED', payload = ?2, updated_at = unixepoch() WHERE revision_id = ?1 AND status = 'ACTIVE'",
                params![candidate_revision_id, candidate_payload],
            )
            .map_err(sqlite_error)?
            != 1
        {
            return conflict("candidate revision restore raced with another writer");
        }
        let previous_payload = serialize_secret_free(&restored.restored_revision)?;
        let previous_status = revision_status_label(previous.status());
        if transaction
            .execute(
                "UPDATE orchestrator_contribution_revisions SET status = 'ACTIVE', payload = ?2, updated_at = unixepoch() WHERE revision_id = ?1 AND status = ?3",
                params![previous_revision_id, previous_payload, previous_status],
            )
            .map_err(sqlite_error)?
            != 1
        {
            return conflict("previous revision restore raced with another writer");
        }
        let head_payload = serialize_secret_free(&restored.head)?;
        if transaction
            .execute(
                "UPDATE orchestrator_contribution_heads SET active_revision_id = ?3, generation = ?4, etag = ?5, payload = ?6, updated_at = unixepoch() WHERE scope_id = ?1 AND service_id = ?2 AND etag = ?7 AND active_revision_id = ?8",
                params![restored.head.scope_id(), restored.head.service_id(), restored.head.active_revision_id(), sqlite_generation(restored.head.generation())?, restored.head.etag(), head_payload, expected_candidate_etag, candidate_revision_id],
            )
            .map_err(sqlite_error)?
            != 1
        {
            return conflict("contribution head restore raced with another writer");
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(restored.head)
    }

    fn clear_initial_contribution_head(
        &self,
        expected_candidate_etag: &str,
        candidate_revision_id: &str,
    ) -> ContributionRepositoryResult<ContributionHeadV1> {
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let candidate = required_revision(&transaction, candidate_revision_id)?;
        let current = load_head(&transaction, candidate.scope_id(), candidate.service_id())?
            .ok_or_else(|| {
                ContributionRepositoryError::NotFound(format!(
                    "head {}/{}",
                    candidate.scope_id(),
                    candidate.service_id()
                ))
            })?;
        let cleared =
            clear_initial_contribution_head(&current, expected_candidate_etag, &candidate)
                .map_err(|error| ContributionRepositoryError::Conflict(error.to_string()))?;
        let tombstone_payload = serialize_secret_free(&cleared.tombstone_revision)?;
        transaction
            .execute(
                "INSERT INTO orchestrator_contribution_revisions(revision_id, scope_id, deployment_id, service_id, release_digest, contract_digest, generation, previous_revision_id, status, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ACTIVE', ?9)",
                params![cleared.tombstone_revision.revision_id(), cleared.tombstone_revision.scope_id(), cleared.tombstone_revision.deployment_id(), cleared.tombstone_revision.service_id(), cleared.tombstone_revision.release_digest(), cleared.tombstone_revision.contract_digest(), sqlite_generation(cleared.tombstone_revision.generation())?, cleared.tombstone_revision.previous_revision_id(), tombstone_payload],
            )
            .map_err(sqlite_error)?;
        let retired_payload = serialize_secret_free(&cleared.retired_candidate)?;
        if transaction
            .execute(
                "UPDATE orchestrator_contribution_revisions SET status = 'RETIRED', payload = ?2, updated_at = unixepoch() WHERE revision_id = ?1 AND status = 'ACTIVE'",
                params![candidate_revision_id, retired_payload],
            )
            .map_err(sqlite_error)?
            != 1
        {
            return conflict("initial candidate retirement raced with another writer");
        }
        let head_payload = serialize_secret_free(&cleared.head)?;
        if transaction
            .execute(
                "UPDATE orchestrator_contribution_heads SET active_revision_id = ?3, generation = ?4, etag = ?5, payload = ?6, updated_at = unixepoch() WHERE scope_id = ?1 AND service_id = ?2 AND etag = ?7 AND active_revision_id = ?8",
                params![cleared.head.scope_id(), cleared.head.service_id(), cleared.head.active_revision_id(), sqlite_generation(cleared.head.generation())?, cleared.head.etag(), head_payload, expected_candidate_etag, candidate_revision_id],
            )
            .map_err(sqlite_error)?
            != 1
        {
            return conflict("initial contribution head clear raced with another writer");
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(cleared.head)
    }

    fn put_contribution_activation_bundle(
        &self,
        activation: &ContributionActivationV1,
        receipts: &[ProjectionReceiptV1],
    ) -> ContributionRepositoryResult<()> {
        let metadata = validate_activation_bundle(activation, receipts)?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let revision = required_revision(&transaction, &metadata.candidate_revision_id)?;
        validate_activation_revision(&metadata, &revision)?;
        let existing = load_activation(&transaction, &metadata.activation_id)?;
        if let Some(existing) = existing.as_ref() {
            validate_activation_transition(existing, activation)?;
        }
        let payload = serialize_secret_free(activation)?;
        if existing.is_some() {
            transaction.execute(
                "UPDATE orchestrator_contribution_activations SET state = ?2, payload = ?3, updated_at = unixepoch() WHERE activation_id = ?1",
                params![metadata.activation_id, metadata.state, payload],
            ).map_err(sqlite_error)?;
        } else {
            transaction.execute(
                "INSERT INTO orchestrator_contribution_activations(activation_id, scope_id, service_id, candidate_revision_id, previous_revision_id, expected_head_etag, state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![metadata.activation_id, metadata.scope_id, metadata.service_id, metadata.candidate_revision_id, metadata.previous_revision_id, metadata.expected_head_etag, metadata.state, payload],
            ).map_err(sqlite_error)?;
        }
        for receipt in receipts {
            put_receipt(&transaction, &metadata, &revision, receipt)?;
        }
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn contribution_activation(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Option<ContributionActivationV1>> {
        let connection = self.connection().map_err(persistence)?;
        load_activation(&connection, activation_id)
    }

    fn contribution_activations(
        &self,
        scope_id: &str,
    ) -> ContributionRepositoryResult<Vec<ContributionActivationV1>> {
        let connection = self.connection().map_err(persistence)?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM orchestrator_contribution_activations WHERE scope_id = ?1 ORDER BY activation_id",
            )
            .map_err(sqlite_error)?;
        collect_payloads(
            statement
                .query_map([scope_id], payload_row)
                .map_err(sqlite_error)?,
        )
    }

    fn contribution_projection_receipts(
        &self,
        activation_id: &str,
    ) -> ContributionRepositoryResult<Vec<ProjectionReceiptV1>> {
        let connection = self.connection().map_err(persistence)?;
        let mut statement = connection
            .prepare("SELECT payload FROM orchestrator_contribution_projection_receipts WHERE activation_id = ?1 ORDER BY target")
            .map_err(sqlite_error)?;
        collect_payloads(
            statement
                .query_map([activation_id], payload_row)
                .map_err(sqlite_error)?,
        )
    }

    fn compare_and_swap_contribution_projection_receipt(
        &self,
        expected: &ProjectionReceiptV1,
        observed: &ProjectionReceiptV1,
    ) -> ContributionRepositoryResult<ProjectionReceiptV1> {
        validate_receipt_transition(expected, observed)?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let current = load_receipt(
            &transaction,
            expected.activation_id(),
            expected.target().as_str(),
        )?
        .ok_or_else(|| not_found("projection receipt"))?;
        if current == *observed {
            transaction.commit().map_err(sqlite_error)?;
            return Ok(current);
        }
        if current != *expected {
            return conflict("projection receipt changed before consumer acknowledgement");
        }
        let activation = load_activation(&transaction, expected.activation_id())?
            .ok_or_else(|| not_found("contribution activation"))?;
        let metadata = activation_metadata(&activation)?;
        let revision = required_revision(&transaction, expected.candidate_revision_id())?;
        put_receipt(&transaction, &metadata, &revision, observed)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(observed.clone())
    }

    fn insert_permission_assignment(
        &self,
        assignment: &PermissionAssignmentV1,
    ) -> ContributionRepositoryResult<()> {
        assignment.validate().map_err(invalid)?;
        let payload = serialize_secret_free(assignment)?;
        let mut connection = self.connection().map_err(persistence)?;
        let transaction = immediate(&mut connection)?;
        let existing = transaction
            .query_row(
                "SELECT payload FROM orchestrator_permission_assignments_v1 WHERE assignment_id = ?1",
                [&assignment.assignment_id],
                payload_row,
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(payload) = existing {
            let existing: PermissionAssignmentV1 = deserialize_checked(&payload)?;
            if existing == *assignment {
                transaction.commit().map_err(sqlite_error)?;
                return Ok(());
            }
            return conflict("permission assignment identity contains different content");
        }
        transaction.execute(
            "INSERT INTO orchestrator_permission_assignments_v1(assignment_id, scope_id, permission_key, subject_kind, subject_id, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![assignment.assignment_id, assignment.scope_id, assignment.permission_key, subject_kind_label(assignment.subject_kind), assignment.subject_id, payload],
        ).map_err(sqlite_error)?;
        transaction.commit().map_err(sqlite_error)?;
        Ok(())
    }

    fn delete_permission_assignment(
        &self,
        assignment_id: &str,
    ) -> ContributionRepositoryResult<bool> {
        let connection = self.connection().map_err(persistence)?;
        Ok(connection
            .execute(
                "DELETE FROM orchestrator_permission_assignments_v1 WHERE assignment_id = ?1",
                [assignment_id],
            )
            .map_err(sqlite_error)?
            > 0)
    }

    fn permission_assignments(
        &self,
        scope_id: &str,
        permission_key: Option<&str>,
    ) -> ContributionRepositoryResult<Vec<PermissionAssignmentV1>> {
        let connection = self.connection().map_err(persistence)?;
        let mut statement = connection
            .prepare("SELECT payload FROM orchestrator_permission_assignments_v1 WHERE scope_id = ?1 AND (?2 IS NULL OR permission_key = ?2) ORDER BY permission_key, subject_kind, subject_id, assignment_id")
            .map_err(sqlite_error)?;
        collect_payloads(
            statement
                .query_map(params![scope_id, permission_key], payload_row)
                .map_err(sqlite_error)?,
        )
    }
}

fn put_receipt(
    transaction: &rusqlite::Transaction<'_>,
    activation: &crate::contribution::ActivationMetadata,
    revision: &ContributionRevisionV1,
    receipt: &ProjectionReceiptV1,
) -> ContributionRepositoryResult<()> {
    let metadata = receipt_metadata(receipt)?;
    validate_receipt_metadata(activation, &metadata, revision)?;
    let existing = load_receipt(transaction, &metadata.activation_id, &metadata.target)?;
    if let Some(existing) = existing.as_ref() {
        validate_receipt_transition(existing, receipt)?;
    }
    let payload = serialize_secret_free(receipt)?;
    let generation = sqlite_generation(metadata.candidate_generation)?;
    let observed = metadata
        .observed_generation
        .map(sqlite_generation)
        .transpose()?;
    if existing.is_some() {
        transaction.execute(
            "UPDATE orchestrator_contribution_projection_receipts SET observed_generation = ?3, state = ?4, payload = ?5, updated_at = unixepoch() WHERE activation_id = ?1 AND target = ?2",
            params![metadata.activation_id, metadata.target, observed, metadata.state, payload],
        ).map_err(sqlite_error)?;
    } else {
        transaction.execute(
            "INSERT INTO orchestrator_contribution_projection_receipts(activation_id, target, candidate_revision_id, previous_revision_id, candidate_generation, observed_generation, state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![metadata.activation_id, metadata.target, metadata.candidate_revision_id, metadata.previous_revision_id, generation, observed, metadata.state, payload],
        ).map_err(sqlite_error)?;
    }
    Ok(())
}

fn load_revision(
    connection: &Connection,
    revision_id: &str,
) -> ContributionRepositoryResult<Option<ContributionRevisionV1>> {
    optional_payload(
        connection,
        "SELECT payload FROM orchestrator_contribution_revisions WHERE revision_id = ?1",
        revision_id,
    )?
    .map(|payload| deserialize_checked(&payload))
    .transpose()
}

fn required_revision(
    connection: &Connection,
    revision_id: &str,
) -> ContributionRepositoryResult<ContributionRevisionV1> {
    load_revision(connection, revision_id)?
        .ok_or_else(|| ContributionRepositoryError::NotFound(format!("revision {revision_id}")))
}

fn load_head(
    connection: &Connection,
    scope_id: &str,
    service_id: &str,
) -> ContributionRepositoryResult<Option<ContributionHeadV1>> {
    connection.query_row(
        "SELECT payload FROM orchestrator_contribution_heads WHERE scope_id = ?1 AND service_id = ?2",
        params![scope_id, service_id],
        payload_row,
    ).optional().map_err(sqlite_error)?
        .map(|payload| deserialize_checked(&payload)).transpose()
}

fn load_activation(
    connection: &Connection,
    activation_id: &str,
) -> ContributionRepositoryResult<Option<ContributionActivationV1>> {
    optional_payload(
        connection,
        "SELECT payload FROM orchestrator_contribution_activations WHERE activation_id = ?1",
        activation_id,
    )?
    .map(|payload| deserialize_checked(&payload))
    .transpose()
}

fn load_receipt(
    connection: &Connection,
    activation_id: &str,
    target: &str,
) -> ContributionRepositoryResult<Option<ProjectionReceiptV1>> {
    connection.query_row(
        "SELECT payload FROM orchestrator_contribution_projection_receipts WHERE activation_id = ?1 AND target = ?2",
        params![activation_id, target],
        payload_row,
    ).optional().map_err(sqlite_error)?
        .map(|payload| deserialize_checked(&payload)).transpose()
}

fn count_receipts(
    connection: &Connection,
    activation_id: &str,
) -> ContributionRepositoryResult<usize> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM orchestrator_contribution_projection_receipts WHERE activation_id = ?1",
            [activation_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sqlite_error)?;
    usize::try_from(count).map_err(|_| persistence("negative projection receipt count"))
}

fn optional_payload(
    connection: &Connection,
    sql: &str,
    value: &str,
) -> ContributionRepositoryResult<Option<String>> {
    connection
        .query_row(sql, [value], payload_row)
        .optional()
        .map_err(sqlite_error)
}

fn collect_payloads<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>>,
) -> ContributionRepositoryResult<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(sqlite_error)?
        .into_iter()
        .map(|payload| deserialize_checked(&payload))
        .collect()
}

fn payload_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<String> {
    row.get(0)
}

fn immediate(
    connection: &mut Connection,
) -> ContributionRepositoryResult<rusqlite::Transaction<'_>> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)
}

fn sqlite_generation(value: u64) -> ContributionRepositoryResult<i64> {
    i64::try_from(value).map_err(|_| invalid("generation exceeds SQLite INTEGER"))
}

fn invalid(error: impl std::fmt::Display) -> ContributionRepositoryError {
    ContributionRepositoryError::Invalid(error.to_string())
}

fn conflict<T>(error: impl std::fmt::Display) -> ContributionRepositoryResult<T> {
    Err(ContributionRepositoryError::Conflict(error.to_string()))
}

fn persistence(error: impl std::fmt::Display) -> ContributionRepositoryError {
    ContributionRepositoryError::Persistence(error.to_string())
}

fn sqlite_error(error: rusqlite::Error) -> ContributionRepositoryError {
    match &error {
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::ConstraintViolation
                    | rusqlite::ErrorCode::DatabaseBusy
                    | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            ContributionRepositoryError::Conflict(error.to_string())
        }
        _ => persistence(error),
    }
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
                title: "Read".into(),
                description: String::new(),
            }],
            Vec::new(),
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn sqlite_persists_head_bundle_and_assignment_independently() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(temp.path().join("contribution.db")).unwrap();
        let first = revision(1, None);
        store.insert_contribution_revision(&first).unwrap();
        let activation = ContributionActivationV1::prepare("activation-1", &first, None).unwrap();
        let receipts = [
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Auth, &first).unwrap(),
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Gateway, &first)
                .unwrap(),
        ];
        store
            .put_contribution_activation_bundle(&activation, &receipts)
            .unwrap();
        assert_eq!(
            store
                .contribution_projection_receipts("activation-1")
                .unwrap()
                .len(),
            2
        );
        let active = first.activate().unwrap();
        let head = store
            .compare_and_swap_contribution_head(None, &active)
            .unwrap();
        let assignment = PermissionAssignmentV1 {
            assignment_id: "assignment-1".into(),
            scope_id: "default".into(),
            permission_key: "contest.read".into(),
            subject_kind: PermissionSubjectKindV1::Role,
            subject_id: "judge".into(),
        };
        store.insert_permission_assignment(&assignment).unwrap();
        let second = revision(2, Some(first.revision_id().to_string()));
        store.insert_contribution_revision(&second).unwrap();
        let second_head = store
            .compare_and_swap_contribution_head(Some(head.etag()), &second.activate().unwrap())
            .unwrap();
        store
            .transition_contribution_revision(&active.retire().unwrap())
            .unwrap();
        let restored = store
            .restore_contribution_head(
                second_head.etag(),
                second.revision_id(),
                first.revision_id(),
            )
            .unwrap();
        assert_eq!(restored.active_revision_id(), first.revision_id());
        assert_eq!(restored.generation(), 2);
        assert_ne!(restored.etag(), head.etag());
        assert_eq!(
            store
                .permission_assignments("default", Some("contest.read"))
                .unwrap(),
            vec![assignment]
        );
    }

    #[test]
    fn sqlite_rejects_bundle_before_any_partial_write() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteOrchestratorStore::open(temp.path().join("contribution.db")).unwrap();
        let first = revision(1, None);
        store.insert_contribution_revision(&first).unwrap();
        let activation = ContributionActivationV1::prepare("activation-1", &first, None).unwrap();
        let receipt =
            ProjectionReceiptV1::pending("activation-1", ProjectionTargetV1::Gateway, &first)
                .unwrap();
        assert!(
            store
                .put_contribution_activation_bundle(&activation, &[receipt.clone(), receipt])
                .is_err()
        );
        assert!(
            store
                .contribution_activation("activation-1")
                .unwrap()
                .is_none()
        );
    }
}
