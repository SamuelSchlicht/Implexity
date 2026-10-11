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

use crate::gas_gap_mass::GasMassScalar;
use implexity_ad::{Dual, Scalar};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::{
    CsrMatrix,
    dense::{DenseLu, DenseMatrix},
    lu::SparseLu,
};
use serde::{Deserialize, Serialize};
pub(crate) fn geometry_conditioning_product(values:&[f64])->Option<f64>{
    if values.iter().any(|v|!v.is_finite()||*v<0.){return None;}
    if values.contains(&0.){return Some(0.);}
    let mut product=1.;
    for &value in values{let next=product*value;if !next.is_finite()||next==0.{return None;}product=next;}
    Some(product)
}
pub trait TransientGasScalar: GasMassScalar {
    fn geometry_has_time_channel() -> bool { false }
    fn supports_parallel_face_evaluation() -> bool { false }
    fn lift_geometry_partial(value: f64, inputs: &[Self], partials: &[f64]) -> CaeResult<Self> {
        if !value.is_finite()
            || inputs.len() != partials.len()
            || inputs.iter().any(|v| !v.finite_first_order())
            || partials.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "geometry primitive value or partials invalid",
            ));
        }
        let mut output = Self::from_f64(value);
        for (input, partial) in inputs.iter().zip(partials) {
            output = output + (*input - Self::from_f64(input.value())) * *partial;
        }
        if !output.finite_first_order() {
            return Err(CaeError::contract(
                "geometry primitive first-order overflow",
            ));
        }
        Ok(output)
    }
    fn geometry_product_direction_bounds(self, other: Self) -> Option<Vec<f64>> {
        let mut a = self.geometry_first_order_directions()?;
        let mut b = other.geometry_first_order_directions()?;
        let dimension = a.len().max(b.len());
        if (!a.is_empty() && a.len()!=dimension) || (!b.is_empty() && b.len()!=dimension) {return None;}
        a.resize(dimension,0.); b.resize(dimension,0.);
        Some(a.iter().zip(&b).map(|(a,b)|
            a.abs()*other.value().abs()+self.value().abs()*b.abs()).collect())
    }
    fn geometry_product_factors_direction_bounds(factors:&[Self])->Option<Vec<f64>>{
        if factors.iter().any(|v|!v.finite_first_order()){return None;}
        let rows=factors.iter().map(|v|v.geometry_first_order_directions()).collect::<Option<Vec<_>>>()?;
        let width=rows.iter().map(Vec::len).max().unwrap_or(0);
        if rows.iter().zip(factors).any(|(row,factor)|row.iter().any(|v|!v.is_finite())||(row.len()!=width&&!(row.is_empty()&&!factor.has_direction()))){return None;}
        let mut bounds=vec![0.;width];
        for k in 0..width{for i in 0..factors.len(){
            let values=(0..factors.len()).map(|j|if j==i{rows[j].get(k).copied().unwrap_or(0.).abs()}else{factors[j].value().abs()}).collect::<Vec<_>>();
            bounds[k]+=geometry_conditioning_product(&values)?;
            if !bounds[k].is_finite(){return None;}
        }}
        Some(bounds)
    }
    fn maximum_direction(self) -> f64;
    fn first_order_directions(self) -> Option<Vec<f64>> {
        None
    }
    fn geometry_first_order_directions(self) -> Option<Vec<f64>> {
        self.first_order_directions()
    }
}
impl TransientGasScalar for f64 {
    fn supports_parallel_face_evaluation() -> bool { true }
    fn first_order_directions(self) -> Option<Vec<f64>> {
        Some(vec![])
    }
    fn maximum_direction(self) -> f64 {
        0.
    }
}
impl<const N: usize> TransientGasScalar for Dual<N> {
    fn supports_parallel_face_evaluation() -> bool { true }
    fn first_order_directions(self) -> Option<Vec<f64>> {
        Some(self.eps.to_vec())
    }
    fn maximum_direction(self) -> f64 {
        self.eps.iter().map(|v| v.abs()).fold(0., f64::max)
    }
}
fn fail(s: impl Into<String>) -> CaeError {
    CaeError::contract(s)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientVelocityGrid {
    pub velocity_m_s: Vec<[f64; 3]>,
    pub quadrature_weight: Vec<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransientGasBoundary {
    Interior { right: usize },
    Port { port: usize },
    DiffuseWall { wall: usize },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientGasFace<S = f64> {
    pub left: usize,
    pub boundary: TransientGasBoundary,
    pub area_m2: S,
    pub normal: [S; 3],
    pub ale_velocity_m_s: [S; 3],
    pub wall_velocity_m_s: [S; 3],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientGasInputs<S = f64> {
    pub volume_m3: Vec<S>,
    pub previous_volume_m3: Vec<S>,
    pub previous_inventory_kg: Vec<S>,
    pub faces: Vec<TransientGasFace<S>>,
    pub port_density_kg_m3: Vec<Vec<S>>,
    pub grid: TransientVelocityGrid,
    pub gas_rt_m2_s2: S,
    pub viscosity_pa_s: S,
    pub step_s: S,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientGasPolicy {
    pub maximum_unknowns: usize,
    pub equilibrium_relative_tolerance: f64,
    pub maximum_equilibrium_iterations: usize,
    pub linear_relative_tolerance: f64,
    pub normal_tolerance: f64,
    pub maximum_discrete_temperature_relative_error: f64,
    pub maximum_boundary_equilibrium_mass_fraction: f64,
    pub geometric_absolute_tolerance_m3: f64,
    pub geometric_relative_tolerance: f64,
    pub metric_absolute_tolerance_m2: f64,
    pub metric_relative_tolerance: f64,
}
#[derive(Clone, Debug)]
pub struct TransientGasMoments<S = f64> {
    pub mass_kg: S,
    pub momentum_kg_m_s: [S; 3],
    pub kinetic_energy_j: S,
}
#[derive(Clone, Debug)]
pub struct TransientGasPacket<S = f64> {
    pub mass_kg: S,
    pub momentum_kg_m_s: [S; 3],
    pub kinetic_energy_j: S,
    pub wall_work_j: S,
    pub wall_heat_j: S,
}
#[derive(Clone, Debug)]
pub struct TransientGasSolution<S = f64> {
    pub inventory_kg: Vec<S>,
    pub streamed_inventory_kg: Vec<S>,
    pub cell_moments: Vec<TransientGasMoments<S>>,
    pub packets: Vec<TransientGasPacket<S>>,
    pub collision_heat_bath_energy_j: Vec<S>,
    pub maximum_linear_relative_residual: f64,
    pub mass_balance_defect_kg: S,
    pub momentum_balance_defect_kg_m_s: [S; 3],
    pub energy_balance_defect_j: S,
}
fn matrix(n: usize, e: impl IntoIterator<Item = (usize, usize, f64)>) -> CaeResult<CsrMatrix> {
    let e = e.into_iter().collect::<Vec<_>>();
    CsrMatrix::from_triplets(
        n,
        n,
        &e.iter().map(|x| x.0).collect::<Vec<_>>(),
        &e.iter().map(|x| x.1).collect::<Vec<_>>(),
        &e.iter().map(|x| x.2).collect::<Vec<_>>(),
    )
    .map_err(|e| fail(e.to_string()))
}
fn dot<S: Scalar>(a: [S; 3], b: [S; 3]) -> S {
    (0..3).map(|i| a[i] * b[i]).fold(S::zero(), |a, b| a + b)
}
pub(crate) fn moments<S: Scalar>(q: &[S], g: &TransientVelocityGrid) -> TransientGasMoments<S> {
    let mut r = TransientGasMoments {
        mass_kg: S::zero(),
        momentum_kg_m_s: [S::zero(); 3],
        kinetic_energy_j: S::zero(),
    };
    for (q, c) in q.iter().zip(&g.velocity_m_s) {
        r.mass_kg = r.mass_kg + *q;
        for k in 0..3 {
            r.momentum_kg_m_s[k] = r.momentum_kg_m_s[k] + *q * c[k];
        }
        r.kinetic_energy_j = r.kinetic_energy_j + *q * (0.5 * c.iter().map(|v| v * v).sum::<f64>());
    }
    r
}
fn probability<S: Scalar>(
    g: &TransientVelocityGrid,
    rt: S,
    u: [S; 3],
    lambda: [S; 3],
) -> CaeResult<Vec<S>> {
    let e = g
        .velocity_m_s
        .iter()
        .zip(&g.quadrature_weight)
        .map(|(c, w)| {
            let c = c.map(S::from_f64);
            let d = std::array::from_fn(|i| c[i] - u[i]);
            S::from_f64(w.ln()) - dot(d, d) / (rt * 2.) + dot(lambda, c) / rt.sqrt()
        })
        .collect::<Vec<_>>();
    let shift = e
        .iter()
        .map(|x| x.value())
        .fold(f64::NEG_INFINITY, f64::max);
    let v = e.iter().map(|x| (*x - shift).exp()).collect::<Vec<_>>();
    let sum = v.iter().copied().fold(S::zero(), |a, b| a + b);
    if !sum.value().is_finite() || sum.value() <= 0. {
        return Err(fail("kinetic velocity equilibrium is not representable"));
    }
    Ok(v.into_iter().map(|x| x / sum).collect())
}
fn equilibrium<S: TransientGasScalar>(
    q: &[S],
    g: &TransientVelocityGrid,
    rt: S,
    p: &TransientGasPolicy,
) -> CaeResult<Vec<S>> {
    if q.iter().all(|v| v.value() == 0.) && q.iter().any(|v| v.has_direction()) {
        return Err(fail(
            "vacuum kinetic inventory derivative changes the active stratum",
        ));
    }
    equilibrium_moments(moments(q, g), g, rt, p)
}
fn equilibrium_moments<S: TransientGasScalar>(
    m: TransientGasMoments<S>,
    g: &TransientVelocityGrid,
    rt: S,
    p: &TransientGasPolicy,
) -> CaeResult<Vec<S>> {
    if m.mass_kg.value() == 0. {
        if m.mass_kg.has_direction() || m.momentum_kg_m_s.iter().any(|v| v.has_direction()) {
            return Err(fail(
                "vacuum equilibrium derivative changes the active stratum",
            ));
        }
        return Ok(vec![S::zero(); g.velocity_m_s.len()]);
    }
    let scale = rt.value().sqrt();
    let target = m
        .momentum_kg_m_s
        .map(|j| j.value() / m.mass_kg.value() / scale);
    let mut lambda = [0.; 3];
    let mut factor = None;
    let mut prob = Vec::new();
    for iteration in 0..=p.maximum_equilibrium_iterations {
        prob = probability(g, rt.value(), [0.; 3], lambda)?;
        let mean = std::array::from_fn::<_, 3, _>(|i| {
            prob.iter()
                .zip(&g.velocity_m_s)
                .map(|(w, c)| w * c[i] / scale)
                .sum::<f64>()
        });
        let r = std::array::from_fn::<_, 3, _>(|i| mean[i] - target[i]);
        let cov = std::array::from_fn::<_, 3, _>(|i| {
            std::array::from_fn::<_, 3, _>(|j| {
                prob.iter()
                    .zip(&g.velocity_m_s)
                    .map(|(w, c)| w * (c[i] / scale - mean[i]) * (c[j] / scale - mean[j]))
                    .sum::<f64>()
            })
        });
        let mat = DenseMatrix::new(3, 3, cov.into_iter().flatten().collect())
            .map_err(|e| fail(e.to_string()))?;
        let f = DenseLu::new(&mat)
            .map_err(|_| fail("kinetic moment lies outside the resolved velocity domain"))?;
        if r.iter()
            .all(|x| x.abs() <= p.equilibrium_relative_tolerance)
        {
            factor = Some(f);
            break;
        }
        if iteration == p.maximum_equilibrium_iterations {
            return Err(fail("kinetic discrete Maxwellian did not converge"));
        }
        let d = f
            .solve(&r.map(|x| -x), 1, false)
            .map_err(|e| fail(e.to_string()))?;
        let old = r.iter().map(|v| v * v).sum::<f64>();
        let mut damping = 1.;
        let mut accepted = false;
        for _ in 0..50 {
            let trial = std::array::from_fn(|i| lambda[i] + damping * d[i]);
            let pr = probability(g, rt.value(), [0.; 3], trial)?;
            let norm = (0..3)
                .map(|i| {
                    let x = pr
                        .iter()
                        .zip(&g.velocity_m_s)
                        .map(|(w, c)| w * c[i] / scale)
                        .sum::<f64>()
                        - target[i];
                    x * x
                })
                .sum::<f64>();
            if norm < old {
                lambda = trial;
                accepted = true;
                break;
            }
            damping *= 0.5;
        }
        if !accepted {
            return Err(fail(
                "kinetic moment cannot be matched on the declared velocity grid",
            ));
        }
    }
    let lc = lambda.map(S::from_f64);
    let ps = probability(g, rt, [S::zero(); 3], lc)?;
    let target = m.momentum_kg_m_s.map(|j| j / m.mass_kg / rt.sqrt());
    let residual = (0..3)
        .map(|i| {
            ps.iter()
                .zip(&g.velocity_m_s)
                .map(|(w, c)| *w * c[i] / rt.sqrt())
                .fold(S::zero(), |a, b| a + b)
                - target[i]
        })
        .collect::<Vec<_>>();
    let rhs = residual
        .iter()
        .map(|r| -(*r - S::from_f64(r.value())))
        .collect::<Vec<_>>();
    let inverse = factor
        .ok_or_else(|| fail("kinetic equilibrium factor missing"))?
        .solve(&[1., 0., 0., 0., 1., 0., 0., 0., 1.], 3, false)
        .map_err(|e| fail(e.to_string()))?;
    if inverse.iter().any(|v| !v.is_finite()) {
        return Err(fail("kinetic equilibrium covariance inverse is not finite"));
    }
    let dl = std::array::from_fn::<_, 3, _>(|i| {
        (0..3)
            .map(|j| rhs[j] * inverse[i * 3 + j])
            .fold(S::zero(), |a, b| a + b)
    });
    let lc = std::array::from_fn(|i| lc[i] + dl[i]);
    let discrete_probability = probability(g, rt, [S::zero(); 3], lc)?;
    let mean = m.momentum_kg_m_s.map(|v| v / m.mass_kg);
    let min = std::array::from_fn::<_, 3, _>(|i| {
        g.velocity_m_s
            .iter()
            .map(|c| c[i])
            .fold(f64::INFINITY, f64::min)
    });
    let max = std::array::from_fn::<_, 3, _>(|i| {
        g.velocity_m_s
            .iter()
            .map(|c| c[i])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let boundary_mass = discrete_probability
        .iter()
        .zip(&g.velocity_m_s)
        .filter(|(_, c)| (0..3).any(|i| c[i] == min[i] || c[i] == max[i]))
        .map(|(v, _)| v.value())
        .sum::<f64>();
    if boundary_mass > p.maximum_boundary_equilibrium_mass_fraction {
        return Err(fail(
            "kinetic velocity range truncates the discrete equilibrium",
        ));
    }
    for i in 0..3 {
        for j in 0..3 {
            let cov = discrete_probability
                .iter()
                .zip(&g.velocity_m_s)
                .map(|(w, c)| w.value() * (c[i] - mean[i].value()) * (c[j] - mean[j].value()))
                .sum::<f64>();
            let target = if i == j { rt.value() } else { 0. };
            if !cov.is_finite()
                || (cov - target).abs() / rt.value() > p.maximum_discrete_temperature_relative_error
            {
                return Err(fail(
                    "kinetic velocity quadrature does not resolve the declared gas temperature",
                ));
            }
        }
    }
    let out = discrete_probability
        .into_iter()
        .map(|w| w * m.mass_kg)
        .collect::<Vec<_>>();
    if out
        .iter()
        .any(|v| !v.finite_first_order() || v.value() < 0.)
    {
        return Err(fail("kinetic equilibrium is not finite and nonnegative"));
    }
    let _ = prob;
    Ok(out)
}
pub(crate) fn validate<S: TransientGasScalar>(
    a: &TransientGasInputs<S>,
    p: &TransientGasPolicy,
) -> CaeResult<usize> {
    let nv = a.grid.velocity_m_s.len();
    let n = a
        .volume_m3
        .len()
        .checked_mul(nv)
        .ok_or_else(|| fail("kinetic state size overflow"))?;
    if nv < 4
        || a.grid.quadrature_weight.len() != nv
        || a.volume_m3.is_empty()
        || a.previous_inventory_kg.len() != n
        || a.previous_volume_m3.len() != a.volume_m3.len()
        || !p.geometric_absolute_tolerance_m3.is_finite()
        || p.geometric_absolute_tolerance_m3 < 0.
        || !p.geometric_relative_tolerance.is_finite()
        || p.geometric_relative_tolerance < 0.
        || !p.metric_absolute_tolerance_m2.is_finite()
        || p.metric_absolute_tolerance_m2 < 0.
        || !p.metric_relative_tolerance.is_finite()
        || p.metric_relative_tolerance < 0.
        || n > p.maximum_unknowns
        || p.maximum_equilibrium_iterations == 0
        || ![
            p.equilibrium_relative_tolerance,
            p.linear_relative_tolerance,
            p.normal_tolerance,
            p.maximum_discrete_temperature_relative_error,
            p.maximum_boundary_equilibrium_mass_fraction,
        ]
        .iter()
        .all(|v| v.is_finite() && *v > 0.)
        || [a.gas_rt_m2_s2, a.viscosity_pa_s, a.step_s]
            .iter()
            .any(|x| !x.finite_first_order() || x.value() <= 0.)
        || a.grid.velocity_m_s.iter().flatten().any(|v| !v.is_finite())
        || a.grid
            .quadrature_weight
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.)
    {
        return Err(fail("invalid transient kinetic discretization"));
    }
    for (i, v) in a.volume_m3.iter().enumerate() {
        if !v.finite_first_order() || v.value() < 0. || v.value() == 0. && v.has_direction() {
            return Err(fail("invalid or changing closed kinetic volume"));
        }
        let oldv = a.previous_volume_m3[i];
        if !oldv.finite_first_order() || oldv.value() < 0. {
            return Err(fail("invalid previous kinetic volume"));
        }
        let q = &a.previous_inventory_kg[i * nv..(i + 1) * nv];
        if q.iter().any(|x| {
            !x.finite_first_order()
                || x.value() < 0.
                || (v.value() == 0. || oldv.value() == 0.) && (x.value() != 0. || x.has_direction())
        }) {
            return Err(fail("closed kinetic cell contains trapped inventory"));
        }
    }
    for pr in &a.port_density_kg_m3 {
        if pr.len() != nv || pr.iter().any(|v| !v.finite_first_order() || v.value() < 0.) {
            return Err(fail("invalid kinetic bulk port distribution"));
        }
    }
    for f in &a.faces {
        if f.left >= a.volume_m3.len()
            || !f.area_m2.finite_first_order()
            || f.area_m2.value() < 0.
            || f.area_m2.value() == 0. && f.area_m2.has_direction()
            || f.normal
                .iter()
                .chain(&f.ale_velocity_m_s)
                .chain(&f.wall_velocity_m_s)
                .any(|v| !v.finite_first_order())
            || (dot(f.normal, f.normal).value() - 1.).abs() > p.normal_tolerance
            || (dot(f.normal, f.normal) - S::one()).maximum_direction()
                > p.normal_tolerance
                    * (1.
                        + f.normal
                            .iter()
                            .map(|v| v.maximum_direction())
                            .fold(0., f64::max))
            || f.area_m2.value() > 0. && a.volume_m3[f.left].value() == 0.
        {
            return Err(fail("invalid kinetic face geometry"));
        }
        match f.boundary {
            TransientGasBoundary::Interior { right } => {
                if right >= a.volume_m3.len()
                    || right == f.left
                    || f.area_m2.value() > 0. && a.volume_m3[right].value() == 0.
                {
                    return Err(fail("invalid kinetic interior face"));
                }
            }
            TransientGasBoundary::Port { port } => {
                if port >= a.port_density_kg_m3.len() {
                    return Err(fail("invalid kinetic port index"));
                }
            }
            TransientGasBoundary::DiffuseWall { .. } => {
                if (0..3).any(|k| {
                    (f.wall_velocity_m_s[k] - f.ale_velocity_m_s[k]).value() != 0.
                        || (f.wall_velocity_m_s[k] - f.ale_velocity_m_s[k]).has_direction()
                }) {
                    return Err(fail(
                        "kinetic diffuse wall and material face velocity differ",
                    ));
                }
            }
        }
    }
    let mut metric = vec![[S::zero(); 3]; a.volume_m3.len()];
    let mut metric_scale = vec![[0.; 2]; a.volume_m3.len()];
    for f in &a.faces {
        for k in 0..3 {
            let x = f.area_m2 * f.normal[k];
            metric[f.left][k] = metric[f.left][k] + x;
            if let TransientGasBoundary::Interior { right } = f.boundary {
                metric[right][k] = metric[right][k] - x;
            }
        }
        metric_scale[f.left][0] += f.area_m2.value();
        metric_scale[f.left][1] += f.area_m2.maximum_direction();
        if let TransientGasBoundary::Interior { right } = f.boundary {
            metric_scale[right][0] += f.area_m2.value();
            metric_scale[right][1] += f.area_m2.maximum_direction();
        }
    }
    for (i, v) in metric.iter().enumerate() {
        for x in v {
            let tol =
                p.metric_absolute_tolerance_m2 + p.metric_relative_tolerance * metric_scale[i][0];
            let dtol =
                p.metric_absolute_tolerance_m2 + p.metric_relative_tolerance * metric_scale[i][1];
            if !x.finite_first_order()
                || !tol.is_finite()
                || !dtol.is_finite()
                || x.value().abs() > tol
                || x.maximum_direction() > dtol
            {
                return Err(fail(
                    "kinetic face area vectors do not close the cell metric",
                ));
            }
        }
    }
    let mut swept = vec![S::zero(); a.volume_m3.len()];
    for f in &a.faces {
        let dv = a.step_s * f.area_m2 * dot(f.normal, f.ale_velocity_m_s);
        swept[f.left] = swept[f.left] + dv;
        if let TransientGasBoundary::Interior { right } = f.boundary {
            swept[right] = swept[right] - dv;
        }
    }
    for (i, (new, old)) in a.volume_m3.iter().zip(&a.previous_volume_m3).enumerate() {
        let defect = *new - *old - swept[i];
        let tolerance = p.geometric_absolute_tolerance_m3
            + p.geometric_relative_tolerance * new.value().max(old.value());
        if !defect.finite_first_order()
            || !tolerance.is_finite()
            || defect.value().abs() > tolerance
        {
            return Err(fail(
                "kinetic ALE faces do not satisfy geometric conservation",
            ));
        }
        let direction_tolerance = p.geometric_absolute_tolerance_m3
            + p.geometric_relative_tolerance
                * new
                    .maximum_direction()
                    .max(old.maximum_direction())
                    .max(swept[i].maximum_direction());
        if defect.maximum_direction() > direction_tolerance {
            return Err(fail(
                "kinetic ALE geometry tangent does not satisfy geometric conservation",
            ));
        }
    }
    Ok(nv)
}
fn speeds<S: TransientGasScalar>(
    f: &TransientGasFace<S>,
    g: &TransientVelocityGrid,
) -> CaeResult<Vec<S>> {
    g.velocity_m_s
        .iter()
        .map(|c| {
            let v = std::array::from_fn(|i| S::from_f64(c[i]) - f.ale_velocity_m_s[i]);
            let s = dot(v, f.normal);
            if s.value() == 0. && s.has_direction() {
                return Err(fail(
                    "kinetic grazing velocity derivative crosses a transport branch",
                ));
            }
            Ok(s)
        })
        .collect()
}
fn wall_probability<S: TransientGasScalar>(
    f: &TransientGasFace<S>,
    g: &TransientVelocityGrid,
    rt: S,
    s: &[S],
    policy: &TransientGasPolicy,
) -> CaeResult<(Vec<S>, S)> {
    let pr = velocity_equilibrium_density(g, S::one(), f.wall_velocity_m_s, rt, policy)?;
    let den = pr
        .iter()
        .zip(s)
        .filter(|(_, s)| s.value() < 0.)
        .map(|(w, s)| -*w * *s)
        .fold(S::zero(), |a, b| a + b);
    if den.value() <= 0. || !den.finite_first_order() || !s.iter().any(|v| v.value() > 0.) {
        return Err(fail(
            "kinetic diffuse wall lacks resolved incoming or outgoing velocities",
        ));
    }
    Ok((pr, den))
}
pub fn solve_scalar<S: TransientGasScalar>(
    a: &TransientGasInputs<S>,
    p: &TransientGasPolicy,
) -> CaeResult<TransientGasSolution<S>> {
    let nv = validate(a, p)?;
    let n = a.previous_inventory_kg.len();
    let nw = a
        .faces
        .iter()
        .filter(|f| {
            f.area_m2.value() > 0. && matches!(f.boundary, TransientGasBoundary::DiffuseWall { .. })
        })
        .count();
    let size = n
        .checked_add(nw)
        .ok_or_else(|| fail("kinetic wall auxiliary size overflow"))?;
    if size > p.maximum_unknowns {
        return Err(fail(
            "kinetic state and wall auxiliaries exceed the declared allocation budget",
        ));
    }
    let mut entries = (0..size).map(|i| (i, i, S::one())).collect::<Vec<_>>();
    let mut rhs = a.previous_inventory_kg.clone();
    rhs.resize(size, S::zero());
    let mut wall_index = n;
    for f in &a.faces {
        if f.area_m2.value() == 0. {
            continue;
        }
        let s = speeds(f, &a.grid)?;
        let left = f.left * nv;
        let v = a.volume_m3[f.left];
        match f.boundary {
            TransientGasBoundary::Interior { right } => {
                for k in 0..nv {
                    let d = a.step_s * f.area_m2 * s[k];
                    let donor = if s[k].value() >= 0. {
                        left + k
                    } else {
                        right * nv + k
                    };
                    let vol = if s[k].value() >= 0. {
                        v
                    } else {
                        a.volume_m3[right]
                    };
                    entries.push((left + k, donor, d / vol));
                    entries.push((right * nv + k, donor, -d / vol));
                }
            }
            TransientGasBoundary::Port { port } => {
                for k in 0..nv {
                    let d = a.step_s * f.area_m2 * s[k];
                    if s[k].value() >= 0. {
                        entries.push((left + k, left + k, d / v));
                    } else {
                        rhs[left + k] = rhs[left + k] - d * a.port_density_kg_m3[port][k];
                    }
                }
            }
            TransientGasBoundary::DiffuseWall { .. } => {
                let (pr, den) = wall_probability(f, &a.grid, a.gas_rt_m2_s2, &s, p)?;
                let velocity_scale = a.gas_rt_m2_s2.sqrt();
                for j in 0..nv {
                    if s[j].value() > 0. {
                        let d = a.step_s * f.area_m2 * s[j] / v;
                        entries.push((left + j, left + j, d));
                        entries.push((wall_index, left + j, -s[j] / velocity_scale));
                    } else if s[j].value() < 0. {
                        entries.push((
                            left + j,
                            wall_index,
                            a.step_s * f.area_m2 * s[j] * pr[j] * velocity_scale / (den * v),
                        ));
                    }
                }
                wall_index += 1;
            }
        }
    }
    let mat = matrix(size, entries.iter().map(|(i, j, v)| (*i, *j, v.value())))?;
    let factor = SparseLu::new(&mat.to_csc()).map_err(|e| fail(e.to_string()))?;
    let qp = factor
        .solve(&rhs.iter().map(|v| v.value()).collect::<Vec<_>>())
        .map_err(|e| fail(e.to_string()))?;
    if qp.iter().any(|v| !v.is_finite() || *v < 0.) {
        return Err(fail(
            "kinetic streaming solution is not finite and nonnegative",
        ));
    }
    let mut r = rhs.iter().map(|v| -*v).collect::<Vec<_>>();
    for (i, j, v) in &entries {
        r[*i] = r[*i] + *v * qp[*j];
    }
    let scale = rhs
        .iter()
        .map(|v| v.value().abs())
        .fold(0., f64::max)
        .max(qp.iter().copied().fold(0., f64::max));
    let err = r.iter().map(|v| v.value().abs()).fold(0., f64::max) / scale.max(f64::MIN_POSITIVE);
    if !err.is_finite() || err > p.linear_relative_tolerance {
        return Err(fail("kinetic streaming linear residual exceeds tolerance"));
    }
    let delta = S::implicit_solve(
        &factor,
        &r.iter()
            .map(|v| -(*v - S::from_f64(v.value())))
            .collect::<Vec<_>>(),
    )?;
    let streamed = qp
        .iter()
        .take(n)
        .zip(delta.into_iter().take(n))
        .map(|(v, d)| S::from_f64(*v) + d)
        .collect::<Vec<_>>();
    let mut packets = Vec::new();
    for f in &a.faces {
        let mut pk = TransientGasPacket {
            mass_kg: S::zero(),
            momentum_kg_m_s: [S::zero(); 3],
            kinetic_energy_j: S::zero(),
            wall_work_j: S::zero(),
            wall_heat_j: S::zero(),
        };
        if f.area_m2.value() > 0. {
            let s = speeds(f, &a.grid)?;
            let wall = if matches!(f.boundary, TransientGasBoundary::DiffuseWall { .. }) {
                let (pr, den) = wall_probability(f, &a.grid, a.gas_rt_m2_s2, &s, p)?;
                let outgoing = (0..nv)
                    .filter(|k| s[*k].value() > 0.)
                    .map(|k| s[k] * streamed[f.left * nv + k] / a.volume_m3[f.left])
                    .fold(S::zero(), |a, b| a + b);
                Some((pr, den, outgoing))
            } else {
                None
            };
            for k in 0..nv {
                let density = if s[k].value() >= 0. {
                    streamed[f.left * nv + k] / a.volume_m3[f.left]
                } else {
                    match f.boundary {
                        TransientGasBoundary::Interior { right } => {
                            streamed[right * nv + k] / a.volume_m3[right]
                        }
                        TransientGasBoundary::Port { port } => a.port_density_kg_m3[port][k],
                        TransientGasBoundary::DiffuseWall { .. } => {
                            let (pr, den, out) = wall.as_ref().unwrap();
                            pr[k] * *out / *den
                        }
                    }
                };
                let dq = a.step_s * f.area_m2 * s[k] * density;
                pk.mass_kg = pk.mass_kg + dq;
                for j in 0..3 {
                    pk.momentum_kg_m_s[j] = pk.momentum_kg_m_s[j] + dq * a.grid.velocity_m_s[k][j];
                }
                pk.kinetic_energy_j = pk.kinetic_energy_j
                    + dq * (0.5 * a.grid.velocity_m_s[k].iter().map(|v| v * v).sum::<f64>());
            }
            if matches!(f.boundary, TransientGasBoundary::DiffuseWall { .. }) {
                pk.wall_work_j = dot(pk.momentum_kg_m_s, f.wall_velocity_m_s);
                pk.wall_heat_j = pk.kinetic_energy_j - pk.wall_work_j;
            }
        }
        packets.push(pk);
    }
    let mut inventory = Vec::with_capacity(n);
    let mut heat = Vec::new();
    let mut cm = Vec::new();
    for (i, v) in a.volume_m3.iter().enumerate() {
        let q = &streamed[i * nv..(i + 1) * nv];
        let m = moments(q, &a.grid);
        let eq = equilibrium(q, &a.grid, a.gas_rt_m2_s2, p)?;
        let rate = if v.value() == 0. {
            S::zero()
        } else {
            a.step_s * m.mass_kg * a.gas_rt_m2_s2 / (*v * a.viscosity_pa_s)
        };
        let out = q
            .iter()
            .zip(&eq)
            .map(|(q, e)| (*q + rate * *e) / (S::one() + rate))
            .collect::<Vec<_>>();
        let after = moments(&out, &a.grid);
        heat.push(m.kinetic_energy_j - after.kinetic_energy_j);
        cm.push(after);
        inventory.extend(out);
    }
    let old = moments(
        &a.previous_inventory_kg,
        &TransientVelocityGrid {
            velocity_m_s: (0..a.volume_m3.len())
                .flat_map(|_| a.grid.velocity_m_s.clone())
                .collect(),
            quadrature_weight: Vec::new(),
        },
    );
    let new = moments(
        &inventory,
        &TransientVelocityGrid {
            velocity_m_s: (0..a.volume_m3.len())
                .flat_map(|_| a.grid.velocity_m_s.clone())
                .collect(),
            quadrature_weight: Vec::new(),
        },
    );
    let mut md = new.mass_kg - old.mass_kg;
    let mut jd = std::array::from_fn(|i| new.momentum_kg_m_s[i] - old.momentum_kg_m_s[i]);
    let mut ed = new.kinetic_energy_j - old.kinetic_energy_j
        + heat.iter().copied().fold(S::zero(), |a, b| a + b);
    for (f, pk) in a.faces.iter().zip(&packets) {
        if !matches!(f.boundary, TransientGasBoundary::Interior { .. }) {
            md = md + pk.mass_kg;
            for k in 0..3 {
                jd[k] = jd[k] + pk.momentum_kg_m_s[k];
            }
            ed = ed + pk.kinetic_energy_j;
        }
    }
    if inventory
        .iter()
        .any(|v| !v.finite_first_order() || v.value() < 0.)
    {
        return Err(fail("kinetic state derivative is not finite"));
    }
    if cm
        .iter()
        .flat_map(|m| {
            [
                m.mass_kg,
                m.momentum_kg_m_s[0],
                m.momentum_kg_m_s[1],
                m.momentum_kg_m_s[2],
                m.kinetic_energy_j,
            ]
        })
        .chain(packets.iter().flat_map(|p| {
            [
                p.mass_kg,
                p.momentum_kg_m_s[0],
                p.momentum_kg_m_s[1],
                p.momentum_kg_m_s[2],
                p.kinetic_energy_j,
                p.wall_work_j,
                p.wall_heat_j,
            ]
        }))
        .chain(heat.iter().copied())
        .chain([md, jd[0], jd[1], jd[2], ed])
        .any(|v| !v.finite_first_order())
    {
        return Err(fail("kinetic moments or exchange ledger is not finite"));
    }
    Ok(TransientGasSolution {
        inventory_kg: inventory,
        streamed_inventory_kg: streamed,
        cell_moments: cm,
        packets,
        collision_heat_bath_energy_j: heat,
        maximum_linear_relative_residual: err,
        mass_balance_defect_kg: md,
        momentum_balance_defect_kg_m_s: jd,
        energy_balance_defect_j: ed,
    })
}
pub fn solve(a: &TransientGasInputs, p: &TransientGasPolicy) -> CaeResult<TransientGasSolution> {
    solve_scalar(a, p)
}

pub fn velocity_equilibrium_density<S: TransientGasScalar>(
    grid: &TransientVelocityGrid,
    density_kg_m3: S,
    velocity_m_s: [S; 3],
    gas_rt_m2_s2: S,
    policy: &TransientGasPolicy,
) -> CaeResult<Vec<S>> {
    if !density_kg_m3.finite_first_order()
        || density_kg_m3.value() < 0.
        || velocity_m_s.iter().any(|v| !v.finite_first_order())
    {
        return Err(fail("invalid kinetic equilibrium moments"));
    }
    validate(
        &TransientGasInputs {
            volume_m3: vec![S::one()],
            previous_volume_m3: vec![S::one()],
            previous_inventory_kg: vec![S::zero(); grid.velocity_m_s.len()],
            faces: Vec::new(),
            port_density_kg_m3: Vec::new(),
            grid: grid.clone(),
            gas_rt_m2_s2,
            viscosity_pa_s: S::one(),
            step_s: S::one(),
        },
        policy,
    )?;
    equilibrium_moments(
        TransientGasMoments {
            mass_kg: density_kg_m3,
            momentum_kg_m_s: velocity_m_s.map(|v| v * density_kg_m3),
            kinetic_energy_j: S::zero(),
        },
        grid,
        gas_rt_m2_s2,
        policy,
    )
}
