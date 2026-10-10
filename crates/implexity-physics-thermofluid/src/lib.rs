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
#![recursion_limit = "512"]



#![allow(
    clippy::result_large_err,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::nonminimal_bool,
    clippy::type_complexity,
    clippy::redundant_closure_for_method_calls,
    clippy::float_cmp,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

pub mod addins;
pub mod channel_transport;
pub mod conjugate_energy_contract;
pub mod conjugate_history;
pub mod conjugate_interface;
pub mod conjugate_preload;
pub mod conservative_interfaces;
pub mod elastic_preload;
pub mod equation_of_state;
pub mod fluid_admission;
pub mod incompressible_transport;
pub mod liquid_admissibility;
pub mod liquid_pressure_margin;
pub mod local_group;
pub mod mac_energy_contract;
pub mod nodal_cell_exchange;
pub mod nodal_transport;
pub mod peng_robinson;
pub mod qualification_identity;
pub mod real_fluid_screening;
pub mod rv;
pub mod stokes_brinkman;
pub mod thermal_exchange;
pub mod thermal_stress_wall;
pub mod unified_history;
pub mod unified_history_preconditioner;
pub mod wall_closure;
pub mod water_saturation;

pub use addins::link;

pub const CRATE: &str = "implexity-physics-thermofluid";

pub const LAYER: &str = "physics";

pub mod porous_reference;

pub mod schema_ids;
