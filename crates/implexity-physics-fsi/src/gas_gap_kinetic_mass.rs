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

use crate::gas_gap_mass::{
    self as gas, GasMassFace, GasMassInputs, GasMassOutputBars, GasMassPolicy, GasMassScalar,
    GasMassSolution,
};
use crate::kinetic_gas_mobility::{KineticGasFace, KineticMobilityEvaluator};
use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::lu::SparseLu;
use implexity_linalg::CsrMatrix;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticMassFace<S = f64> {
    pub gap_m: S,
    pub width_m: S,
    pub length_m: S,
    pub dynamic_viscosity_pa_s: S,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticGasMassInputs<S = f64> {
    pub gas: GasMassInputs<S>,
    pub kinetic_faces: Vec<Option<KineticMassFace<S>>>,
    pub maximum_mean_flow_molecular_speed_ratio: f64,
}
#[derive(Clone, Copy)]
struct Flux {
    value: f64,
    left: f64,
    right: f64,
    alpha: f64,
    rt: f64,
    g: f64,
    a: f64,
    h: f64,
    width: f64,
    length: f64,
    mu: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KineticGasMassValidity {
    pub maximum_mean_flow_molecular_speed_ratio: f64,
    pub maximum_face_absolute_pressure_ratio: f64,
    pub initially_empty_open_cells: Vec<usize>,
}
pub struct KineticGasMassLinearization {
    input: KineticGasMassInputs,
    solution: GasMassSolution,
    row_scale: Vec<f64>,
    factor: SparseLu,
    flux: Vec<Flux>,
    validity: KineticGasMassValidity,
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
impl<S: Scalar> KineticGasMassInputs<S> {
    pub fn primal(&self) -> KineticGasMassInputs {
        KineticGasMassInputs {
            gas: self.gas.primal(),
            maximum_mean_flow_molecular_speed_ratio: self.maximum_mean_flow_molecular_speed_ratio,
            kinetic_faces: self
                .kinetic_faces
                .iter()
                .map(|f| {
                    f.as_ref().map(|f| KineticMassFace {
                        gap_m: f.gap_m.value(),
                        width_m: f.width_m.value(),
                        length_m: f.length_m.value(),
                        dynamic_viscosity_pa_s: f.dynamic_viscosity_pa_s.value(),
                    })
                })
                .collect(),
        }
    }
}
fn validate<S: Scalar>(a: &KineticGasMassInputs<S>) -> CaeResult<()> {
    gas::validate(&a.gas)?;
    if a.kinetic_faces.len() != a.gas.faces.len()
        || !a.maximum_mean_flow_molecular_speed_ratio.is_finite()
        || a.maximum_mean_flow_molecular_speed_ratio <= 0.
        || a.maximum_mean_flow_molecular_speed_ratio >= 1.
    {
        return Err(fail("kinetic gas face count"));
    }
    for (f, k) in a.gas.faces.iter().zip(&a.kinetic_faces) {
        if let Some(k) = k {
            if !k.gap_m.value().is_finite()
                || k.gap_m.value() < 0.
                || [
                    k.width_m.value(),
                    k.length_m.value(),
                    k.dynamic_viscosity_pa_s.value(),
                ]
                .iter()
                .any(|v| !v.is_finite() || *v <= 0.)
                || f.conductance_m3_pa_s.value() != 0.
                || k.gap_m.value() == 0. && f.advection_m3_s.value() != 0.
            {
                return Err(fail(
                    "kinetic face must replace continuum conductance and seal zero aperture",
                ));
            }
        }
    }
    Ok(())
}
fn records(
    a: &KineticGasMassInputs,
    cell: &[f64],
    port: &[f64],
    evaluator: &mut KineticMobilityEvaluator,
) -> CaeResult<Vec<Flux>> {
    validate(a)?;
    let g = &a.gas;
    if cell.len() != g.volume_m3.len()
        || port.len() != g.base_port_pressure_pa.len()
        || cell.iter().enumerate().any(|(i, p)| {
            !p.is_finite()
                || if g.volume_m3[i] == 0. {
                    *p != 0.
                } else {
                    *p <= 0.
                }
        })
        || port.iter().any(|p| !p.is_finite() || *p <= 0.)
    {
        return Err(fail("kinetic gas pressure domain"));
    }
    let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
    let rt = g.reference_pressure_pa / g.reference_density_kg_m3;
    a.kinetic_faces
        .iter()
        .zip(&g.faces)
        .map(|(k, f)| {
            let mut out = Flux {
                value: 0.,
                left: 0.,
                right: 0.,
                alpha: 0.,
                rt: 0.,
                g: 0.,
                a: 0.,
                h: 0.,
                width: 0.,
                length: 0.,
                mu: 0.,
            };
            if !gas::active(g, f) {
                return Ok(out);
            }
            let pl = cell[f.left];
            let pr = if let Some(r) = f.right {
                cell[r]
            } else {
                port[f.port.unwrap_or(0)]
            };
            let donor = if f.advection_m3_s >= 0. { pl } else { pr };
            if let Some(k) = k {
                let z = evaluator.face_mass_flux(
                    &KineticGasFace {
                        gap_m: k.gap_m,
                        width_m: k.width_m,
                        length_m: k.length_m,
                        dynamic_viscosity_pa_s: k.dynamic_viscosity_pa_s,
                        gas_rt_m2_s2: rt,
                    },
                    pl,
                    pr,
                )?;
                out.value = z.mass_flux_kg_s;
                out.left = z
                    .pressure_left_derivative_kg_pa_s
                    .ok_or_else(|| fail("kinetic gas left pressure derivative"))?;
                out.right = z
                    .pressure_right_derivative_kg_pa_s
                    .ok_or_else(|| fail("kinetic gas right pressure derivative"))?;
                out.h = z.gap_derivative_kg_m_s.unwrap_or(0.);
                out.width = z.mass_flux_kg_s / k.width_m;
                out.length = -z.mass_flux_kg_s / k.length_m;
                out.mu = z.viscosity_derivative_kg_pa_s2.unwrap_or(0.);
                out.rt = z.rt_derivative_kg_s_m2.unwrap_or(0.);
            } else {
                let term = f.conductance_m3_pa_s * (pl - pr) * (pl + pr) * 0.5;
                out.value = alpha * term;
                out.left = alpha * f.conductance_m3_pa_s * pl;
                out.right = -alpha * f.conductance_m3_pa_s * pr;
                out.alpha = term;
                out.g = alpha * (pl - pr) * (pl + pr) * 0.5;
            }
            out.value += alpha * f.advection_m3_s * donor;
            out.alpha += f.advection_m3_s * donor;
            out.a = alpha * donor;
            if f.advection_m3_s >= 0. {
                out.left += alpha * f.advection_m3_s;
            } else {
                out.right += alpha * f.advection_m3_s;
            }
            if [
                out.value, out.left, out.right, out.alpha, out.rt, out.g, out.a, out.h, out.width,
                out.length, out.mu,
            ]
            .iter()
            .any(|v| !v.is_finite())
            {
                return Err(fail("kinetic gas flux linearization is not finite"));
            }
            Ok(out)
        })
        .collect()
}
fn validity(
    a: &KineticGasMassInputs,
    cell: &[f64],
    port: &[f64],
    flux: &[Flux],
) -> CaeResult<KineticGasMassValidity> {
    let g = &a.gas;
    let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
    let rt = g.reference_pressure_pa / g.reference_density_kg_m3;
    let mut report = KineticGasMassValidity {
        maximum_mean_flow_molecular_speed_ratio: 0.,
        maximum_face_absolute_pressure_ratio: 1.,
        initially_empty_open_cells: g
            .volume_m3
            .iter()
            .zip(&g.previous_mass_kg)
            .enumerate()
            .filter_map(|(i, (v, m))| (*v > 0. && *m == 0.).then_some(i))
            .collect(),
    };
    for ((face, k), q) in g.faces.iter().zip(&a.kinetic_faces).zip(flux) {
        if !gas::active(g, face) {
            continue;
        }
        if let Some(k) = k {
            if k.gap_m > 0. {
                let pl = cell[face.left];
                let pr = if let Some(r) = face.right {
                    cell[r]
                } else {
                    port[face.port.unwrap_or(0)]
                };
                let donor = if face.advection_m3_s >= 0. { pl } else { pr };
                let advective_flux = alpha * face.advection_m3_s * donor;
                let component_flux_bound = (q.value - advective_flux).abs() + advective_flux.abs();
                let ratio = component_flux_bound
                    / (alpha * pl.min(pr) * k.width_m * k.gap_m * (2. * rt).sqrt());
                report.maximum_mean_flow_molecular_speed_ratio =
                    report.maximum_mean_flow_molecular_speed_ratio.max(ratio);
                report.maximum_face_absolute_pressure_ratio = report
                    .maximum_face_absolute_pressure_ratio
                    .max(pl.max(pr) / pl.min(pr));
                if !ratio.is_finite() || ratio > a.maximum_mean_flow_molecular_speed_ratio {
                    return Err(fail("kinetic gap mean flow exceeds the declared linearized molecular-speed domain"));
                }
            }
        }
    }
    if !report.maximum_face_absolute_pressure_ratio.is_finite() {
        return Err(fail("kinetic face pressure ratio is not finite"));
    }
    Ok(report)
}
fn residual(
    a: &KineticGasMassInputs,
    cell: &[f64],
    port: &[f64],
    f: &[Flux],
) -> CaeResult<Vec<f64>> {
    let a = &a.gas;
    let n = cell.len();
    let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
    let mut r = cell
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if a.volume_m3[i] == 0. {
                *p
            } else {
                alpha * a.volume_m3[i] * p - a.previous_mass_kg[i]
            }
        })
        .collect::<Vec<_>>();
    r.extend(
        port.iter()
            .zip(&a.base_port_pressure_pa)
            .map(|(p, b)| p - b),
    );
    for (face, q) in a.faces.iter().zip(f) {
        r[face.left] += a.step_s * q.value;
        if let Some(right) = face.right {
            r[right] -= a.step_s * q.value;
        } else if let Some(p) = face.port {
            r[n + p] -= a.port_pressure_per_kg[p] * a.step_s * q.value;
        }
    }
    if r.iter().any(|v| !v.is_finite()) {
        return Err(fail("kinetic gas residual is not finite"));
    }
    Ok(r)
}
fn jacobian(a: &KineticGasMassInputs, flux: &[Flux], scale: &[f64]) -> CaeResult<CsrMatrix> {
    let a = &a.gas;
    let n = a.volume_m3.len();
    let np = a.base_port_pressure_pa.len();
    let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
    let pref = a.reference_pressure_pa;
    let mut rows = Vec::new();
    let mut cols = Vec::new();
    let mut vals = Vec::new();
    let mut add = |i: usize, j: usize, v: f64| {
        rows.push(i);
        cols.push(j);
        vals.push(v * pref / scale[i]);
    };
    for i in 0..n {
        add(
            i,
            i,
            if a.volume_m3[i] == 0. {
                1.
            } else {
                alpha * a.volume_m3[i]
            },
        );
    }
    for p in 0..np {
        add(n + p, n + p, 1.);
    }
    for (face, q) in a.faces.iter().zip(flux) {
        if !gas::active(a, face) {
            continue;
        }
        let j = face.right.unwrap_or(n + face.port.unwrap_or(0));
        for (column, derivative) in [(face.left, q.left), (j, q.right)] {
            add(face.left, column, a.step_s * derivative);
            if let Some(right) = face.right {
                add(right, column, -a.step_s * derivative);
            } else if let Some(p) = face.port {
                add(
                    n + p,
                    column,
                    -a.port_pressure_per_kg[p] * a.step_s * derivative,
                );
            }
        }
    }
    CsrMatrix::from_triplets(n + np, n + np, &rows, &cols, &vals).map_err(|e| fail(e.to_string()))
}
fn output(
    a: &KineticGasMassInputs,
    cell: Vec<f64>,
    port: Vec<f64>,
    flux: &[Flux],
    iterations: usize,
) -> CaeResult<GasMassSolution> {
    let g = &a.gas;
    let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
    let mass = g
        .volume_m3
        .iter()
        .zip(&cell)
        .map(|(v, p)| alpha * v * p)
        .collect::<Vec<_>>();
    let r = residual(a, &cell, &port, flux)?;
    let boundary = g
        .faces
        .iter()
        .zip(flux)
        .filter(|(f, _)| f.port.is_some())
        .map(|(_, q)| q.value)
        .sum::<f64>();
    let defect =
        mass.iter().sum::<f64>() - g.previous_mass_kg.iter().sum::<f64>() + g.step_s * boundary;
    if mass
        .iter()
        .enumerate()
        .any(|(i, m)| !m.is_finite() || g.volume_m3[i] > 0. && *m <= 0.)
        || !defect.is_finite()
    {
        return Err(fail("kinetic gas mass output domain"));
    }
    Ok(GasMassSolution {
        cell_pressure_pa: cell,
        cell_mass_kg: mass,
        port_pressure_pa: port,
        face_mass_flux_kg_s: flux.iter().map(|q| q.value).collect(),
        closed_cells: g.volume_m3.iter().map(|v| *v == 0.).collect(),
        iterations,
        maximum_mass_residual_kg: r[..g.volume_m3.len()]
            .iter()
            .map(|v| v.abs())
            .fold(0., f64::max),
        maximum_port_residual_pa: r[g.volume_m3.len()..]
            .iter()
            .map(|v| v.abs())
            .fold(0., f64::max),
        mass_balance_defect_kg: defect,
    })
}
fn supply(a: &KineticGasMassInputs) -> CaeResult<()> {
    let mut g = a.gas.clone();
    for (f, k) in g.faces.iter_mut().zip(&a.kinetic_faces) {
        if let Some(k) = k {
            f.conductance_m3_pa_s = if k.gap_m > 0. { 1. } else { 0. };
        }
    }
    gas::supply_check(&g)
}
pub fn solve(
    a: &KineticGasMassInputs,
    p: &GasMassPolicy,
    evaluator: &mut KineticMobilityEvaluator,
) -> CaeResult<(GasMassSolution, KineticGasMassLinearization)> {
    validate(a)?;
    supply(a)?;
    let g = &a.gas;
    if ![
        p.mass_absolute_tolerance_kg,
        p.mass_relative_tolerance,
        p.pressure_absolute_tolerance_pa,
        p.pressure_relative_tolerance,
    ]
    .iter()
    .all(|v| v.is_finite() && *v >= 0.)
        || p.maximum_iterations == 0
        || p.mass_absolute_tolerance_kg + p.mass_relative_tolerance == 0.
        || p.pressure_absolute_tolerance_pa + p.pressure_relative_tolerance == 0.
    {
        return Err(fail("invalid kinetic gas nonlinear policy"));
    }
    let n = g.volume_m3.len();
    let pref = g.reference_pressure_pa;
    let scale = gas::scales(g);
    if scale.iter().any(|v| !v.is_finite() || *v <= 0.) {
        return Err(fail("kinetic gas row scale"));
    }
    let guess = if g.base_port_pressure_pa.is_empty() {
        pref
    } else {
        g.base_port_pressure_pa
            .iter()
            .map(|v| v / g.base_port_pressure_pa.len() as f64)
            .sum()
    };
    let mut cell = g
        .volume_m3
        .iter()
        .zip(&g.previous_mass_kg)
        .map(|(v, m)| {
            if *v == 0. {
                0.
            } else if *m > 0. {
                m * pref / (g.reference_density_kg_m3 * v)
            } else {
                guess
            }
        })
        .collect::<Vec<_>>();
    let mut port = g.base_port_pressure_pa.clone();
    let mut factor: Option<SparseLu> = None;
    for iteration in 0..=p.maximum_iterations {
        let flux = records(a, &cell, &port, evaluator)?;
        let r = residual(a, &cell, &port, &flux)?;
        let j = jacobian(a, &flux, &scale)?.to_csc();
        if let Some(f) = &mut factor {
            if f.symbolic().matches(&j) {
                f.refactor(&j).map_err(|e| fail(e.to_string()))?;
            } else {
                factor = Some(SparseLu::new(&j).map_err(|e| fail(e.to_string()))?);
            }
        } else {
            factor = Some(SparseLu::new(&j).map_err(|e| fail(e.to_string()))?);
        }
        if gas::acceptable(g, p, &r) {
            let validity = validity(a, &cell, &port, &flux)?;
            let solution = output(a, cell, port, &flux, iteration)?;
            let lin = KineticGasMassLinearization {
                input: a.clone(),
                solution: solution.clone(),
                row_scale: scale,
                factor: factor
                    .take()
                    .ok_or_else(|| fail("kinetic factor missing"))?,
                flux,
                validity,
            };
            return Ok((solution, lin));
        }
        if iteration == p.maximum_iterations {
            break;
        }
        let rhs = r
            .iter()
            .zip(&scale)
            .map(|(r, s)| -r / s)
            .collect::<Vec<_>>();
        let dx = factor
            .as_ref()
            .ok_or_else(|| fail("kinetic factor missing"))?
            .solve(&rhs)
            .map_err(|e| fail(e.to_string()))?;
        let old = r
            .iter()
            .zip(&scale)
            .map(|(v, s)| (v / s).abs())
            .fold(0., f64::max);
        let mut fraction = 1_f64;
        for (i, v) in cell.iter().chain(&port).enumerate() {
            if dx[i] < 0. && *v > 0. {
                fraction = fraction.min(0.99 * v / (-pref * dx[i]));
            }
        }
        let mut accepted = false;
        for _ in 0..60 {
            let cp = cell
                .iter()
                .enumerate()
                .map(|(i, v)| v + fraction * pref * dx[i])
                .collect::<Vec<_>>();
            let pp = port
                .iter()
                .enumerate()
                .map(|(i, v)| v + fraction * pref * dx[n + i])
                .collect::<Vec<_>>();
            if let Ok(ff) = records(a, &cp, &pp, evaluator) {
                let rr = residual(a, &cp, &pp, &ff)?;
                let merit = rr
                    .iter()
                    .zip(&scale)
                    .map(|(v, s)| (v / s).abs())
                    .fold(0., f64::max);
                if merit < old || gas::acceptable(g, p, &rr) {
                    cell = cp;
                    port = pp;
                    accepted = true;
                    break;
                }
            }
            fraction *= 0.5;
        }
        if !accepted {
            return Err(fail(
                "kinetic gas Newton step did not reduce the conservation residual",
            ));
        }
    }
    Err(fail("kinetic gas mass solve did not converge"))
}
fn direction_domain<S: GasMassScalar>(
    a: &KineticGasMassInputs<S>,
    s: &GasMassSolution,
) -> CaeResult<()> {
    validate(a)?;
    gas::derivative_domain(&a.gas, &s.cell_pressure_pa, &s.port_pressure_pa)?;
    for (k, f) in a.kinetic_faces.iter().zip(&a.gas.faces) {
        if let Some(k) = k {
            if [k.gap_m, k.width_m, k.length_m, k.dynamic_viscosity_pa_s]
                .iter()
                .any(|v| !v.finite_first_order())
                || f.conductance_m3_pa_s.has_direction()
                || k.gap_m.value() == 0. && k.gap_m.has_direction()
            {
                return Err(fail(
                    "kinetic gas derivative crosses a sealed aperture or replacement contract",
                ));
            }
        }
    }
    Ok(())
}
fn flux_scalar<S: GasMassScalar>(
    a: &KineticGasMassInputs<S>,
    cell: &[S],
    port: &[S],
    flux: &[Flux],
) -> Vec<S> {
    let g = &a.gas;
    let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
    let rt = g.reference_pressure_pa / g.reference_density_kg_m3;
    let delta = |v: S| v - S::from_f64(v.value());
    g.faces
        .iter()
        .zip(&a.kinetic_faces)
        .zip(flux)
        .map(|((f, k), q)| {
            let r = if let Some(r) = f.right {
                cell[r]
            } else {
                port[f.port.unwrap_or(0)]
            };
            let mut out = S::from_f64(q.value)
                + delta(cell[f.left]) * q.left
                + delta(r) * q.right
                + delta(alpha) * q.alpha
                + delta(rt) * q.rt
                + delta(f.conductance_m3_pa_s) * q.g
                + delta(f.advection_m3_s) * q.a;
            if let Some(k) = k {
                out += delta(k.gap_m) * q.h
                    + delta(k.width_m) * q.width
                    + delta(k.length_m) * q.length
                    + delta(k.dynamic_viscosity_pa_s) * q.mu;
            }
            out
        })
        .collect()
}
fn residual_scalar<S: GasMassScalar>(
    a: &KineticGasMassInputs<S>,
    cell: &[S],
    port: &[S],
    flux: &[S],
) -> Vec<S> {
    let g = &a.gas;
    let n = cell.len();
    let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
    let mut r = cell
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if g.volume_m3[i].value() == 0. {
                *p
            } else {
                alpha * g.volume_m3[i] * *p - g.previous_mass_kg[i]
            }
        })
        .collect::<Vec<_>>();
    r.extend(
        port.iter()
            .zip(&g.base_port_pressure_pa)
            .map(|(p, b)| *p - *b),
    );
    for (face, q) in g.faces.iter().zip(flux) {
        r[face.left] += g.step_s * *q;
        if let Some(j) = face.right {
            r[j] -= g.step_s * *q;
        } else if let Some(p) = face.port {
            r[n + p] -= g.port_pressure_per_kg[p] * g.step_s * *q;
        }
    }
    r
}
impl KineticGasMassLinearization {
    pub fn validity(&self) -> &KineticGasMassValidity {
        &self.validity
    }
    pub fn solution(&self) -> &GasMassSolution {
        &self.solution
    }
    pub fn pushforward_scalar<S: GasMassScalar>(
        &self,
        a: &KineticGasMassInputs<S>,
    ) -> CaeResult<GasMassSolution<S>> {
        direction_domain(a, &self.solution)?;
        let primal = a.primal();
        if primal.gas.volume_m3 != self.input.gas.volume_m3
            || primal.gas.previous_mass_kg != self.input.gas.previous_mass_kg
            || primal.gas.base_port_pressure_pa != self.input.gas.base_port_pressure_pa
            || primal.gas.port_pressure_per_kg != self.input.gas.port_pressure_per_kg
            || primal.gas.reference_density_kg_m3 != self.input.gas.reference_density_kg_m3
            || primal.gas.reference_pressure_pa != self.input.gas.reference_pressure_pa
            || primal.gas.step_s != self.input.gas.step_s
            || primal.maximum_mean_flow_molecular_speed_ratio
                != self.input.maximum_mean_flow_molecular_speed_ratio
            || primal.gas.faces.len() != self.input.gas.faces.len()
            || primal.kinetic_faces.len() != self.input.kinetic_faces.len()
        {
            return Err(fail("kinetic gas linearization primal input changed"));
        }
        for (f, old) in primal.gas.faces.iter().zip(&self.input.gas.faces) {
            if f.left != old.left
                || f.right != old.right
                || f.port != old.port
                || f.conductance_m3_pa_s != old.conductance_m3_pa_s
                || f.advection_m3_s != old.advection_m3_s
            {
                return Err(fail("kinetic gas face linearization primal changed"));
            }
        }
        for (f, old) in primal.kinetic_faces.iter().zip(&self.input.kinetic_faces) {
            match (f, old) {
                (Some(f), Some(old))
                    if f.gap_m == old.gap_m
                        && f.width_m == old.width_m
                        && f.length_m == old.length_m
                        && f.dynamic_viscosity_pa_s == old.dynamic_viscosity_pa_s => {}
                (None, None) => {}
                _ => return Err(fail("kinetic gas material linearization primal changed")),
            }
        }
        let cp = self
            .solution
            .cell_pressure_pa
            .iter()
            .map(|v| S::from_f64(*v))
            .collect::<Vec<_>>();
        let pp = self
            .solution
            .port_pressure_pa
            .iter()
            .map(|v| S::from_f64(*v))
            .collect::<Vec<_>>();
        let flux = flux_scalar(a, &cp, &pp, &self.flux);
        let r = residual_scalar(a, &cp, &pp, &flux);
        let rhs = r
            .iter()
            .zip(&self.row_scale)
            .map(|(r, s)| -(*r - S::from_f64(r.value())) / *s)
            .collect::<Vec<_>>();
        let dx = S::implicit_solve(&self.factor, &rhs)?;
        let pref = self.input.gas.reference_pressure_pa;
        let cell = cp
            .iter()
            .enumerate()
            .map(|(i, v)| *v + dx[i] * pref)
            .collect::<Vec<_>>();
        let port = pp
            .iter()
            .enumerate()
            .map(|(i, v)| *v + dx[cp.len() + i] * pref)
            .collect::<Vec<_>>();
        let flux = flux_scalar(a, &cell, &port, &self.flux);
        let alpha = a.gas.reference_density_kg_m3 / a.gas.reference_pressure_pa;
        let mass = a
            .gas
            .volume_m3
            .iter()
            .zip(&cell)
            .map(|(v, p)| {
                if v.value() == 0. {
                    S::zero()
                } else {
                    alpha * *v * *p
                }
            })
            .collect();
        let result = GasMassSolution {
            cell_pressure_pa: cell,
            cell_mass_kg: mass,
            port_pressure_pa: port,
            face_mass_flux_kg_s: flux,
            closed_cells: self.solution.closed_cells.clone(),
            iterations: self.solution.iterations,
            maximum_mass_residual_kg: self.solution.maximum_mass_residual_kg,
            maximum_port_residual_pa: self.solution.maximum_port_residual_pa,
            mass_balance_defect_kg: self.solution.mass_balance_defect_kg,
        };
        gas::check_solution_first_order(&result)?;
        Ok(result)
    }
    pub fn pullback(&self, b: &GasMassOutputBars) -> CaeResult<KineticGasMassInputs> {
        let a = &self.input;
        let g = &a.gas;
        let s = &self.solution;
        let n = g.volume_m3.len();
        let np = g.base_port_pressure_pa.len();
        if b.cell_pressure_pa.len() != n
            || b.cell_mass_kg.len() != n
            || b.port_pressure_pa.len() != np
            || b.face_mass_flux_kg_s.len() != g.faces.len()
            || b.cell_pressure_pa
                .iter()
                .chain(&b.cell_mass_kg)
                .chain(&b.port_pressure_pa)
                .chain(&b.face_mass_flux_kg_s)
                .any(|v| !v.is_finite())
        {
            return Err(fail("kinetic gas output cotangent"));
        }
        let alpha = g.reference_density_kg_m3 / g.reference_pressure_pa;
        let mut bp = b.cell_pressure_pa.clone();
        bp.extend(&b.port_pressure_pa);
        let mut grad = KineticGasMassInputs {
            gas: GasMassInputs {
                volume_m3: vec![0.; n],
                previous_mass_kg: vec![0.; n],
                base_port_pressure_pa: vec![0.; np],
                port_pressure_per_kg: vec![0.; np],
                faces: g
                    .faces
                    .iter()
                    .map(|f| GasMassFace {
                        left: f.left,
                        right: f.right,
                        port: f.port,
                        conductance_m3_pa_s: 0.,
                        advection_m3_s: 0.,
                    })
                    .collect(),
                reference_density_kg_m3: 0.,
                reference_pressure_pa: 0.,
                step_s: 0.,
            },
            kinetic_faces: a
                .kinetic_faces
                .iter()
                .map(|k| {
                    k.as_ref().map(|_| KineticMassFace {
                        gap_m: 0.,
                        width_m: 0.,
                        length_m: 0.,
                        dynamic_viscosity_pa_s: 0.,
                    })
                })
                .collect(),
            maximum_mean_flow_molecular_speed_ratio: 0.,
        };
        let mut ba = 0.;
        let mut brt = 0.;
        for i in 0..n {
            if g.volume_m3[i] > 0. {
                bp[i] += b.cell_mass_kg[i] * alpha * g.volume_m3[i];
                grad.gas.volume_m3[i] += b.cell_mass_kg[i] * alpha * s.cell_pressure_pa[i];
                ba += b.cell_mass_kg[i] * g.volume_m3[i] * s.cell_pressure_pa[i];
            }
        }
        for (e, (f, q)) in g.faces.iter().zip(&self.flux).enumerate() {
            let j = f.right.unwrap_or(n + f.port.unwrap_or(0));
            bp[f.left] += b.face_mass_flux_kg_s[e] * q.left;
            bp[j] += b.face_mass_flux_kg_s[e] * q.right;
        }
        let lscaled = self
            .factor
            .solve_transpose(
                &bp.iter()
                    .map(|v| v * g.reference_pressure_pa)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| fail(e.to_string()))?;
        let lambda = lscaled
            .iter()
            .zip(&self.row_scale)
            .map(|(l, s)| l / s)
            .collect::<Vec<_>>();
        for i in 0..n {
            if g.volume_m3[i] > 0. {
                grad.gas.previous_mass_kg[i] += lambda[i];
                grad.gas.volume_m3[i] -= lambda[i] * alpha * s.cell_pressure_pa[i];
                ba -= lambda[i] * g.volume_m3[i] * s.cell_pressure_pa[i];
            }
        }
        for p in 0..np {
            grad.gas.base_port_pressure_pa[p] += lambda[n + p];
        }
        for (e, (f, q)) in g.faces.iter().zip(&self.flux).enumerate() {
            if !gas::active(g, f) {
                continue;
            }
            let pl = s.cell_pressure_pa[f.left];
            let pr = if let Some(r) = f.right {
                s.cell_pressure_pa[r]
            } else {
                s.port_pressure_pa[f.port.unwrap_or(0)]
            };
            let weight = if let Some(r) = f.right {
                lambda[f.left] - lambda[r]
            } else {
                let p = f.port.unwrap_or(0);
                lambda[f.left] - lambda[n + p] * g.port_pressure_per_kg[p]
            };
            let bf = b.face_mass_flux_kg_s[e] - g.step_s * weight;
            if f.advection_m3_s == 0. && pl != pr && bf != 0. {
                return Err(fail(
                    "kinetic gas advection reversal pullback is not unique",
                ));
            }
            grad.gas.faces[e].conductance_m3_pa_s += bf * q.g;
            grad.gas.faces[e].advection_m3_s += bf * q.a;
            ba += bf * q.alpha;
            brt += bf * q.rt;
            grad.gas.step_s -= weight * q.value;
            if let Some(p) = f.port {
                grad.gas.port_pressure_per_kg[p] += lambda[n + p] * g.step_s * q.value;
            }
            if let Some(k) = &mut grad.kinetic_faces[e] {
                k.gap_m += bf * q.h;
                k.width_m += bf * q.width;
                k.length_m += bf * q.length;
                k.dynamic_viscosity_pa_s += bf * q.mu;
            }
        }
        grad.gas.reference_density_kg_m3 = ba / g.reference_pressure_pa
            - brt * g.reference_pressure_pa / g.reference_density_kg_m3.powi(2);
        grad.gas.reference_pressure_pa = -ba * g.reference_density_kg_m3
            / g.reference_pressure_pa.powi(2)
            + brt / g.reference_density_kg_m3;
        if grad
            .gas
            .volume_m3
            .iter()
            .chain(&grad.gas.previous_mass_kg)
            .chain(&grad.gas.base_port_pressure_pa)
            .chain(&grad.gas.port_pressure_per_kg)
            .copied()
            .chain([
                grad.gas.reference_density_kg_m3,
                grad.gas.reference_pressure_pa,
                grad.gas.step_s,
            ])
            .chain(
                grad.gas
                    .faces
                    .iter()
                    .flat_map(|f| [f.conductance_m3_pa_s, f.advection_m3_s]),
            )
            .chain(
                grad.kinetic_faces
                    .iter()
                    .flatten()
                    .flat_map(|f| [f.gap_m, f.width_m, f.length_m, f.dynamic_viscosity_pa_s]),
            )
            .any(|v| !v.is_finite())
        {
            return Err(fail("kinetic gas cotangent is not finite"));
        }
        Ok(grad)
    }
}
pub fn solve_scalar<S: GasMassScalar>(
    a: &KineticGasMassInputs<S>,
    policy: &GasMassPolicy,
    evaluator: &mut KineticMobilityEvaluator,
) -> CaeResult<GasMassSolution<S>> {
    let (_, linearization) = solve(&a.primal(), policy, evaluator)?;
    linearization.pushforward_scalar(a)
}
