//! Store application coordination and host adapters.
//! Pure release/configuration rules and read-only validation belong to orchestrator-manager::store.
//! Mutation orchestration here owns the existing durable boundary; transport stays in store_v1_api.
pub(crate) mod admission;
pub(crate) mod artifacts;
pub(crate) mod bindings;
pub(crate) mod commands;
pub(crate) mod composition;
pub(crate) mod context;
pub(crate) mod contribution;
pub(crate) mod error;
pub(crate) mod history;
pub(crate) mod install;
pub(crate) mod metadata;
pub(crate) mod node;
pub(crate) mod placement;
pub(crate) mod replacement;
pub(crate) mod runtime_plan;
pub(crate) mod service_context;
pub(crate) mod topology;

pub(crate) use bindings::selected_topology_spec;
pub(crate) use orchestrator_manager::store::validation::InstallTopologySelection;
pub(crate) use topology::{
    StoreTopologyApplyPlan, align_group_binding_generations, binding_context_transition_plans,
    propose_generation_sibling_topology,
};
