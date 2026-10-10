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

use crate::gas_quadratic_halfspace::{QuadraticHalfspaceOperator, quadratic_halfspace_operator};
pub use crate::gas_quadratic_velocity::{QuadraticEntropyPolicy, QuadraticVelocityBasis};
use crate::gas_velocity_halfspace::HalfspaceMeasure;
use implexity_core::{CaeError, CaeResult};
use implexity_solve::positive_cone::{
    ConeProjectionPolicy, LinearConeResidual, LinearConeResidualProblem,
};
use std::collections::BTreeMap;
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone)]
pub struct QuadraticOpenFace {
    pub outward_normal: [f64; 3],
    pub area_m2: f64,
    pub incoming_coefficient_density: Vec<f64>,
}
#[derive(Clone)]
pub struct QuadraticFreeflightInputs {
    pub volume_m3: f64,
    pub step_s: f64,
    pub previous_coefficient_inventory: Vec<f64>,
    pub faces: Vec<QuadraticOpenFace>,
}
#[derive(Clone)]
pub struct QuadraticFreeflightPolicy {
    pub cone: ConeProjectionPolicy,
    pub entropy: QuadraticEntropyPolicy,
    pub maximum_operator_nonzeros: usize,
    pub geometry_tolerance: f64,
}
pub struct QuadraticFreeflightSolution {
    pub coefficient_inventory: Vec<f64>,
    pub face_weak_inventory_packets: Vec<Vec<f64>>,
    pub face_ten_moment_packets: Vec<[f64; 10]>,
    pub weak_residual: Vec<f64>,
    pub physical_moment_residual: [f64; 10],
    pub cone: LinearConeResidual,
    step_rhs: Vec<f64>,
    step_constraint_rhs: Vec<f64>,
    step_s: f64,
    volume_m3: f64,
    face_areas: Vec<f64>,
    face_outgoing: Vec<QuadraticHalfspaceOperator<f64>>,
    face_supply: Vec<Vec<f64>>,
}
pub struct QuadraticFreeflightStepDirection {
    pub coefficient_inventory: Vec<f64>,
    pub face_weak_inventory_packets: Vec<Vec<f64>>,
    pub face_ten_moment_packets: Vec<[f64; 10]>,
}
impl QuadraticFreeflightSolution {
    pub fn step_jvp(
        &self,
        b: &QuadraticVelocityBasis,
    ) -> CaeResult<QuadraticFreeflightStepDirection> {
        let state = self
            .cone
            .jvp_rhs(&self.step_rhs, &self.step_constraint_rhs)?;
        let mut packets = Vec::with_capacity(self.face_outgoing.len());
        let mut physical = Vec::with_capacity(self.face_outgoing.len());
        for (f, out) in self.face_outgoing.iter().enumerate() {
            let flow = out.apply(&self.coefficient_inventory)?;
            let change = out.apply(&state)?;
            let packet = (0..state.len())
                .map(|i| {
                    self.face_areas[f] * (flow[i] / self.volume_m3 - self.face_supply[f][i])
                        + self.step_s * self.face_areas[f] * change[i] / self.volume_m3
                })
                .collect::<Vec<_>>();
            physical.push(b.weak_moments(&packet)?);
            packets.push(packet);
        }
        Ok(QuadraticFreeflightStepDirection {
            coefficient_inventory: state,
            face_weak_inventory_packets: packets,
            face_ten_moment_packets: physical,
        })
    }
    pub fn step_vjp(&self, state_bar: &[f64]) -> CaeResult<f64> {
        let (stationarity, constraint) = self.cone.vjp_rhs(state_bar)?;
        let value = stationarity
            .iter()
            .zip(&self.step_rhs)
            .map(|(a, b)| a * b)
            .sum::<f64>()
            + constraint
                .iter()
                .zip(&self.step_constraint_rhs)
                .map(|(a, b)| a * b)
                .sum::<f64>();
        if !value.is_finite() {
            return Err(fail("quadratic free-flight step cotangent is nonfinite"));
        }
        Ok(value)
    }
}
pub fn solve(
    b: &QuadraticVelocityBasis,
    a: &QuadraticFreeflightInputs,
    p: &QuadraticFreeflightPolicy,
) -> CaeResult<QuadraticFreeflightSolution> {
    let n = b.coefficient_count();
    if !a.volume_m3.is_finite()
        || a.volume_m3 <= 0.
        || !a.step_s.is_finite()
        || a.step_s <= 0.
        || a.previous_coefficient_inventory.len() != n
        || a.previous_coefficient_inventory
            .iter()
            .any(|v| !v.is_finite() || *v < 0.)
        || a.faces.is_empty()
        || !p.geometry_tolerance.is_finite()
        || p.geometry_tolerance <= 0.
    {
        return Err(fail("invalid quadratic free-flight state or geometry"));
    }
    let mut closure = [0.; 3];
    let mut area = 0.;
    let mut operator = b
        .mass_rows()?
        .into_iter()
        .map(|r| r.into_iter().collect::<BTreeMap<_, _>>())
        .collect::<Vec<_>>();
    let mut rhs = b.mass_apply(&a.previous_coefficient_inventory)?;
    let mut faces = vec![];
    for face in &a.faces {
        let norm = face.outward_normal.iter().map(|v| v * v).sum::<f64>();
        if face.outward_normal.iter().any(|v| !v.is_finite())
            || (norm - 1.).abs() > p.geometry_tolerance
            || !face.area_m2.is_finite()
            || face.area_m2 < 0.
            || face.incoming_coefficient_density.len() != n
            || face
                .incoming_coefficient_density
                .iter()
                .any(|v| !v.is_finite() || *v < 0.)
        {
            return Err(fail("invalid quadratic free-flight face"));
        }
        area += face.area_m2;
        for j in 0..3 {
            closure[j] += face.area_m2 * face.outward_normal[j];
        }
        let outgoing = quadratic_halfspace_operator(
            b,
            face.outward_normal,
            0.,
            HalfspaceMeasure::PositiveSpeed,
            p.maximum_operator_nonzeros,
        )?;
        let incoming = quadratic_halfspace_operator(
            b,
            face.outward_normal.map(|v| -v),
            0.,
            HalfspaceMeasure::PositiveSpeed,
            p.maximum_operator_nonzeros,
        )?;
        let supply = incoming.apply(&face.incoming_coefficient_density)?;
        let outgoing_rows = outgoing.rows();
        for i in 0..n {
            rhs[i] += a.step_s * face.area_m2 * supply[i];
            for &(j, v) in &outgoing_rows[i] {
                *operator[i].entry(j).or_default() += a.step_s * face.area_m2 / a.volume_m3 * v;
            }
        }
        faces.push((outgoing, supply));
    }
    if !area.is_finite()
        || area <= 0.
        || closure
            .iter()
            .any(|v| !v.is_finite() || v.abs() > p.geometry_tolerance * area)
    {
        return Err(fail(
            "quadratic free-flight faces do not close the control volume",
        ));
    }
    let rows = operator
        .into_iter()
        .map(|r| r.into_iter().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if rows.iter().map(|r| r.len()).sum::<usize>() > p.maximum_operator_nonzeros {
        return Err(fail("quadratic free-flight sparse operator size exceeded"));
    }
    let reproduction = b.reproduction_vectors();
    let mut constraints = vec![vec![0.; n]; 10];
    for (i, row) in rows.iter().enumerate() {
        for &(j, v) in row {
            for m in 0..10 {
                constraints[m][j] += reproduction[i][m] * v;
            }
        }
    }
    let lifting = (0..10)
        .map(|m| {
            b.integrated_moment_rows()
                .iter()
                .map(|r| r[m])
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let target = b.weak_moments(&rhs)?;
    let seed = b.moment_entropy(target, &p.entropy)?.coefficient_density;
    let problem = LinearConeResidualProblem::new(
        rows,
        lifting,
        constraints,
        target.to_vec(),
        p.cone.clone(),
    )?;
    let cone = problem.solve(&rhs, &seed)?;
    let mut packets = vec![];
    let mut physical = vec![];
    let mut residual = b.mass_apply(&cone.state)?;
    let old = b.mass_apply(&a.previous_coefficient_inventory)?;
    for i in 0..n {
        residual[i] -= old[i];
    }
    let mut step_rhs = vec![0.; n];
    for (f, (out, supply)) in a.faces.iter().zip(&faces) {
        let flow = out.apply(&cone.state)?;
        let packet = (0..n)
            .map(|i| a.step_s * f.area_m2 * (flow[i] / a.volume_m3 - supply[i]))
            .collect::<Vec<_>>();
        for i in 0..n {
            residual[i] += packet[i];
            step_rhs[i] -= f.area_m2 * (flow[i] / a.volume_m3 - supply[i]);
        }
        physical.push(b.weak_moments(&packet)?);
        packets.push(packet);
    }
    let moment_residual = b.weak_moments(&residual)?;
    if residual
        .iter()
        .chain(moment_residual.iter())
        .any(|v| !v.is_finite())
    {
        return Err(fail("quadratic free-flight residual is nonfinite"));
    }
    let step_constraint_rhs = b.weak_moments(&step_rhs)?.to_vec();
    let (face_outgoing, face_supply) = faces.into_iter().unzip();
    Ok(QuadraticFreeflightSolution {
        coefficient_inventory: cone.state.clone(),
        face_weak_inventory_packets: packets,
        face_ten_moment_packets: physical,
        weak_residual: residual,
        physical_moment_residual: moment_residual,
        cone,
        step_rhs,
        step_constraint_rhs,
        step_s: a.step_s,
        volume_m3: a.volume_m3,
        face_areas: a.faces.iter().map(|f| f.area_m2).collect(),
        face_outgoing,
        face_supply,
    })
}
