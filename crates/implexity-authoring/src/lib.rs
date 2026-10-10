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


#![allow(
    clippy::float_cmp,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::items_after_statements,
    clippy::nonminimal_bool,
    clippy::match_same_arms,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    clippy::struct_excessive_bools
)]

pub mod error;
pub mod py;
pub mod spatial_values;

pub use error::{AResult, AuthoringError};

pub const CRATE: &str = "implexity-authoring";

pub const LAYER: &str = "kernel";
pub mod api;
pub mod editing;
pub mod engineering_glyphs;
pub mod entities;
pub mod field_brush;
pub mod field_interaction;
pub mod geometry_freeze;
pub mod geometry_holds;
pub mod geometry_sculpt;
pub mod geometry_selection;
pub mod guided_setup;
pub mod incremental_fields;
pub mod interaction_runtime;
pub mod interaction_transactions;
pub mod interactive_runtime;
pub mod kinds;
pub mod manipulation;
pub mod migrate_problem_backend;
pub mod model_manager;
pub mod model_view;
pub mod optimization_setup;
pub mod physics_binding;
pub mod problem;
pub mod progressive_fields;
pub mod rigid_translation;
pub mod seeds;
pub mod sensitivity_authoring;
pub mod services;
pub mod setup_transaction;
pub mod spatial_design;
pub mod spatial_selection;
pub mod spatial_transactions;
pub mod surface_authoring;
pub mod surface_regions;
pub mod sync;
pub mod zlib_deflate;
