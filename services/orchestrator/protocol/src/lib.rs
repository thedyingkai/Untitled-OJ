//! Credential-free wire types shared by the control plane and Node Agent.
//!
//! This crate owns serialized reports and closed runtime profile validation.
//! It has no transport, filesystem, database, Docker, or Agent dependency.

mod error;
mod node;
mod profiles;

pub use error::RuntimeError;
pub use node::{
    CredentialRefreshStatus, DeploymentRuntimeObservationV1, DockerRuntimeFacts,
    ManagedDeploymentInventoryV1, NodeRuntimeFactsV1, RuntimeDesiredState, RuntimeInstance,
    RuntimeObservedState,
};
pub use profiles::*;
