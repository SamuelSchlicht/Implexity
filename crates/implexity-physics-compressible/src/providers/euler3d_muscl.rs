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

use serde_json::Value;

use crate::errors::PResult;
use crate::euler3d::{
    Problem, WallLayout,
};
use crate::euler3d_ad::AdmittedHistory;

pub static PROVIDER: Euler3DTopologyProvider = Euler3DTopologyProvider::muscl();

pub use crate::euler3d_muscl::{MusclLedger,METHOD,MusclSolidLoads,FORCE_SAMPLING};


#[allow(clippy::type_complexity)]
pub fn reference_rhs(
    q: &[[f64; 5]],
    p: &Problem,
    phi: Option<&[f64]>,
) -> PResult<(Vec<[f64; 5]>, Vec<f64>, [f64; 5], [f64; 5])> {
    crate::euler3d_muscl::reference_rhs(q,p,phi).map_err(Into::into)
}


#[allow(clippy::too_many_lines)]
pub fn checked_history(
    problem: &Value,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
    fluid_fraction: Option<&[f64]>,
) -> PResult<(Vec<Vec<[f64; 5]>>, MusclLedger)> {
    crate::euler3d_muscl::checked_history(problem,step_s,step_count,budget,fluid_fraction).map_err(Into::into)
}


pub fn admit_history(flow: &Value, timing: &Value, phi: &[f64]) -> PResult<AdmittedHistory> {
    crate::euler3d_muscl::admit_history(flow,timing,phi).map_err(Into::into)
}


pub fn step<S: implexity_ad::Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: Option<&[S]>,
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    crate::euler3d_muscl::step(q,p,phi,dt).map_err(Into::into)
}


pub fn fixed_history(
    q0: &[[f64; 5]],
    p: &Problem,
    dt: f64,
    count: usize,
    phi: Option<&[f64]>,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    crate::euler3d_muscl::fixed_history(q0,p,dt,count,phi).map_err(Into::into)
}


pub fn history_responses(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    phi: &[f64],
    dt: f64,
) -> PResult<crate::euler3d_ad::HistoryResponses> {
    crate::euler3d_muscl::history_responses(states,p,phi,dt).map_err(Into::into)
}


pub fn conforming_solid_load_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    solid_nodes: &[[f64; 3]],
) -> PResult<MusclSolidLoads> {
    crate::euler3d_muscl::conforming_solid_load_history(states,p,layout,dt,solid_nodes).map_err(Into::into)
}

use crate::providers::euler3d_ad::Euler3DTopologyProvider;

pub const NAME: &str = "compressible_cartesian_euler3d_muscl_topology";
