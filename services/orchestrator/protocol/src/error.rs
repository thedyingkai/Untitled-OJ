//! Shared error vocabulary retained during the runtime/protocol split.
//! Execution variants carry messages only; concrete engine errors stay in the adapter.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("invalid OCI image reference: {0}")]
    InvalidImageReference(String),
    #[error("invalid container health policy: {0}")]
    InvalidHealthPolicy(String),
    #[error("invalid release replacement payload: {0}")]
    InvalidReleaseReplacement(String),
    #[error("invalid published endpoint: {0}")]
    InvalidPublishedEndpoint(String),
    #[error("invalid Docker registry credentials: {0}")]
    InvalidRegistryCredentials(String),
    #[error("invalid runtime contract: {0}")]
    InvalidRuntimeContract(String),
    #[error("invalid materialized runtime context: {0}")]
    InvalidRuntimeContext(String),
    #[error("docker engine is unavailable: {0}")]
    EngineUnavailable(String),
    #[error("docker operation failed: {0}")]
    Engine(String),
    #[error("pulled image did not expose requested digest {requested}; found {actual:?}")]
    DigestMismatch {
        requested: String,
        actual: Vec<String>,
    },
    #[error("runtime instance does not contain a container id")]
    MissingContainerId,
}
