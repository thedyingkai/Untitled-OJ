//! Credential-free wire types shared by the control plane and Node Agent.
//!
//! This crate owns reports, execution payloads and closed runtime validation.
//! It has no transport, filesystem, database, Docker, or Agent dependency.

mod error;
pub mod execution;
pub use execution::*;
mod node;
mod profiles;

pub use error::RuntimeError;
pub use node::{
    CredentialRefreshStatus, DeploymentRuntimeObservationV1, DockerRuntimeFacts,
    ManagedDeploymentInventoryV1, NodeRuntimeFactsV1, RuntimeDesiredState, RuntimeInstance,
    RuntimeObservedState,
};
pub use profiles::*;
