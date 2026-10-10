// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END


#![forbid(unsafe_code)]

pub mod addins;
pub mod carrier;
pub mod contact;
pub mod editor;
pub mod frames;
pub mod interface;
mod json;
pub mod model;
pub mod narrow_gap;
pub mod narrow_gap_lbm;
pub mod narrow_gap_lbm_provider;
pub mod surface_gap;
pub mod surface_gap_history;
pub mod surface_gap_envelope;
pub mod surface_gap_geometry_owner;
pub mod surface_gap_transport;
pub mod surface_gap_field;
pub mod surface_gap_problem;
pub mod surface_gap_provider;
pub mod narrow_gap_provider;
pub mod moving_contact;
pub mod restart;
pub mod modifier;
pub mod options;
pub mod problem;
pub mod provider;
pub mod regime;
pub mod run;
pub mod templates;

mod compact_geometry_directions;

pub const CRATE: &str = "implexity-physics-fsi";

pub const LAYER: &str = "physics";

pub mod gas_gap_mass;

pub mod gas_gap_geometry;


pub mod gas_gap_box_quadrature;

pub mod contact_constraint_tangent;

pub(crate) mod gas_bgk_preconditioner;
pub(crate) mod gas_continuous_layout;
pub(crate) mod gas_continuous_mixed;
pub(crate) mod gas_continuous_mixed_implicit;
pub(crate) mod gas_continuous_mixed_solver;
pub(crate) mod gas_continuous_preconditioner;
pub(crate) mod gas_continuous_sparse_block;
pub(crate) mod gas_continuous_transport;
pub(crate) mod gas_continuum_fv;
pub(crate) mod gas_continuum_lift;
pub(crate) mod gas_geometry_time;
pub(crate) mod gas_layered_tetra_mesh;
pub(crate) mod gas_mixed_network;
pub(crate) mod gas_mixed_preconditioner;
pub(crate) mod gas_mixed_solver;
pub(crate) mod gas_mixed_vjp;
pub(crate) mod gas_moment_coarse;
pub(crate) mod gas_multiscale_flux;
pub(crate) mod gas_multiscale_network;
pub(crate) mod gas_multiscale_solver;
pub(crate) mod gas_quadratic_axis_operator;
pub(crate) mod gas_quadratic_balance;
pub(crate) mod gas_quadratic_coefficient_reconstruction;
pub(crate) mod gas_quadratic_diffuse_wall;
pub(crate) mod gas_quadratic_gaussian;
pub(crate) mod gas_quadratic_halfspace;
pub(crate) mod gas_quadratic_multiscale_flux;
pub(crate) mod gas_quadratic_network;
pub(crate) mod gas_quadratic_network_solver;
pub(crate) mod gas_quadratic_reconstruction;
pub(crate) mod gas_quadratic_reconstruction_admission;
pub(crate) mod gas_quadratic_velocity;
pub(crate) mod gas_transient_kinetic;
pub(crate) mod gas_velocity_halfspace;
pub(crate) mod gas_vjp;
pub(crate) mod gas_weak_force;
pub(crate) mod native_closed_release;
pub(crate) mod native_continuous_adjoint;
pub(crate) mod native_continuous_checkpoint;
pub(crate) mod native_continuous_coupled_field;
pub(crate) mod native_continuous_document;
pub(crate) mod native_continuous_factory;
pub(crate) mod native_continuous_gas;
pub(crate) mod native_continuous_history;
pub(crate) mod native_continuous_initial_adjoint;
pub(crate) mod native_continuous_interface;
pub(crate) mod native_continuous_port_stage_vjp;
pub(crate) mod native_continuous_ports;
pub(crate) mod native_continuous_problem;
pub mod native_continuous_provider;
pub(crate) mod native_continuous_tangent;
pub(crate) mod native_normalization;
pub(crate) mod native_cut_cell_partition;
pub(crate) mod native_producer_rounding;
pub(crate) mod native_cut_cell_refinement;
pub(crate) mod native_cut_cell_state;
pub(crate) mod native_dry_gap_factory;
pub(crate) mod native_gap_column_owner;
pub(crate) mod native_gas_coupled_field;
pub(crate) mod native_gas_geometry_adapter;
pub(crate) mod native_gas_moment_vjp;
pub(crate) mod native_gas_port_coupling;
pub(crate) mod native_gas_stress_trace;
pub(crate) mod native_gas_wall_anchor;
pub(crate) mod native_gas_wall_transfer;
pub(crate) mod patch_unilateral_contact;
pub(crate) mod reference_traction;
pub(crate) mod native_geometry_construction_error;
pub(crate) mod native_gas_union_slot_labels;

pub mod kinetic_gas_mobility;
pub mod gas_gap_kinetic_mass;
pub mod gas_kinetic_port_coupling;
pub mod gas_quadratic_freeflight;
pub(crate) mod native_normal_birth;
pub(crate) mod native_layered_birth_source;
pub(crate) mod native_runtime_birth_clock;
pub(crate) mod native_runtime_birth_stages;
pub(crate) mod native_runtime_birth_sample;
pub(crate) mod native_layered_birth_multicontext;
pub(crate) mod native_runtime_birth_consumer;

mod native_affine_grid_sign;

mod native_closed_stage;

mod native_exact_affine_predicate;

mod native_physical_plane_preparation;

mod native_physical_segment_overlay;

mod native_source_face_windows;

mod native_source_facet_index;

mod native_source_stage_labels;

mod native_source_wall_stage;

mod native_union_arrangement;

mod native_union_boundary;

mod native_union_facets;

mod native_union_quadrature;

mod native_union_slot_index;
