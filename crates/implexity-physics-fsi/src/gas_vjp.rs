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
use crate::gas_multiscale_network::{
    MultiscaleNetworkInputs, MultiscaleNetworkPolicy, MultiscaleNetworkTopology,
    residual_cells_prepared_scalar,
};
use crate::gas_transient_kinetic::{TransientGasFace, TransientGasInputs, TransientGasScalar};
use implexity_ad::{
    Scalar,
};
use implexity_core::{CaeError, CaeResult};
use implexity_ad::compact_scalar_tape::{CompactScalarTape as Tape,CompactScalarVar as Var};
use crate::compact_geometry_directions::{GeometryDirections,GeometryEdges};
use implexity_linalg::lu::SparseLu;
use std::{
    cell::RefCell,
    iter::Sum,
    ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Sub, SubAssign},
    sync::atomic::{AtomicU64, Ordering},
};

struct Context {
    session: u64,
    tape: Tape,
    failed: Option<String>,
    geometry_inputs: Option<Vec<Var>>,
    geometry_directions: GeometryDirections,
    geometry_edges: GeometryEdges,
}
thread_local! { static CONTEXT:RefCell<Option<Context>>=const{RefCell::new(None)}; }
static SESSION: AtomicU64 = AtomicU64::new(1);
struct Scope;
impl Scope {
    fn enter() -> CaeResult<Self> {
        CONTEXT.with(|c| {
            let mut c = c.borrow_mut();
            if c.is_some() {
                return Err(CaeError::contract("nested molecular reverse scope"));
            }
            let session = SESSION
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
                .map_err(|_| CaeError::contract("molecular reverse session exhausted"))?;
            *c = Some(Context {
                session,
                tape: Tape::new(),
                failed: None,
                geometry_inputs: None,
                geometry_directions: GeometryDirections::default(),
                geometry_edges: GeometryEdges::default(),
            });
            Ok(Self)
        })
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXT.with(|c| *c.borrow_mut() = None);
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ReverseScalar {
    value: f64,
    variable: Option<Var>,
    session: u64,
}
impl ReverseScalar {
    fn input(value: f64) -> CaeResult<Self> {
        CONTEXT.with(|c| {
            let mut c = c.borrow_mut();
            let c = c
                .as_mut()
                .ok_or_else(|| CaeError::contract("molecular reverse scope missing"))?;
            if !value.is_finite() {
                return Err(CaeError::contract("molecular reverse input not finite"));
            }
            let variable = c.tape.input(value);
            Ok(Self {
                value,
                variable: Some(variable),
                session: c.session,
            })
        })
    }
    fn record(value: f64, inputs: &[Self], partials: &[f64]) -> Self {
        if inputs
            .iter()
            .zip(partials)
            .all(|(v, p)| v.variable.is_none() || *p == 0.)
        {
            return Self::from_f64(value);
        }
        CONTEXT.with(|context| {
            let mut context = context.borrow_mut();
            let Some(c) = context.as_mut() else {
                return Self::from_f64(f64::NAN);
            };
            if inputs.len() != partials.len()
                || !value.is_finite()
                || inputs.iter().zip(partials).any(|(v, p)| {
                    v.variable.is_some() && (v.session != c.session || !p.is_finite())
                })
            {
                c.failed = Some("invalid molecular reverse edge or session".into());
                return Self::from_f64(f64::NAN);
            }
            let mut dependent = inputs
                .iter()
                .zip(partials)
                .filter(|(v, p)| v.variable.is_some() && **p != 0.);
            let first = dependent.next();
            let second = dependent.next();
            let third = dependent.next();
            let operation = if let Some((a, weight)) = first {
                let av = a.variable.unwrap();
                if second.is_none() {
                    if *weight == 1. && value.to_bits() == a.value.to_bits() {
                        return *a;
                    }
                    if value == a.value * *weight {
                        c.tape.scale(av, *weight)
                    } else {
                        let derivative = *weight;
                        c.tape
                            .map(av, |_| implexity_ad::Dual::new(value, [derivative]))
                    }
                } else if third.is_none() {
                    let (b, wb) = second.unwrap();
                    let bv = b.variable.unwrap();
                    if av == bv && *weight + *wb == 0. {
                        return Self::from_f64(value);
                    }
                    if *weight == 1. && *wb == 1. && value == a.value + b.value {
                        c.tape.add(av, bv)
                    } else if *weight == 1. && *wb == -1. && value == a.value - b.value {
                        c.tape.sub(av, bv)
                    } else if *weight == b.value && *wb == a.value && value == a.value * b.value {
                        c.tape.mul(av, bv)
                    } else if *weight == 1. / b.value
                        && *wb == -a.value / (b.value * b.value)
                        && value == a.value / b.value
                    {
                        c.tape.div(av, bv)
                    } else {
                        let da = *weight;
                        let db = *wb;
                        c.tape
                            .map2(av, bv, |_, _| implexity_ad::Dual::new(value, [da, db]))
                    }
                } else {
                    let mut vars = vec![];
                    let mut weights = vec![];
                    for (v, p) in inputs.iter().zip(partials) {
                        if let Some(var) = v.variable {
                            vars.push(var);
                            weights.push(*p);
                        }
                    }
                    c.tape.custom(
                        &vars,
                        vec![value],
                        Box::new(move |bar| Ok(weights.iter().map(|w| vec![bar[0] * w]).collect())),
                    )
                }
            } else {
                unreachable!()
            };
            match operation {
                Ok(variable) => {
                    if c.geometry_inputs.is_some() {
                        c.geometry_directions.reserve_nodes(variable.index()+1);
                        if !c.geometry_edges.append(variable.index(),inputs.iter().zip(partials).filter_map(|(input,partial)|input.variable.map(|parent|(parent.index(),*partial)))){
                            c.failed=Some("geometry forward node order differs".into());
                        }
                    }
                    Self { value, variable: Some(variable), session: c.session }
                },
                Err(e) => {
                    c.failed = Some(e.to_string());
                    Self::from_f64(f64::NAN)
                }
            }
        })
    }
}
impl Scalar for ReverseScalar {
    fn from_f64(value: f64) -> Self {
        Self {
            value,
            variable: None,
            session: 0,
        }
    }
    fn value(&self) -> f64 {
        self.value
    }
    fn chain(self, f: f64, df: f64, _: f64) -> Self {
        Self::record(f, &[self], &[df])
    }
    fn chain2(a: Self, b: Self, f: f64, fa: f64, fb: f64, _: f64, _: f64, _: f64) -> Self {
        Self::record(f, &[a, b], &[fa, fb])
    }
    fn lift(f: f64, inputs: &[Self], grad: &[f64], _: &[f64]) -> Self {
        Self::record(f, inputs, grad)
    }
}
impl Add for ReverseScalar {
    type Output = Self;
    fn add(self, r: Self) -> Self {
        Self::record(self.value + r.value, &[self, r], &[1., 1.])
    }
}
impl Sub for ReverseScalar {
    type Output = Self;
    fn sub(self, r: Self) -> Self {
        Self::record(self.value - r.value, &[self, r], &[1., -1.])
    }
}
impl Mul for ReverseScalar {
    type Output = Self;
    fn mul(self, r: Self) -> Self {
        Self::record(self.value * r.value, &[self, r], &[r.value, self.value])
    }
}
impl Div for ReverseScalar {
    type Output = Self;
    fn div(self, r: Self) -> Self {
        Self::record(
            self.value / r.value,
            &[self, r],
            &[1. / r.value, -self.value / (r.value * r.value)],
        )
    }
}
impl Neg for ReverseScalar {
    type Output = Self;
    fn neg(self) -> Self {
        Self::record(-self.value, &[self], &[-1.])
    }
}
impl Add<f64> for ReverseScalar {
    type Output = Self;
    fn add(self, r: f64) -> Self {
        self + Self::from_f64(r)
    }
}
impl Sub<f64> for ReverseScalar {
    type Output = Self;
    fn sub(self, r: f64) -> Self {
        self - Self::from_f64(r)
    }
}
impl Mul<f64> for ReverseScalar {
    type Output = Self;
    fn mul(self, r: f64) -> Self {
        self * Self::from_f64(r)
    }
}
impl Div<f64> for ReverseScalar {
    type Output = Self;
    fn div(self, r: f64) -> Self {
        self / Self::from_f64(r)
    }
}
impl AddAssign for ReverseScalar {
    fn add_assign(&mut self, r: Self) {
        *self = *self + r;
    }
}
impl SubAssign for ReverseScalar {
    fn sub_assign(&mut self, r: Self) {
        *self = *self - r;
    }
}
impl MulAssign for ReverseScalar {
    fn mul_assign(&mut self, r: Self) {
        *self = *self * r;
    }
}
impl DivAssign for ReverseScalar {
    fn div_assign(&mut self, r: Self) {
        *self = *self / r;
    }
}
impl Sum for ReverseScalar {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::from_f64(0.), |a, b| a + b)
    }
}
impl GasMassScalar for ReverseScalar {
    fn try_first_order_lift(
        value: f64,
        inputs: &[Self],
        partials: &[f64],
    ) -> CaeResult<Option<Self>> {
        <Self as TransientGasScalar>::lift_geometry_partial(value, inputs, partials).map(Some)
    }
    fn implicit_solve(factor: &SparseLu, rhs: &[Self]) -> CaeResult<Vec<Self>> {
        let n = rhs.len();
        let values = factor
            .solve(&rhs.iter().map(|v| v.value).collect::<Vec<_>>())
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let mut inverse = vec![vec![0.; n]; n];
        for j in 0..n {
            let mut seed = vec![0.; n];
            seed[j] = 1.;
            let column = factor
                .solve(&seed)
                .map_err(|e| CaeError::contract(e.to_string()))?;
            for i in 0..n {
                inverse[i][j] = column[i];
            }
        }
        Ok(values
            .iter()
            .enumerate()
            .map(|(i, v)| Self::record(*v, rhs, &inverse[i]))
            .collect())
    }
    fn has_direction(self) -> bool {
        self.variable.is_some()
    }
    fn finite_first_order(self) -> bool {
        self.value.is_finite()
            && CONTEXT.with(|c| c.borrow().as_ref().is_none_or(|c| c.failed.is_none()))
    }
}
impl TransientGasScalar for ReverseScalar {
    fn lift_geometry_partial(value: f64, inputs: &[Self], partials: &[f64]) -> CaeResult<Self> {
        if !value.is_finite()
            || inputs.len() != partials.len()
            || inputs.iter().any(|v| !v.finite_first_order())
            || partials.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "geometry reverse primitive value or partials invalid",
            ));
        }
        let output = Self::record(value, inputs, partials);
        if !output.finite_first_order() {
            return Err(CaeError::contract(
                "geometry reverse primitive record failed",
            ));
        }
        Ok(output)
    }

    fn geometry_first_order_directions(self) -> Option<Vec<f64>> {
        if self.variable.is_none() {
            return Some(CONTEXT.with(|context| {
                context
                    .borrow()
                    .as_ref()
                    .and_then(|context| context.geometry_inputs.as_ref())
                    .map_or_else(Vec::new, |inputs| vec![0.; inputs.len()])
            }));
        }
        CONTEXT.with(|context| {
            let mut context = context.borrow_mut();
            let c = context.as_mut()?;
            let inputs = c.geometry_inputs.as_ref()?;
            if self.session != c.session {
                c.failed = Some("geometry reverse session mismatch".into());
                return None;
            }
            let dimension = inputs.len();
            let target = self.variable.unwrap().index();
            let mut pending = vec![(target, false)];
            while let Some((node, ready)) = pending.pop() {
                if c.geometry_directions.get(node).is_some() {
                    continue;
                }
                let Some(edges) = c.geometry_edges.get(node) else {
                    c.failed = Some("geometry forward node is missing".into());
                    return None;
                };
                if !ready {
                    pending.push((node, true));
                    for (parent, _) in edges.iter().rev() {
                        if *parent >= node {
                            c.failed = Some("geometry forward graph is not acyclic".into());
                            return None;
                        }
                        pending.push((*parent, false));
                    }
                    continue;
                }
                let mut directions = vec![0.; dimension];
                for (parent, partial) in edges {
                    let Some(parent_directions) = c.geometry_directions.get(*parent) else {
                        c.failed = Some("geometry forward parent is missing".into());
                        return None;
                    };
                    if parent_directions.len() != dimension {
                        c.failed = Some("geometry forward parent dimension differs".into());
                        return None;
                    }
                    for (direction, parent_direction) in directions.iter_mut().zip(parent_directions) {
                        *direction += partial * parent_direction;
                    }
                }
                if directions.iter().any(|v| !v.is_finite()) {
                    c.failed = Some("geometry forward direction overflow".into());
                    return None;
                }
                if !c.geometry_directions.store(node,&directions){c.failed=Some("geometry forward dimension differs".into());return None;}
            }
            let result = c.geometry_directions.get(target).map(<[f64]>::to_vec);
            match result {
                Some(v) if v.len() == dimension => Some(v),
                _ => {
                    c.failed = Some("geometry forward direction is missing".into());
                    None
                }
            }
        })
    }
    fn maximum_direction(self) -> f64 {
        0.
    }
}

pub struct PreparedNetworkVjp {
    inputs: MultiscaleNetworkInputs<ReverseScalar>,
}
impl PreparedNetworkVjp {
    pub fn new(a: &MultiscaleNetworkInputs<f64>) -> Self {
        let constant = ReverseScalar::from_f64;
        Self {
            inputs: MultiscaleNetworkInputs {
                force_density_n_m3: a
                    .force_density_n_m3
                    .as_ref()
                    .map(|f| f.iter().map(|v| v.map(constant)).collect()),
                gas: TransientGasInputs {
                    volume_m3: a.gas.volume_m3.iter().map(|v| constant(*v)).collect(),
                    previous_volume_m3: a
                        .gas
                        .previous_volume_m3
                        .iter()
                        .map(|v| constant(*v))
                        .collect(),
                    previous_inventory_kg: a
                        .gas
                        .previous_inventory_kg
                        .iter()
                        .map(|v| constant(*v))
                        .collect(),
                    faces: a
                        .gas
                        .faces
                        .iter()
                        .map(|f| TransientGasFace {
                            left: f.left,
                            boundary: f.boundary.clone(),
                            area_m2: constant(f.area_m2),
                            normal: f.normal.map(constant),
                            ale_velocity_m_s: f.ale_velocity_m_s.map(constant),
                            wall_velocity_m_s: f.wall_velocity_m_s.map(constant),
                        })
                        .collect(),
                    port_density_kg_m3: a
                        .gas
                        .port_density_kg_m3
                        .iter()
                        .map(|p| p.iter().map(|v| constant(*v)).collect())
                        .collect(),
                    grid: a.gas.grid.clone(),
                    gas_rt_m2_s2: constant(a.gas.gas_rt_m2_s2),
                    viscosity_pa_s: constant(a.gas.viscosity_pa_s),
                    step_s: constant(a.gas.step_s),
                },
                left_centroid_to_face_m: a
                    .left_centroid_to_face_m
                    .iter()
                    .map(|v| v.map(constant))
                    .collect(),
                right_centroid_to_face_m: a
                    .right_centroid_to_face_m
                    .iter()
                    .map(|v| v.map(|v| v.map(constant)))
                    .collect(),
            },
        }
    }
    pub fn apply(
        &self,
        topology: &MultiscaleNetworkTopology,
        state: &[f64],
        offsets: &[Option<usize>],
        mass_scale_kg: &[f64],
        populations: usize,
        cotangent: &[f64],
        policy: &MultiscaleNetworkPolicy,
    ) -> CaeResult<Vec<f64>> {
        if state.len() != cotangent.len()
            || offsets.len() != mass_scale_kg.len()
            || offsets.len() != self.inputs.gas.volume_m3.len()
            || populations != self.inputs.gas.grid.velocity_m_s.len()
            || mass_scale_kg.iter().any(|v| !v.is_finite() || *v <= 0.)
            || state.iter().chain(cotangent).any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(
                "molecular reverse network layout or value",
            ));
        }
        let n = offsets.len();
        if topology.incident_faces.len() != n
            || topology.neighbor_cells.len() != n
            || topology.residual_support_cells.len() != n
            || topology
                .residual_support_cells
                .iter()
                .flatten()
                .any(|c| *c >= n)
        {
            return Err(CaeError::contract("molecular reverse topology layout"));
        }
        let mut expected = 0usize;
        for offset in offsets.iter().flatten() {
            if *offset != expected {
                return Err(CaeError::contract(
                    "molecular reverse offsets are not canonical",
                ));
            }
            expected = expected
                .checked_add(populations)
                .ok_or_else(|| CaeError::contract("molecular reverse size overflow"))?;
        }
        if expected != state.len() {
            return Err(CaeError::contract("molecular reverse state size differs"));
        }
        let mut out = vec![0.; state.len()];
        for (cell, offset) in offsets.iter().enumerate() {
            let Some(offset) = offset else { continue };
            if *offset + populations > state.len() {
                return Err(CaeError::contract("molecular reverse state offset"));
            }
            if cotangent[*offset..*offset + populations]
                .iter()
                .all(|v| *v == 0.)
            {
                continue;
            }
            let _scope = Scope::enter()?;
            let mut variables = std::collections::BTreeMap::new();
            for c in &topology.residual_support_cells[cell] {
                if let Some(o) = offsets[*c] {
                    if o + populations > state.len() {
                        return Err(CaeError::contract("molecular reverse support offset"));
                    }
                    for k in 0..populations {
                        variables.insert(
                            o + k,
                            (
                                ReverseScalar::input(state[o + k] * mass_scale_kg[*c])?,
                                mass_scale_kg[*c],
                            ),
                        );
                    }
                }
            }
            let r = residual_cells_prepared_scalar(
                &self.inputs,
                topology,
                |c, k| offsets[c].map_or(ReverseScalar::from_f64(0.), |o| variables[&(o + k)].0),
                &[cell],
                policy,
            )?;
            let total = r
                .inventory_residual_kg
                .iter()
                .enumerate()
                .map(|(k, r)| *r * (cotangent[*offset + k] / mass_scale_kg[cell]))
                .sum::<ReverseScalar>();
            CONTEXT.with(|context| {
                let context = context.borrow();
                let c = context
                    .as_ref()
                    .ok_or_else(|| CaeError::contract("molecular reverse scope lost"))?;
                if let Some(e) = &c.failed {
                    return Err(CaeError::contract(e.clone()));
                }
                if let Some(output) = total.variable {
                    let grads = c
                        .tape
                        .vjp(output, &[1.])
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    for (index, (v, scale)) in &variables {
                        out[*index] += grads
                            .wrt(v.variable.ok_or_else(|| {
                                CaeError::contract("molecular reverse input missing")
                            })?)
                            .map_err(|e| CaeError::contract(e.to_string()))?[0]
                            * scale;
                    }
                }
                Ok(())
            })?;
        }
        if out.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("molecular reverse cotangent overflow"));
        }
        Ok(out)
    }
}

#[derive(Clone, Debug)]
pub struct NetworkOutputBars {
    pub inventory_residual_kg: Vec<f64>,
    pub face_packets: Vec<[f64; 7]>,
}

#[derive(Clone, Debug)]
pub struct NetworkInputBars {
    pub current_inventory_kg: Vec<f64>,
    pub previous_inventory_kg: Vec<f64>,
    pub port_density_kg_m3: Vec<Vec<f64>>,
}

pub fn output_vjp(
    inputs: &MultiscaleNetworkInputs<f64>,
    topology: &MultiscaleNetworkTopology,
    inventory: &[f64],
    bars: &NetworkOutputBars,
    policy: &MultiscaleNetworkPolicy,
) -> CaeResult<NetworkInputBars> {
    use crate::gas_transient_kinetic::TransientGasBoundary;
    use std::collections::{BTreeMap, BTreeSet};
    let n = inputs.gas.volume_m3.len();
    let nv = inputs.gas.grid.velocity_m_s.len();
    let size = n
        .checked_mul(nv)
        .ok_or_else(|| CaeError::contract("molecular output reverse size overflow"))?;
    if nv == 0
        || inputs.gas.previous_inventory_kg.len() != size
        || inventory.len() != size
        || bars.inventory_residual_kg.len() != size
        || bars.face_packets.len() != inputs.gas.faces.len()
        || topology.incident_faces.len() != n
        || topology.residual_support_cells.len() != n
        || topology
            .incident_faces
            .iter()
            .flatten()
            .any(|fi| *fi >= inputs.gas.faces.len())
        || topology
            .residual_support_cells
            .iter()
            .flatten()
            .any(|c| *c >= n)
        || inventory
            .iter()
            .chain(&bars.inventory_residual_kg)
            .chain(bars.face_packets.iter().flatten())
            .any(|v| !v.is_finite())
    {
        return Err(CaeError::contract(
            "molecular output reverse layout or value",
        ));
    }
    let mut prepared = PreparedNetworkVjp::new(inputs);
    let mut result = NetworkInputBars {
        current_inventory_kg: vec![0.; size],
        previous_inventory_kg: bars.inventory_residual_kg.iter().map(|v| -v).collect(),
        port_density_kg_m3: inputs
            .gas
            .port_density_kg_m3
            .iter()
            .map(|p| vec![0.; p.len()])
            .collect(),
    };
    for cell in 0..n {
        let packet_active = topology.incident_faces[cell].iter().any(|fi| {
            inputs.gas.faces[*fi].left == cell && bars.face_packets[*fi].iter().any(|v| *v != 0.)
        });
        if !packet_active
            && bars.inventory_residual_kg[cell * nv..(cell + 1) * nv]
                .iter()
                .all(|v| *v == 0.)
        {
            continue;
        }
        let _scope = Scope::enter()?;
        let mut variables = BTreeMap::new();
        for &c in &topology.residual_support_cells[cell] {
            if c >= n {
                return Err(CaeError::contract(
                    "molecular output reverse support outside cells",
                ));
            }
            if inputs.gas.volume_m3[c] == 0. {
                continue;
            }
            for k in 0..nv {
                variables.insert(c * nv + k, ReverseScalar::input(inventory[c * nv + k])?);
            }
        }
        let ports: BTreeSet<_> = topology.residual_support_cells[cell]
            .iter()
            .flat_map(|c| topology.incident_faces[*c].iter())
            .filter_map(|fi| {
                if let TransientGasBoundary::Port { port } = inputs.gas.faces[*fi].boundary {
                    Some(port)
                } else {
                    None
                }
            })
            .collect();
        let mut port_variables = BTreeMap::new();
        for &port in &ports {
            let density = prepared
                .inputs
                .gas
                .port_density_kg_m3
                .get_mut(port)
                .ok_or_else(|| CaeError::contract("molecular reverse port outside density rows"))?;
            for (k, v) in density.iter_mut().enumerate() {
                *v = ReverseScalar::input(inputs.gas.port_density_kg_m3[port][k])?;
                port_variables.insert((port, k), *v);
            }
        }
        let r = residual_cells_prepared_scalar(
            &prepared.inputs,
            topology,
            |c, k| {
                variables
                    .get(&(c * nv + k))
                    .copied()
                    .unwrap_or_else(|| ReverseScalar::from_f64(inventory[c * nv + k]))
            },
            &[cell],
            policy,
        )?;
        let mut total = r
            .inventory_residual_kg
            .iter()
            .enumerate()
            .map(|(k, v)| *v * bars.inventory_residual_kg[cell * nv + k])
            .sum::<ReverseScalar>();
        for (fi, p) in &r.face_packets {
            if inputs.gas.faces[*fi].left != cell {
                continue;
            }
            let b = bars.face_packets[*fi];
            let v = [
                p.mass_kg,
                p.momentum_kg_m_s[0],
                p.momentum_kg_m_s[1],
                p.momentum_kg_m_s[2],
                p.kinetic_energy_j,
                p.wall_work_j,
                p.wall_heat_j,
            ];
            total += v.iter().zip(b).map(|(v, b)| *v * b).sum::<ReverseScalar>();
        }
        CONTEXT.with(|context| {
            let context = context.borrow();
            let c = context
                .as_ref()
                .ok_or_else(|| CaeError::contract("molecular output reverse scope lost"))?;
            if let Some(e) = &c.failed {
                return Err(CaeError::contract(e.clone()));
            }
            if let Some(output) = total.variable {
                let gradients = c
                    .tape
                    .vjp(output, &[1.])
                    .map_err(|e| CaeError::contract(e.to_string()))?;
                for (i, v) in &variables {
                    result.current_inventory_kg[*i] += gradients
                        .wrt(v.variable.unwrap())
                        .map_err(|e| CaeError::contract(e.to_string()))?[0];
                }
                for ((port, k), v) in &port_variables {
                    result.port_density_kg_m3[*port][*k] += gradients
                        .wrt(v.variable.unwrap())
                        .map_err(|e| CaeError::contract(e.to_string()))?[0];
                }
            }
            Ok(())
        })?;
        for &port in &ports {
            for (v, &base) in prepared.inputs.gas.port_density_kg_m3[port]
                .iter_mut()
                .zip(&inputs.gas.port_density_kg_m3[port])
            {
                *v = ReverseScalar::from_f64(base);
            }
        }
    }
    for (cell, old) in inputs.gas.previous_inventory_kg.chunks(nv).enumerate() {
        if old.iter().all(|v| *v == 0.) {
            result.previous_inventory_kg[cell * nv..(cell + 1) * nv].fill(0.);
        }
    }
    if result
        .current_inventory_kg
        .iter()
        .chain(&result.previous_inventory_kg)
        .chain(result.port_density_kg_m3.iter().flatten())
        .any(|v| !v.is_finite())
    {
        return Err(CaeError::contract("molecular output reverse overflow"));
    }
    Ok(result)
}

pub(crate) fn scoped_geometry_pullback(
    values: &[f64],
    evaluate: impl FnOnce(&[ReverseScalar]) -> CaeResult<ReverseScalar>,
) -> CaeResult<Vec<f64>> {
    if values.is_empty() || values.len() > 48 {
        return Err(CaeError::contract(
            "geometry reverse scope requires 1 to 48 local parameters",
        ));
    }
    scoped_pullback_inner(values, evaluate, true)
}

pub(crate) fn scoped_pullback(
    values: &[f64],
    evaluate: impl FnOnce(&[ReverseScalar]) -> CaeResult<ReverseScalar>,
) -> CaeResult<Vec<f64>> {
    scoped_pullback_inner(values, evaluate, false)
}

fn scoped_pullback_inner(
    values: &[f64],
    evaluate: impl FnOnce(&[ReverseScalar]) -> CaeResult<ReverseScalar>,
    geometry: bool,
) -> CaeResult<Vec<f64>> {
    let _scope = Scope::enter()?;
    let variables = values
        .iter()
        .map(|v| ReverseScalar::input(*v))
        .collect::<CaeResult<Vec<_>>>()?;
    if geometry {
        CONTEXT.with(|context| {
            let mut context = context.borrow_mut();
            let c = context.as_mut().unwrap();
            c.geometry_inputs = Some(variables.iter().map(|v| v.variable.unwrap()).collect());
            c.geometry_directions.set_dimension(variables.len());
            for (j, input) in variables.iter().enumerate() {
                let variable = input.variable.unwrap();
                c.geometry_directions.reserve_nodes(variable.index()+1);
                c.geometry_edges.ensure_node(variable.index());
                let mut directions = vec![0.; variables.len()];
                directions[j] = 1.;
                if !c.geometry_directions.store(variable.index(),&directions){c.failed=Some("geometry input direction dimension differs".into());}
            }
        });
    }
    let output = evaluate(&variables)?;
    CONTEXT.with(|context| {
        let context = context.borrow();
        let c = context
            .as_ref()
            .ok_or_else(|| CaeError::contract("molecular pullback scope lost"))?;
        if let Some(error) = &c.failed {
            return Err(CaeError::contract(error.clone()));
        }
        if !output.value.is_finite() {
            return Err(CaeError::contract("molecular pullback output not finite"));
        }
        if let Some(output) = output.variable {
            let gradients = c
                .tape
                .vjp(output, &[1.])
                .map_err(|e| CaeError::contract(e.to_string()))?;
            variables
                .iter()
                .map(|v| {
                    let value = gradients
                        .wrt(v.variable.unwrap())
                        .map_err(|e| CaeError::contract(e.to_string()))?[0];
                    if !value.is_finite() {
                        return Err(CaeError::contract(
                            "molecular pullback derivative not finite",
                        ));
                    }
                    Ok(value)
                })
                .collect()
        } else {
            Ok(vec![0.; values.len()])
        }
    })
}

pub(crate) fn weighted_output_scalar<S: TransientGasScalar>(
    result: &crate::gas_multiscale_network::MultiscaleNetworkResidual<S>,
    bars: &NetworkOutputBars,
    populations: usize,
) -> CaeResult<S> {
    if populations == 0
        || result.cells.len().checked_mul(populations) != Some(result.inventory_residual_kg.len())
        || result.cells.iter().any(|c| {
            c.checked_mul(populations)
                .and_then(|x| x.checked_add(populations))
                .is_none_or(|end| end > bars.inventory_residual_kg.len())
        })
        || result
            .face_packets
            .iter()
            .any(|(i, _)| *i >= bars.face_packets.len())
    {
        return Err(CaeError::contract("molecular weighted output layout"));
    }
    let mut total = S::zero();
    for (local, cell) in result.cells.iter().enumerate() {
        for k in 0..populations {
            total += result.inventory_residual_kg[local * populations + k]
                * bars.inventory_residual_kg[cell * populations + k];
        }
    }
    for (face, packet) in &result.face_packets {
        let values = [
            packet.mass_kg,
            packet.momentum_kg_m_s[0],
            packet.momentum_kg_m_s[1],
            packet.momentum_kg_m_s[2],
            packet.kinetic_energy_j,
            packet.wall_work_j,
            packet.wall_heat_j,
        ];
        for (value, bar) in values.into_iter().zip(bars.face_packets[*face]) {
            total += value * bar;
        }
    }
    if !total.finite_first_order() {
        return Err(CaeError::contract("molecular weighted output not finite"));
    }
    Ok(total)
}
pub(crate) fn scoped_pullbacks(
    values: &[f64],
    evaluate: impl FnOnce(&[ReverseScalar]) -> CaeResult<Vec<ReverseScalar>>,
) -> CaeResult<Vec<Vec<f64>>> {
    let _scope = Scope::enter()?;
    let variables = values
        .iter()
        .map(|v| ReverseScalar::input(*v))
        .collect::<CaeResult<Vec<_>>>()?;
    let outputs = evaluate(&variables)?;
    CONTEXT.with(|context| {
        let context = context.borrow();
        let c = context
            .as_ref()
            .ok_or_else(|| CaeError::contract("molecular pullback scope lost"))?;
        if let Some(error) = &c.failed {
            return Err(CaeError::contract(error.clone()));
        }
        outputs
            .iter()
            .map(|output| {
                if !output.value.is_finite() {
                    return Err(CaeError::contract("molecular pullback output not finite"));
                }
                if let Some(output) = output.variable {
                    let gradients = c
                        .tape
                        .vjp(output, &[1.])
                        .map_err(|e| CaeError::contract(e.to_string()))?;
                    variables
                        .iter()
                        .map(|v| {
                            let value = gradients
                                .wrt(v.variable.unwrap())
                                .map_err(|e| CaeError::contract(e.to_string()))?[0];
                            if !value.is_finite() {
                                return Err(CaeError::contract(
                                    "molecular pullback derivative not finite",
                                ));
                            }
                            Ok(value)
                        })
                        .collect()
                } else {
                    Ok(vec![0.; values.len()])
                }
            })
            .collect()
    })
}

