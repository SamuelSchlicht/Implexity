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

pub mod backends;
pub mod capability_status;
pub mod component_manifests;
pub mod contracts;
pub mod contributions;
pub mod coupling_graph;
pub mod coupling_inventory;
pub mod distributions;
pub mod error;
pub mod extensions;
pub mod field_source_catalog;
pub mod graph;
pub mod history_field_sources;
pub mod ids;
pub mod json;
pub mod mathematical_contracts;
pub mod numeric_contract;
pub mod numerical_requirements;
pub mod objective_terms;
pub mod orchestration;
pub mod package_catalog;
pub mod package_session;
pub mod package_state;
pub mod packages;
pub mod parity;
pub mod plugins;
pub mod providers;
pub mod py_repr;
pub mod pyobj;
pub mod registries;
pub mod rng;
pub mod route_tables;
pub mod runtime_environment;
pub mod schema_ids;
pub mod schemas;
pub mod semantic_physics;
pub mod sufficiency;
pub mod sync;
pub mod wire;

pub use error::{CaeError, CaeResult, CaseError};

pub const CRATE: &str = "implexity-core";

pub const LAYER: &str = "foundation";

pub const PYTHON_COMPATIBILITY_VERSION: &str = "24.0.0rc12.dev9";

pub const RUST_VERSION: &str = env!("CARGO_PKG_VERSION");
