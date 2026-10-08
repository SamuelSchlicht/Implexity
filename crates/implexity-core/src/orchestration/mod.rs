// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





pub mod compat;
mod intent;
mod plan;
mod registry;
mod types;

pub use intent::{EngineeringIntent, IntentGoal, RELATIONS, py_float};
pub use plan::{CouplingEdge, OrchestrationPlan, OrchestrationPlanner, direct_port_match, provider_score};
pub use registry::{
    AddInAdapter, AddInRegistry, AddInRegistrySnapshot, ContractInput, LegacyProviderAdapter,
    ProviderBundleAdapter, RegisteredAddIn, RegistryBindingToken, adapter_ptr, numerical_owner,
    register_provider_bundle, resolve_owner_identity, same_adapter, stable_implementation,
};
pub use types::{
    AddInCategory, AddInContract, AuthoringRequirement, DesignCoordinateRef, ExecutionKind,
    ExternalPortValue, Fidelity, PlanStatus, PortKey, PortSpec, PublishedContract, ResponseCapability,
    RuntimeRoute, STRICT_CONTRACT_VERSION,
};
