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

use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{lu::SparseLu, sparse::CsrMatrix};
use serde::{Deserialize, Serialize};

fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasMassFace<S = f64> {
    pub left: usize,
    pub right: Option<usize>,
    pub port: Option<usize>,
    pub conductance_m3_pa_s: S,
    pub advection_m3_s: S,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasMassInputs<S = f64> {
    pub volume_m3: Vec<S>,
    pub previous_mass_kg: Vec<S>,
    pub base_port_pressure_pa: Vec<S>,
    pub port_pressure_per_kg: Vec<S>,
    pub faces: Vec<GasMassFace<S>>,
    pub reference_density_kg_m3: S,
    pub reference_pressure_pa: S,
    pub step_s: S,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasMassPolicy {
    pub mass_absolute_tolerance_kg: f64,
    pub mass_relative_tolerance: f64,
    pub pressure_absolute_tolerance_pa: f64,
    pub pressure_relative_tolerance: f64,
    pub maximum_iterations: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct GasMassSolution<S = f64> {
    pub cell_pressure_pa: Vec<S>,
    pub cell_mass_kg: Vec<S>,
    pub port_pressure_pa: Vec<S>,
    pub face_mass_flux_kg_s: Vec<S>,
    pub closed_cells: Vec<bool>,
    pub iterations: usize,
    pub maximum_mass_residual_kg: f64,
    pub maximum_port_residual_pa: f64,
    pub mass_balance_defect_kg: f64,
}
pub struct GasMassLinearization {
    input: GasMassInputs,
    solution: GasMassSolution,
    row_scale: Vec<f64>,
    factor: SparseLu,
}
#[derive(Clone, Debug)]
pub struct GasMassOutputBars {
    pub cell_pressure_pa: Vec<f64>,
    pub cell_mass_kg: Vec<f64>,
    pub port_pressure_pa: Vec<f64>,
    pub face_mass_flux_kg_s: Vec<f64>,
}
pub trait GasMassScalar: Scalar {
    fn try_first_order_lift(
        _value: f64,
        _inputs: &[Self],
        _partials: &[f64],
    ) -> CaeResult<Option<Self>> {
        Ok(None)
    }
    fn implicit_solve(factor: &SparseLu, rhs: &[Self]) -> CaeResult<Vec<Self>>;
    fn has_direction(self) -> bool;
    fn finite_first_order(self) -> bool;
}
impl GasMassScalar for f64 {
    fn try_first_order_lift(
        value: f64,
        inputs: &[Self],
        partials: &[f64],
    ) -> CaeResult<Option<Self>> {
        first_order_affine_lift(value, inputs, partials).map(Some)
    }
    fn implicit_solve(factor: &SparseLu, rhs: &[Self]) -> CaeResult<Vec<Self>> {
        factor.solve(rhs).map_err(|e| fail(e.to_string()))
    }
    fn has_direction(self) -> bool {
        false
    }
    fn finite_first_order(self) -> bool {
        self.is_finite()
    }
}
impl<const N: usize> GasMassScalar for Dual<N> {
    fn try_first_order_lift(
        value: f64,
        inputs: &[Self],
        partials: &[f64],
    ) -> CaeResult<Option<Self>> {
        first_order_affine_lift(value, inputs, partials).map(Some)
    }
    fn implicit_solve(factor: &SparseLu, rhs: &[Self]) -> CaeResult<Vec<Self>> {
        let re = factor
            .solve(&rhs.iter().map(|x| x.re).collect::<Vec<_>>())
            .map_err(|e| fail(e.to_string()))?;
        let mut result = re.into_iter().map(Dual::constant).collect::<Vec<_>>();
        for k in 0..N {
            let eps = factor
                .solve(&rhs.iter().map(|x| x.eps[k]).collect::<Vec<_>>())
                .map_err(|e| fail(e.to_string()))?;
            for (i, x) in result.iter_mut().enumerate() {
                x.eps[k] = eps[i];
            }
        }
        Ok(result)
    }
    fn has_direction(self) -> bool {
        self.eps.iter().any(|v| *v != 0.)
    }
    fn finite_first_order(self) -> bool {
        self.re.is_finite() && self.eps.iter().all(|v| v.is_finite())
    }
}
impl<S: Scalar> GasMassInputs<S> {
    pub fn primal(&self) -> GasMassInputs {
        GasMassInputs {
            volume_m3: self.volume_m3.iter().map(Scalar::value).collect(),
            previous_mass_kg: self.previous_mass_kg.iter().map(Scalar::value).collect(),
            base_port_pressure_pa: self
                .base_port_pressure_pa
                .iter()
                .map(Scalar::value)
                .collect(),
            port_pressure_per_kg: self
                .port_pressure_per_kg
                .iter()
                .map(Scalar::value)
                .collect(),
            faces: self
                .faces
                .iter()
                .map(|f| GasMassFace {
                    left: f.left,
                    right: f.right,
                    port: f.port,
                    conductance_m3_pa_s: f.conductance_m3_pa_s.value(),
                    advection_m3_s: f.advection_m3_s.value(),
                })
                .collect(),
            reference_density_kg_m3: self.reference_density_kg_m3.value(),
            reference_pressure_pa: self.reference_pressure_pa.value(),
            step_s: self.step_s.value(),
        }
    }
}
pub(crate) fn validate<S: Scalar>(a: &GasMassInputs<S>) -> CaeResult<()> {
    let n = a.volume_m3.len();
    let np = a.base_port_pressure_pa.len();
    if n == 0
        || a.previous_mass_kg.len() != n
        || a.port_pressure_per_kg.len() != np
        || ![
            a.reference_density_kg_m3.value(),
            a.reference_pressure_pa.value(),
            a.step_s.value(),
        ]
        .iter()
        .all(|v| v.is_finite() && *v > 0.)
        || a.volume_m3
            .iter()
            .chain(&a.previous_mass_kg)
            .chain(&a.port_pressure_per_kg)
            .any(|v| !v.value().is_finite() || v.value() < 0.)
        || a.base_port_pressure_pa
            .iter()
            .any(|v| !v.value().is_finite() || v.value() <= 0.)
        || a.faces.iter().any(|f| {
            f.left >= n
                || f.right.is_some_and(|r| r >= n || r == f.left)
                || f.port.is_some_and(|p| p >= np)
                || f.right.is_some() == f.port.is_some()
                || !f.conductance_m3_pa_s.value().is_finite()
                || f.conductance_m3_pa_s.value() < 0.
                || !f.advection_m3_s.value().is_finite()
        })
    {
        return Err(fail("invalid conservative gas network input"));
    }
    for (i, v) in a.volume_m3.iter().enumerate() {
        if v.value() == 0. && a.previous_mass_kg[i].value() != 0. {
            return Err(fail(format!(
                "gas cell {i} closes with trapped mass; drainage or contact event must be resolved before closure"
            )));
        }
    }
    Ok(())
}
pub(crate) fn active<S: Scalar>(a: &GasMassInputs<S>, f: &GasMassFace<S>) -> bool {
    a.volume_m3[f.left].value() > 0. && f.right.is_none_or(|r| a.volume_m3[r].value() > 0.)
}
pub fn face_mass_fluxes<S: Scalar>(
    a: &GasMassInputs<S>,
    cell: &[S],
    port: &[S],
) -> CaeResult<Vec<S>> {
    validate(a)?;
    if cell.len() != a.volume_m3.len()
        || port.len() != a.base_port_pressure_pa.len()
        || cell.iter().enumerate().any(|(i, p)| {
            !p.value().is_finite()
                || if a.volume_m3[i].value() == 0. {
                    p.value() != 0.
                } else {
                    p.value() <= 0.
                }
        })
        || port
            .iter()
            .any(|p| !p.value().is_finite() || p.value() <= 0.)
    {
        return Err(fail("gas pressure is outside its open or closed domain"));
    }
    let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
    let result = a
        .faces
        .iter()
        .map(|f| {
            if !active(a, f) {
                return S::zero();
            }
            let l = cell[f.left];
            let r = if let Some(i) = f.right {
                cell[i]
            } else {
                port[f.port.unwrap_or(0)]
            };
            let donor = if f.advection_m3_s.value() >= 0. { l } else { r };
            alpha * (f.conductance_m3_pa_s * (l - r) * (l + r) * 0.5 + f.advection_m3_s * donor)
        })
        .collect::<Vec<_>>();
    if result.iter().any(|v| !v.value().is_finite()) {
        return Err(fail("gas face mass flux is not finite"));
    }
    Ok(result)
}
pub fn residual<S: Scalar>(a: &GasMassInputs<S>, cell: &[S], port: &[S]) -> CaeResult<Vec<S>> {
    let flux = face_mass_fluxes(a, cell, port)?;
    let n = a.volume_m3.len();
    let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
    let mut r = (0..n)
        .map(|i| {
            if a.volume_m3[i].value() == 0. {
                cell[i]
            } else {
                alpha * a.volume_m3[i] * cell[i] - a.previous_mass_kg[i]
            }
        })
        .collect::<Vec<_>>();
    r.extend(
        port.iter()
            .zip(&a.base_port_pressure_pa)
            .map(|(p, b)| *p - *b),
    );
    for (f, q) in a.faces.iter().zip(flux) {
        if !active(a, f) {
            continue;
        }
        r[f.left] += a.step_s * q;
        if let Some(i) = f.right {
            r[i] -= a.step_s * q;
        } else if let Some(p) = f.port {
            r[n + p] -= a.port_pressure_per_kg[p] * a.step_s * q;
        }
    }
    if r.iter().any(|v| !v.value().is_finite()) {
        return Err(fail("gas conservation residual is not finite"));
    }
    Ok(r)
}
pub(crate) fn supply_check(a: &GasMassInputs) -> CaeResult<()> {
    let n = a.volume_m3.len();
    let mut adjacent = vec![Vec::new(); n];
    let mut supplied = a
        .previous_mass_kg
        .iter()
        .map(|m| *m > 0.)
        .collect::<Vec<_>>();
    for f in &a.faces {
        if !active(a, f) {
            continue;
        }
        if let Some(r) = f.right {
            if f.conductance_m3_pa_s > 0. || f.advection_m3_s > 0. {
                adjacent[f.left].push(r);
            }
            if f.conductance_m3_pa_s > 0. || f.advection_m3_s < 0. {
                adjacent[r].push(f.left);
            }
        } else if f.conductance_m3_pa_s > 0. || f.advection_m3_s < 0. {
            supplied[f.left] = true;
        }
    }
    let mut stack = supplied
        .iter()
        .enumerate()
        .filter_map(|(i, s)| s.then_some(i))
        .collect::<Vec<_>>();
    while let Some(j) = stack.pop() {
        for &k in &adjacent[j] {
            if !supplied[k] {
                supplied[k] = true;
                stack.push(k);
            }
        }
    }
    if a.volume_m3
        .iter()
        .enumerate()
        .any(|(i, v)| *v > 0. && !supplied[i])
    {
        return Err(fail(
            "opening gas cell has no initial mass or incoming reservoir path; positive pressure is unavailable",
        ));
    }
    Ok(())
}
pub(crate) fn scales(a: &GasMassInputs) -> Vec<f64> {
    a.volume_m3
        .iter()
        .zip(&a.previous_mass_kg)
        .map(|(v, m)| {
            if *v == 0. {
                a.reference_pressure_pa
            } else {
                (a.reference_density_kg_m3 * v).max(*m)
            }
        })
        .chain(
            a.base_port_pressure_pa
                .iter()
                .map(|_| a.reference_pressure_pa),
        )
        .collect()
}
fn jacobian(a: &GasMassInputs, cell: &[f64], port: &[f64], scale: &[f64]) -> CaeResult<CsrMatrix> {
    let n = cell.len();
    let nt = n + port.len();
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
    for (i, v) in a.volume_m3.iter().enumerate() {
        add(i, i, if *v == 0. { 1. } else { alpha * v });
    }
    for p in 0..port.len() {
        add(n + p, n + p, 1.);
    }
    for f in &a.faces {
        if !active(a, f) {
            continue;
        }
        let j = f.right.unwrap_or(n + f.port.unwrap_or(0));
        let pl = cell[f.left];
        let pr = if let Some(r) = f.right {
            cell[r]
        } else {
            port[f.port.unwrap_or(0)]
        };
        let dl = alpha
            * (f.conductance_m3_pa_s * pl
                + if f.advection_m3_s >= 0. {
                    f.advection_m3_s
                } else {
                    0.
                });
        let dr = alpha
            * (-f.conductance_m3_pa_s * pr
                + if f.advection_m3_s < 0. {
                    f.advection_m3_s
                } else {
                    0.
                });
        add(f.left, f.left, a.step_s * dl);
        add(f.left, j, a.step_s * dr);
        if let Some(r) = f.right {
            add(r, f.left, -a.step_s * dl);
            add(r, j, -a.step_s * dr);
        } else if let Some(p) = f.port {
            let g = -a.port_pressure_per_kg[p] * a.step_s;
            add(n + p, f.left, g * dl);
            add(n + p, j, g * dr);
        }
    }
    CsrMatrix::from_triplets(nt, nt, &rows, &cols, &vals).map_err(|e| fail(e.to_string()))
}
pub(crate) fn acceptable(a: &GasMassInputs, p: &GasMassPolicy, r: &[f64]) -> bool {
    let n = a.volume_m3.len();
    (0..n).all(|i| {
        if a.volume_m3[i] == 0. {
            r[i] == 0.
        } else {
            r[i].abs()
                <= p.mass_absolute_tolerance_kg
                    + p.mass_relative_tolerance
                        * (a.reference_density_kg_m3 * a.volume_m3[i]).max(a.previous_mass_kg[i])
        }
    }) && (0..a.base_port_pressure_pa.len()).all(|i| {
        r[n + i].abs()
            <= p.pressure_absolute_tolerance_pa
                + p.pressure_relative_tolerance * a.base_port_pressure_pa[i]
    })
}
fn outputs<S: Scalar>(
    a: &GasMassInputs<S>,
    cell: Vec<S>,
    port: Vec<S>,
    iterations: usize,
) -> CaeResult<GasMassSolution<S>> {
    let f = face_mass_fluxes(a, &cell, &port)?;
    let r = residual(a, &cell, &port)?;
    let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
    let mass = a
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
        .collect::<Vec<_>>();
    if mass
        .iter()
        .enumerate()
        .any(|(i, m)| !m.value().is_finite() || a.volume_m3[i].value() > 0. && m.value() <= 0.)
    {
        return Err(fail("open gas mass is not positive and representable"));
    }
    let boundary = a
        .faces
        .iter()
        .zip(&f)
        .filter(|(face, _)| face.port.is_some())
        .map(|(_, f)| f.value())
        .sum::<f64>();
    let defect = mass.iter().map(Scalar::value).sum::<f64>()
        - a.previous_mass_kg.iter().map(Scalar::value).sum::<f64>()
        + a.step_s.value() * boundary;
    if !defect.is_finite() {
        return Err(fail("gas mass ledger is not finite"));
    }
    Ok(GasMassSolution {
        cell_pressure_pa: cell,
        cell_mass_kg: mass,
        port_pressure_pa: port,
        face_mass_flux_kg_s: f,
        closed_cells: a.volume_m3.iter().map(|v| v.value() == 0.).collect(),
        iterations,
        maximum_mass_residual_kg: r[..a.volume_m3.len()]
            .iter()
            .map(|v| v.value().abs())
            .fold(0., f64::max),
        maximum_port_residual_pa: r[a.volume_m3.len()..]
            .iter()
            .map(|v| v.value().abs())
            .fold(0., f64::max),
        mass_balance_defect_kg: defect,
    })
}
pub fn solve(
    a: &GasMassInputs,
    p: &GasMassPolicy,
) -> CaeResult<(GasMassSolution, GasMassLinearization)> {
    validate(a)?;
    supply_check(a)?;
    if ![
        p.mass_absolute_tolerance_kg,
        p.mass_relative_tolerance,
        p.pressure_absolute_tolerance_pa,
        p.pressure_relative_tolerance,
    ]
    .iter()
    .all(|v| v.is_finite() && *v >= 0.)
        || p.maximum_iterations == 0
        || p.mass_relative_tolerance == 0. && p.mass_absolute_tolerance_kg == 0.
        || p.pressure_relative_tolerance == 0. && p.pressure_absolute_tolerance_pa == 0.
    {
        return Err(fail("invalid gas nonlinear solver policy"));
    }
    let n = a.volume_m3.len();
    let pref = a.reference_pressure_pa;
    let scale = scales(a);
    if scale.iter().any(|v| !v.is_finite() || *v <= 0.) {
        return Err(fail("gas nonlinear row scale is not positive"));
    }
    let guess = if a.base_port_pressure_pa.is_empty() {
        pref
    } else {
        a.base_port_pressure_pa.iter().sum::<f64>() / a.base_port_pressure_pa.len() as f64
    };
    let mut cell = a
        .volume_m3
        .iter()
        .zip(&a.previous_mass_kg)
        .map(|(v, m)| {
            if *v == 0. {
                0.
            } else if *m > 0. {
                m * pref / (a.reference_density_kg_m3 * v)
            } else {
                guess
            }
        })
        .collect::<Vec<_>>();
    let mut port = a.base_port_pressure_pa.clone();
    let mut cached: Option<SparseLu> = None;
    for iteration in 0..=p.maximum_iterations {
        let r = residual(a, &cell, &port)?;
        let j = jacobian(a, &cell, &port, &scale)?.to_csc();
        if let Some(lu) = &mut cached {
            if lu.symbolic().matches(&j) {
                lu.refactor(&j).map_err(|e| fail(e.to_string()))?;
            } else {
                cached = Some(SparseLu::new(&j).map_err(|e| fail(e.to_string()))?);
            }
        } else {
            cached = Some(SparseLu::new(&j).map_err(|e| fail(e.to_string()))?);
        }
        if acceptable(a, p, &r) {
            let solution = outputs(a, cell, port, iteration)?;
            let factor = cached
                .take()
                .ok_or_else(|| fail("missing gas linearization"))?;
            let lin = GasMassLinearization {
                input: a.clone(),
                solution: solution.clone(),
                row_scale: scale,
                factor,
            };
            return Ok((solution, lin));
        }
        if iteration == p.maximum_iterations {
            break;
        }
        let rhs = r
            .iter()
            .zip(&scale)
            .map(|(v, s)| -v / s)
            .collect::<Vec<_>>();
        let dx = cached
            .as_ref()
            .ok_or_else(|| fail("missing gas nonlinear factor"))?
            .solve(&rhs)
            .map_err(|e| fail(e.to_string()))?;
        let old = r
            .iter()
            .zip(&scale)
            .map(|(v, s)| (v / s).abs())
            .fold(0., f64::max);
        let mut fraction = 1_f64;
        for (i, x) in cell.iter().chain(&port).enumerate() {
            if dx[i] < 0. && *x > 0. {
                fraction = fraction.min(0.99 * x / (-pref * dx[i]));
            }
        }
        let mut accepted = false;
        for _ in 0..60 {
            let c = cell
                .iter()
                .enumerate()
                .map(|(i, v)| v + fraction * pref * dx[i])
                .collect::<Vec<_>>();
            let pp = port
                .iter()
                .enumerate()
                .map(|(i, v)| v + fraction * pref * dx[n + i])
                .collect::<Vec<_>>();
            if let Ok(rr) = residual(a, &c, &pp) {
                let merit = rr
                    .iter()
                    .zip(&scale)
                    .map(|(v, s)| (v / s).abs())
                    .fold(0., f64::max);
                if merit < old || acceptable(a, p, &rr) {
                    cell = c;
                    port = pp;
                    accepted = true;
                    break;
                }
            }
            fraction *= 0.5;
        }
        if !accepted {
            return Err(fail(
                "positive-pressure gas Newton step did not reduce conservation residual",
            ));
        }
    }
    Err(fail("conservative gas mass solve did not converge"))
}
pub(crate) fn derivative_domain<S: GasMassScalar>(
    a: &GasMassInputs<S>,
    cell: &[f64],
    port: &[f64],
) -> CaeResult<()> {
    if a.volume_m3
        .iter()
        .chain(&a.previous_mass_kg)
        .chain(&a.base_port_pressure_pa)
        .chain(&a.port_pressure_per_kg)
        .copied()
        .chain([a.reference_density_kg_m3, a.reference_pressure_pa, a.step_s])
        .chain(
            a.faces
                .iter()
                .flat_map(|f| [f.conductance_m3_pa_s, f.advection_m3_s]),
        )
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("nonfinite gas first-order input"));
    }
    for (i, v) in a.volume_m3.iter().enumerate() {
        if v.value() == 0. && (v.has_direction() || a.previous_mass_kg[i].has_direction()) {
            return Err(fail(
                "closed gas cell opening or trapped-mass derivative requires an active-set event",
            ));
        }
    }
    for f in &a.faces {
        if !active(a, f) {
            continue;
        }
        let r = if let Some(r) = f.right {
            cell[r]
        } else {
            port[f.port.unwrap_or(0)]
        };
        if f.advection_m3_s.value() == 0. && f.advection_m3_s.has_direction() && cell[f.left] != r {
            return Err(fail(
                "gas upwind derivative is undefined at zero advection and unequal densities",
            ));
        }
    }
    Ok(())
}
pub fn solve_scalar<S: GasMassScalar>(
    a: &GasMassInputs<S>,
    p: &GasMassPolicy,
) -> CaeResult<GasMassSolution<S>> {
    let (solution, lin) = solve(&a.primal(), p)?;
    derivative_domain(a, &solution.cell_pressure_pa, &solution.port_pressure_pa)?;
    let cell = solution
        .cell_pressure_pa
        .iter()
        .map(|v| S::from_f64(*v))
        .collect::<Vec<_>>();
    let port = solution
        .port_pressure_pa
        .iter()
        .map(|v| S::from_f64(*v))
        .collect::<Vec<_>>();
    let r = residual(a, &cell, &port)?;
    let rhs = r
        .iter()
        .zip(&lin.row_scale)
        .map(|(v, s)| -(*v - S::from_f64(v.value())) / *s)
        .collect::<Vec<_>>();
    let dx = S::implicit_solve(&lin.factor, &rhs)?;
    let pref = lin.input.reference_pressure_pa;
    let cp = cell
        .iter()
        .enumerate()
        .map(|(i, v)| *v + dx[i] * pref)
        .collect();
    let pp = port
        .iter()
        .enumerate()
        .map(|(i, v)| *v + dx[cell.len() + i] * pref)
        .collect();
    let result = outputs(a, cp, pp, solution.iterations)?;
    check_solution_first_order(&result)?;
    Ok(result)
}
pub(crate) fn check_solution_first_order<S: GasMassScalar>(
    s: &GasMassSolution<S>,
) -> CaeResult<()> {
    if s.cell_pressure_pa
        .iter()
        .chain(&s.cell_mass_kg)
        .chain(&s.port_pressure_pa)
        .chain(&s.face_mass_flux_kg_s)
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("gas first-order solution is not finite"));
    }
    Ok(())
}
impl GasMassLinearization {
    pub fn solution(&self) -> &GasMassSolution {
        &self.solution
    }
    pub fn pushforward(&self, d: &GasMassInputs) -> CaeResult<GasMassSolution<Dual<1>>> {
        if d.volume_m3.len() != self.input.volume_m3.len()
            || d.previous_mass_kg.len() != self.input.previous_mass_kg.len()
            || d.base_port_pressure_pa.len() != self.input.base_port_pressure_pa.len()
            || d.port_pressure_per_kg.len() != self.input.port_pressure_per_kg.len()
            || d.faces.len() != self.input.faces.len()
            || d.faces
                .iter()
                .zip(&self.input.faces)
                .any(|(d, f)| d.left != f.left || d.right != f.right || d.port != f.port)
        {
            return Err(fail("gas input direction shape"));
        }
        let lift = |v: f64, d: f64| Dual::new(v, [d]);
        let a = &self.input;
        let input = GasMassInputs {
            volume_m3: a
                .volume_m3
                .iter()
                .zip(&d.volume_m3)
                .map(|(v, d)| lift(*v, *d))
                .collect(),
            previous_mass_kg: a
                .previous_mass_kg
                .iter()
                .zip(&d.previous_mass_kg)
                .map(|(v, d)| lift(*v, *d))
                .collect(),
            base_port_pressure_pa: a
                .base_port_pressure_pa
                .iter()
                .zip(&d.base_port_pressure_pa)
                .map(|(v, d)| lift(*v, *d))
                .collect(),
            port_pressure_per_kg: a
                .port_pressure_per_kg
                .iter()
                .zip(&d.port_pressure_per_kg)
                .map(|(v, d)| lift(*v, *d))
                .collect(),
            faces: a
                .faces
                .iter()
                .zip(&d.faces)
                .map(|(f, d)| GasMassFace {
                    left: f.left,
                    right: f.right,
                    port: f.port,
                    conductance_m3_pa_s: lift(f.conductance_m3_pa_s, d.conductance_m3_pa_s),
                    advection_m3_s: lift(f.advection_m3_s, d.advection_m3_s),
                })
                .collect(),
            reference_density_kg_m3: lift(a.reference_density_kg_m3, d.reference_density_kg_m3),
            reference_pressure_pa: lift(a.reference_pressure_pa, d.reference_pressure_pa),
            step_s: lift(a.step_s, d.step_s),
        };
        derivative_domain(
            &input,
            &self.solution.cell_pressure_pa,
            &self.solution.port_pressure_pa,
        )?;
        let cell = self
            .solution
            .cell_pressure_pa
            .iter()
            .map(|v| Dual::constant(*v))
            .collect::<Vec<_>>();
        let port = self
            .solution
            .port_pressure_pa
            .iter()
            .map(|v| Dual::constant(*v))
            .collect::<Vec<_>>();
        let r = residual(&input, &cell, &port)?;
        let rhs = r
            .iter()
            .zip(&self.row_scale)
            .map(|(v, s)| -v.eps[0] / s)
            .collect::<Vec<_>>();
        let dx = self.factor.solve(&rhs).map_err(|e| fail(e.to_string()))?;
        let pref = a.reference_pressure_pa;
        let result = outputs(
            &input,
            cell.iter()
                .enumerate()
                .map(|(i, v)| lift(v.re, pref * dx[i]))
                .collect(),
            port.iter()
                .enumerate()
                .map(|(i, v)| lift(v.re, pref * dx[cell.len() + i]))
                .collect(),
            self.solution.iterations,
        )?;
        check_solution_first_order(&result)?;
        Ok(result)
    }
    pub fn pullback(&self, b: &GasMassOutputBars) -> CaeResult<GasMassInputs> {
        let a = &self.input;
        let n = a.volume_m3.len();
        let np = a.base_port_pressure_pa.len();
        let nf = a.faces.len();
        if b.cell_pressure_pa.len() != n
            || b.cell_mass_kg.len() != n
            || b.port_pressure_pa.len() != np
            || b.face_mass_flux_kg_s.len() != nf
            || b.cell_pressure_pa
                .iter()
                .chain(&b.cell_mass_kg)
                .chain(&b.port_pressure_pa)
                .chain(&b.face_mass_flux_kg_s)
                .any(|v| !v.is_finite())
        {
            return Err(fail("gas output cotangent shape or values"));
        }
        let alpha = a.reference_density_kg_m3 / a.reference_pressure_pa;
        let s = &self.solution;
        let mut bp = b.cell_pressure_pa.clone();
        bp.extend(&b.port_pressure_pa);
        let mut g = GasMassInputs {
            volume_m3: vec![0.; n],
            previous_mass_kg: vec![0.; n],
            base_port_pressure_pa: vec![0.; np],
            port_pressure_per_kg: vec![0.; np],
            faces: a
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
        };
        let mut ba = 0.;
        for i in 0..n {
            if a.volume_m3[i] > 0. {
                bp[i] += b.cell_mass_kg[i] * alpha * a.volume_m3[i];
                g.volume_m3[i] += b.cell_mass_kg[i] * alpha * s.cell_pressure_pa[i];
                ba += b.cell_mass_kg[i] * a.volume_m3[i] * s.cell_pressure_pa[i];
            }
        }
        for (e, f) in a.faces.iter().enumerate() {
            if !active(a, f) {
                continue;
            }
            let j = f.right.unwrap_or(n + f.port.unwrap_or(0));
            let l = s.cell_pressure_pa[f.left];
            let r = if let Some(j) = f.right {
                s.cell_pressure_pa[j]
            } else {
                s.port_pressure_pa[f.port.unwrap_or(0)]
            };
            bp[f.left] += b.face_mass_flux_kg_s[e]
                * alpha
                * (f.conductance_m3_pa_s * l
                    + if f.advection_m3_s >= 0. {
                        f.advection_m3_s
                    } else {
                        0.
                    });
            bp[j] += b.face_mass_flux_kg_s[e]
                * alpha
                * (-f.conductance_m3_pa_s * r
                    + if f.advection_m3_s < 0. {
                        f.advection_m3_s
                    } else {
                        0.
                    });
        }
        let lambda_scaled = self
            .factor
            .solve_transpose(
                &bp.iter()
                    .map(|v| v * a.reference_pressure_pa)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| fail(e.to_string()))?;
        let lambda = lambda_scaled
            .iter()
            .zip(&self.row_scale)
            .map(|(v, s)| v / s)
            .collect::<Vec<_>>();
        for i in 0..n {
            if a.volume_m3[i] > 0. {
                g.previous_mass_kg[i] += lambda[i];
                g.volume_m3[i] -= lambda[i] * alpha * s.cell_pressure_pa[i];
                ba -= lambda[i] * a.volume_m3[i] * s.cell_pressure_pa[i];
            }
        }
        for p in 0..np {
            g.base_port_pressure_pa[p] += lambda[n + p];
        }
        for (e, f) in a.faces.iter().enumerate() {
            if !active(a, f) {
                continue;
            }
            let l = s.cell_pressure_pa[f.left];
            let r = if let Some(j) = f.right {
                s.cell_pressure_pa[j]
            } else {
                s.port_pressure_pa[f.port.unwrap_or(0)]
            };
            let weight = if let Some(j) = f.right {
                lambda[f.left] - lambda[j]
            } else {
                let p = f.port.unwrap_or(0);
                lambda[f.left] - lambda[n + p] * a.port_pressure_per_kg[p]
            };
            let bf = b.face_mass_flux_kg_s[e] - a.step_s * weight;
            let donor = if f.advection_m3_s >= 0. { l } else { r };
            if f.advection_m3_s == 0. && l != r && bf != 0. {
                return Err(fail(
                    "gas advection pullback at unequal-density reversal is not unique",
                ));
            }
            g.faces[e].conductance_m3_pa_s += bf * alpha * (l - r) * (l + r) * 0.5;
            g.faces[e].advection_m3_s += bf * alpha * donor;
            ba += bf * (f.conductance_m3_pa_s * (l - r) * (l + r) * 0.5 + f.advection_m3_s * donor);
            g.step_s -= weight * s.face_mass_flux_kg_s[e];
            if let Some(p) = f.port {
                g.port_pressure_per_kg[p] += lambda[n + p] * a.step_s * s.face_mass_flux_kg_s[e];
            }
        }
        g.reference_density_kg_m3 = ba / a.reference_pressure_pa;
        g.reference_pressure_pa =
            -ba * a.reference_density_kg_m3 / (a.reference_pressure_pa * a.reference_pressure_pa);
        Ok(g)
    }
}

fn first_order_affine_lift<S: GasMassScalar>(
    value: f64,
    inputs: &[S],
    partials: &[f64],
) -> CaeResult<S> {
    if !value.is_finite()
        || inputs.len() != partials.len()
        || inputs.iter().any(|v| !v.finite_first_order())
        || partials.iter().any(|v| !v.is_finite())
    {
        return Err(fail("invalid first-order scalar primitive"));
    }
    let result = inputs
        .iter()
        .zip(partials)
        .fold(S::from_f64(value), |v, (a, p)| {
            v + (*a - S::from_f64(a.value())) * *p
        });
    if !result.finite_first_order() {
        return Err(fail("nonfinite first-order scalar primitive"));
    }
    Ok(result)
}
