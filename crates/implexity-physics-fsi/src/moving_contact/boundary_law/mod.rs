// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

pub mod boundary_discrete_geometry;
pub mod boundary_geometry;
pub mod boundary_mapped_contact;
pub mod boundary_mapped_pair_law;
pub mod boundary_path;
pub mod exposed_surface;
pub mod mapped_surface_patch;
pub mod reference_embedding;
pub mod swept_coverage;
pub mod surface_admission;

pub mod semismooth_directional;

pub mod plane_manifold;

pub mod complete_shell_path;
pub mod primal_vf_family;

pub mod edge_path;

pub mod normal_cone;

pub mod dynamic_family;

pub mod physical_pair_mass;

pub mod native_family_impulse;

pub mod shell_root;

pub mod endpoint_manifold_root;

pub mod endpoint_physical_transaction;

mod contact_invariants;
pub mod contact_step_force;
pub mod compliant_energy_momentum_contact;
pub mod mapped_energy_momentum_contact;
pub mod facet_area_trace;

pub mod surface_quadrature_ownership;

pub mod closed_surface_distance;

pub mod closed_surface_contact_step;
