use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_protocol::{
    RuntimeDesiredState, RuntimeInstance, RuntimeObservedState, STANDARD_RUNTIME_PROFILE_ID,
    STANDARD_RUNTIME_PROFILE_SHA256,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeManagementMode {
    #[default]
    Managed,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoredRuntimeInstance {
    pub node_id: String,
    pub instance: RuntimeInstance,
    #[serde(default)]
    pub management_mode: RuntimeManagementMode,
    #[serde(default)]
    pub endpoint: String,
    /// Protocol used by the control-plane health probe for an External
    /// deployment. Empty only for Managed or legacy External projections that
    /// have not yet produced formal probe evidence.
    #[serde(default)]
    pub external_probe_protocol: String,
    /// HTTP(S) readiness path used by the External probe. Empty for TCP.
    #[serde(default)]
    pub external_probe_health_path: String,
    #[serde(default)]
    pub last_observed_at_ms: i64,
    #[serde(default)]
    pub drift_reason: String,
    #[serde(default)]
    pub credential_expires_at_ms: i64,
    #[serde(default)]
    pub credential_last_success_at_ms: i64,
    #[serde(default)]
    pub credential_last_error: String,
    pub updated_at: String,
}

impl StoredRuntimeInstance {
    pub fn validate(&self) -> StorageResult<()> {
        for (name, value) in [
            ("node_id", self.node_id.as_str()),
            ("deployment_id", self.instance.deployment_id.as_str()),
            ("service_id", self.instance.service_id.as_str()),
            ("updated_at", self.updated_at.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(StorageError::Invariant(format!(
                    "runtime instance {name} must not be empty"
                )));
            }
        }
        match self.management_mode {
            RuntimeManagementMode::Managed if self.instance.release_version.trim().is_empty() => {
                return Err(StorageError::Invariant(
                    "managed runtime instance release_version must not be empty".to_string(),
                ));
            }
            RuntimeManagementMode::Managed if self.instance.container_id.trim().is_empty() => {
                return Err(StorageError::Invariant(
                    "managed runtime instance container_id must not be empty".to_string(),
                ));
            }
            RuntimeManagementMode::External if self.endpoint.trim().is_empty() => {
                return Err(StorageError::Invariant(
                    "external runtime instance endpoint must not be empty".to_string(),
                ));
            }
            RuntimeManagementMode::External if !self.instance.container_id.trim().is_empty() => {
                return Err(StorageError::Invariant(
                    "external runtime instance must not claim a Docker container_id".to_string(),
                ));
            }
            RuntimeManagementMode::Managed | RuntimeManagementMode::External => {}
        }
        if self.management_mode == RuntimeManagementMode::Managed
            && (!self.external_probe_protocol.is_empty()
                || !self.external_probe_health_path.is_empty())
        {
            return Err(StorageError::Invariant(
                "managed runtime instance must not contain External probe configuration"
                    .to_string(),
            ));
        }
        if !self.external_probe_protocol.is_empty() {
            let protocol = self.external_probe_protocol.as_bytes();
            if protocol.len() > 32
                || !protocol[0].is_ascii_lowercase()
                || !protocol.iter().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(*byte, b'+' | b'.' | b'-')
                })
            {
                return Err(StorageError::Invariant(
                    "external probe protocol must be a bounded lowercase protocol token"
                        .to_string(),
                ));
            }
        }
        if !self.external_probe_health_path.is_empty()
            && (self.external_probe_protocol != "http" && self.external_probe_protocol != "https"
                || !self.external_probe_health_path.starts_with('/'))
        {
            return Err(StorageError::Invariant(
                "external probe health_path is only valid for HTTP(S) and must begin with /"
                    .to_string(),
            ));
        }
        if self.management_mode == RuntimeManagementMode::Managed
            && self.instance.artifact_digest.trim().is_empty()
        {
            return Err(StorageError::Invariant(
                "managed runtime instance artifact_digest must not be empty".to_string(),
            ));
        }
        if !self.instance.artifact_digest.trim().is_empty() {
            validate_artifact_digest(&self.instance.artifact_digest)?;
        }
        if self.last_observed_at_ms < 0
            || self.credential_expires_at_ms < 0
            || self.credential_last_success_at_ms < 0
        {
            return Err(StorageError::Invariant(
                "runtime observation and credential timestamps must not be negative".to_string(),
            ));
        }
        for (name, value) in [
            ("drift_reason", self.drift_reason.as_str()),
            ("credential_last_error", self.credential_last_error.as_str()),
        ] {
            if value.len() > 512 || value.chars().any(char::is_control) {
                return Err(StorageError::Invariant(format!(
                    "runtime instance {name} must be bounded printable text"
                )));
            }
        }
        Ok(())
    }
}

fn validate_artifact_digest(value: &str) -> StorageResult<()> {
    let digest = value
        .strip_prefix("sha256:")
        .or_else(|| value.split_once("@sha256:").map(|(_, value)| value))
        .ok_or_else(|| {
            StorageError::Invariant(
                "runtime instance artifact_digest must be a sha256 digest".to_string(),
            )
        })?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StorageError::Invariant(
            "runtime instance artifact_digest must use 64 lowercase hex characters".to_string(),
        ));
    }
    Ok(())
}

impl SqliteOrchestratorStore {
    pub fn put_runtime_instance(&self, value: &StoredRuntimeInstance) -> StorageResult<()> {
        value.validate()?;
        self.connection()?.execute(
            "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(deployment_id) DO UPDATE SET node_id = excluded.node_id, service_id = excluded.service_id, desired_state = excluded.desired_state, observed_state = excluded.observed_state, payload = excluded.payload, updated_at = unixepoch()",
            params![
                value.instance.deployment_id,
                value.node_id,
                value.instance.service_id,
                desired_state(&value.instance.desired_state),
                observed_state(&value.instance.observed_state),
                serde_json::to_string(value)?,
            ],
        )?;
        Ok(())
    }

    pub fn runtime_instance(
        &self,
        deployment_id: &str,
    ) -> StorageResult<Option<StoredRuntimeInstance>> {
        let payload = self
            .connection()?
            .query_row(
                "SELECT payload FROM orchestrator_runtime_instances WHERE deployment_id = ?1",
                [deployment_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        payload.map(|payload| decode(&payload)).transpose()
    }

    pub fn runtime_instances(
        &self,
        node_id: Option<&str>,
    ) -> StorageResult<Vec<StoredRuntimeInstance>> {
        let connection = self.connection()?;
        let (sql, parameter) = match node_id {
            Some(node_id) => (
                "SELECT payload FROM orchestrator_runtime_instances WHERE node_id = ?1 ORDER BY service_id, deployment_id",
                Some(node_id),
            ),
            None => (
                "SELECT payload FROM orchestrator_runtime_instances ORDER BY node_id, service_id, deployment_id",
                None,
            ),
        };
        let mut statement = connection.prepare(sql)?;
        let payloads = if let Some(parameter) = parameter {
            statement
                .query_map([parameter], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        payloads
            .into_iter()
            .map(|payload| decode(&payload))
            .collect()
    }

    pub fn delete_runtime_instance(&self, deployment_id: &str) -> StorageResult<bool> {
        Ok(self.connection()?.execute(
            "DELETE FROM orchestrator_runtime_instances WHERE deployment_id = ?1",
            [deployment_id],
        )? > 0)
    }

    /// Atomically removes the replaced projection and publishes the healthy
    /// replacement. Repeating the same call is safe after a lost response.
    pub fn replace_runtime_instance(
        &self,
        replaced_deployment_id: &str,
        value: &StoredRuntimeInstance,
    ) -> StorageResult<()> {
        value.validate()?;
        if replaced_deployment_id.trim().is_empty()
            || replaced_deployment_id == value.instance.deployment_id
        {
            return Err(StorageError::Invariant(
                "runtime replacement requires distinct non-empty deployment ids".to_string(),
            ));
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM orchestrator_runtime_instances WHERE deployment_id = ?1",
            [replaced_deployment_id],
        )?;
        transaction.execute(
            "INSERT INTO orchestrator_runtime_instances(deployment_id, node_id, service_id, desired_state, observed_state, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(deployment_id) DO UPDATE SET node_id = excluded.node_id, service_id = excluded.service_id, desired_state = excluded.desired_state, observed_state = excluded.observed_state, payload = excluded.payload, updated_at = unixepoch()",
            params![
                value.instance.deployment_id,
                value.node_id,
                value.instance.service_id,
                desired_state(&value.instance.desired_state),
                observed_state(&value.instance.observed_state),
                serde_json::to_string(value)?,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn decode(payload: &str) -> StorageResult<StoredRuntimeInstance> {
    let value: StoredRuntimeInstance =
        serde_json::from_value(normalize_legacy_runtime_payload(payload)?)?;
    value.validate()?;
    Ok(value)
}

pub(crate) fn normalize_legacy_runtime_payload(
    payload: &str,
) -> serde_json::Result<serde_json::Value> {
    let mut value: serde_json::Value = serde_json::from_str(payload)?;
    let Some(contract) = value
        .pointer_mut("/instance/runtime_contract")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(value);
    };
    if contract.get("id").and_then(serde_json::Value::as_str) == Some("standard-v1") {
        contract.insert(
            "id".to_string(),
            serde_json::Value::String(STANDARD_RUNTIME_PROFILE_ID.to_string()),
        );
        contract.insert(
            "profile_sha256".to_string(),
            serde_json::Value::String(STANDARD_RUNTIME_PROFILE_SHA256.to_string()),
        );
    }
    Ok(value)
}

pub(crate) fn desired_state(state: &RuntimeDesiredState) -> &'static str {
    match state {
        RuntimeDesiredState::Running => "RUNNING",
        RuntimeDesiredState::Stopped => "STOPPED",
        RuntimeDesiredState::Removed => "REMOVED",
    }
}

pub(crate) fn observed_state(state: &RuntimeObservedState) -> &'static str {
    match state {
        RuntimeObservedState::Created => "CREATED",
        RuntimeObservedState::Running => "RUNNING",
        RuntimeObservedState::Stopped => "STOPPED",
        RuntimeObservedState::Exited => "EXITED",
        RuntimeObservedState::Missing => "MISSING",
        RuntimeObservedState::Unknown => "UNKNOWN",
    }
}
