use crate::{SqliteOrchestratorStore, StorageError, StorageResult};
use orchestrator_runtime::{RuntimeDesiredState, RuntimeInstance, RuntimeObservedState};
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
            && self.instance.artifact_digest.trim().is_empty()
        {
            return Err(StorageError::Invariant(
                "managed runtime instance artifact_digest must not be empty".to_string(),
            ));
        }
        if !self.instance.artifact_digest.trim().is_empty() {
            validate_artifact_digest(&self.instance.artifact_digest)?;
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
    let value: StoredRuntimeInstance = serde_json::from_str(payload)?;
    value.validate()?;
    Ok(value)
}

fn desired_state(state: &RuntimeDesiredState) -> &'static str {
    match state {
        RuntimeDesiredState::Running => "RUNNING",
        RuntimeDesiredState::Stopped => "STOPPED",
        RuntimeDesiredState::Removed => "REMOVED",
    }
}

fn observed_state(state: &RuntimeObservedState) -> &'static str {
    match state {
        RuntimeObservedState::Created => "CREATED",
        RuntimeObservedState::Running => "RUNNING",
        RuntimeObservedState::Stopped => "STOPPED",
        RuntimeObservedState::Exited => "EXITED",
        RuntimeObservedState::Missing => "MISSING",
        RuntimeObservedState::Unknown => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn runtime_instance_survives_store_restart() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("orchestrator.db");
        let value = StoredRuntimeInstance {
            node_id: "spiffe://ojos/node/node-1".to_string(),
            instance: RuntimeInstance {
                deployment_id: "deployment-1".to_string(),
                service_id: "judge".to_string(),
                release_version: "1.0.0".to_string(),
                container_id: "container-1".to_string(),
                artifact_digest: format!("sha256:{}", "a".repeat(64)),
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "healthy".to_string(),
            },
            management_mode: RuntimeManagementMode::Managed,
            endpoint: String::new(),
            updated_at: "2026-08-03T00:00:00Z".to_string(),
        };
        {
            let store = SqliteOrchestratorStore::open(&database).unwrap();
            store.put_runtime_instance(&value).unwrap();
        }
        let reopened = SqliteOrchestratorStore::open(&database).unwrap();
        assert_eq!(
            reopened.runtime_instance("deployment-1").unwrap(),
            Some(value)
        );
    }

    #[test]
    fn replacement_projection_is_atomic_and_idempotent() {
        let directory = tempdir().unwrap();
        let store =
            SqliteOrchestratorStore::open(directory.path().join("orchestrator.db")).unwrap();
        let old = StoredRuntimeInstance {
            node_id: "spiffe://ojos/node/node-1".to_string(),
            instance: RuntimeInstance {
                deployment_id: "deployment-old".to_string(),
                service_id: "judge".to_string(),
                release_version: "1.0.0".to_string(),
                container_id: "container-old".to_string(),
                artifact_digest: format!("sha256:{}", "a".repeat(64)),
                desired_state: RuntimeDesiredState::Running,
                observed_state: RuntimeObservedState::Running,
                health: "HEALTHY".to_string(),
            },
            management_mode: RuntimeManagementMode::Managed,
            endpoint: String::new(),
            updated_at: "2026-08-03T00:00:00Z".to_string(),
        };
        let mut replacement = old.clone();
        replacement.instance.deployment_id = "deployment-new".to_string();
        replacement.instance.container_id = "container-new".to_string();
        store.put_runtime_instance(&old).unwrap();

        store
            .replace_runtime_instance("deployment-old", &replacement)
            .unwrap();
        store
            .replace_runtime_instance("deployment-old", &replacement)
            .unwrap();

        assert!(store.runtime_instance("deployment-old").unwrap().is_none());
        assert_eq!(
            store.runtime_instance("deployment-new").unwrap(),
            Some(replacement)
        );
    }
}
