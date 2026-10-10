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
    clippy::float_cmp_const,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::needless_range_loop,
    clippy::too_many_lines,
    clippy::type_complexity,
    clippy::items_after_statements,

    clippy::nonminimal_bool,

    clippy::match_same_arms
)]

pub mod boxes;
pub mod contour_from_sdf;
pub mod derivatives;
pub mod differentiable_drag;
pub mod differentiable_reachability;
pub mod direct_occupancy;
pub mod document;
pub mod domain_sdf;
pub mod engineering;
pub mod engineering_history;
pub mod error;
pub mod errors;
pub mod eval;
pub mod examples;
pub mod field_registration;
pub mod field_views;
pub mod fieldclass;
pub mod glsl;
pub mod glsl_text;
pub mod kinds;
pub mod lattice;
pub mod linalg3;
pub mod node;
pub mod numpy;
pub mod occupancy;
pub mod ops;
pub mod phase_connectivity;
pub mod phase_partition;
pub mod preview;
pub mod primitives;
pub mod profiles;
pub mod pyfmt;
pub mod result_fields;
pub mod sampled;
pub mod scalar;
pub mod semantic_regions;
pub mod topology_monitor;
pub mod tpms;
pub mod value;

pub use error::{GResult, GeometryError};
pub use fieldclass::{ClassKind, FieldClass};
pub use node::{Attr, Node, NodeRef, ParamRef, Registry};
pub use value::{DType, NdArray, ParamValue};

pub const CRATE: &str = "implexity-geometry";

pub const LAYER: &str = "kernel";

#[must_use]
pub fn kernel_registry() -> Registry {
    let mut r = Registry::empty();
    let all = primitives::entries()
        .into_iter()
        .chain(ops::entries())
        .chain(profiles::entries())
        .chain(tpms::entries())
        .chain(sampled::entries())
        .chain(lattice::entries());
    for e in all {

        let _ = r.register(e);
    }
    r
}

pub mod field_processing;
