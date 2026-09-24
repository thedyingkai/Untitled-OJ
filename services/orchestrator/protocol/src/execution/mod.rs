//! Shared execution descriptions and their deterministic validation.
//! Serialized jobs never contain credentials or Agent-local filesystem policy.
//! The optional local expansion in ContainerSpec remains excluded from serde.
//! Docker API requests and credential materialization belong to runtime/Agent.

mod container;
mod health;
mod migration;
mod pipeline;
mod service_context;
pub mod validation;
mod volumes;

pub use container::*;
pub use health::*;
pub use migration::*;
pub use pipeline::*;
pub use service_context::*;
pub use volumes::*;
