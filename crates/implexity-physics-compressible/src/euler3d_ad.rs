// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::Scalar;
use implexity_core::contracts::{
    Evaluation, FieldValue,
};
use implexity_core::CaeResult;
use implexity_optim::design::{NamedArrays, design_identity};
use implexity_optim::provider_ops::{
    DesignSensitivities,
};
use implexity_physics_solid::structural_dynamics::{conforming_interface_map, transfer_conforming_forces};
use implexity_physics_thermofluid::rv::{Recording, Rv};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use crate::array::{Field, is_bool_array};
use crate::errors::{PResult, ModelError};
use crate::euler3d::{
    Problem, WallLayout, add_rate, check_all, divergence, face_index, faces, normalize, others, outward,
    primitive, storage,
};
use crate::pyval::{nums, repr_float, shape_repr};
use crate::quasi1d_euler::{array, scalar};

#[must_use]
pub fn face_fractions<S: Scalar>(p: &Problem, phi: &[S], axis: usize) -> Vec<S> {
    let fs = p.face_shape(axis);
    let m = p.shape[axis];
    let mut out = vec![S::zero(); fs.iter().product()];
    for i in 0..fs[0] {
        for j in 0..fs[1] {
            for k in 0..fs[2] {
                let idx = [i, j, k];
                let t = idx[axis];
                let mut lo = idx;
                lo[axis] = t.saturating_sub(1).min(m - 1);
                let mut hi = idx;
                hi[axis] = t.min(m - 1);
                let (l, r) = (phi[p.cell(lo)], phi[p.cell(hi)]);
                out[face_index(p, axis, idx)] = l * 2.0 * r / (l + r);
            }
        }
    }
    out
}


pub fn transport_rhs<S: Scalar>(q: &[[S; 5]], p: &Problem, reconstruct: bool) -> PResult<Vec<[S; 5]>> {
    let mut d = vec![[S::zero(); 5]; p.cells()];
    for axis in 0..3 {
        let f = faces(q, p, axis, reconstruct, false)?.flux;
        divergence(p, axis, &f, &mut d);
    }
    for (c, row) in d.iter_mut().enumerate() {
        if !p.fluid_mask[c] {
            *row = [S::zero(); 5];
        }
    }
    Ok(d)
}


pub fn volume_fraction_rhs<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    reconstruct: bool,
) -> PResult<Vec<[S; 5]>> {
    if !p.all_fluid() {
        return Err(ModelError::invalid("volume-fraction transport requires an all-fluid base grid"));
    }
    let pressure: Vec<S> = q.iter().map(|x| primitive(*x, p.gamma)[4]).collect();
    let mut d = vec![[S::zero(); 5]; p.cells()];
    for axis in 0..3 {
        let flux = faces(q, p, axis, reconstruct, false)?.flux;
        let fphi = face_fractions(p, phi, axis);
        let weighted: Vec<[S; 5]> = flux.iter().zip(&fphi).map(|(f, w)| f.map(|x| *w * x)).collect();
        divergence(p, axis, &weighted, &mut d);
        let h = p.spacing[axis];
        for_cells(p, |idx, c| {
            let mut next = idx;
            next[axis] += 1;
            let diff = fphi[face_index(p, axis, next)] - fphi[face_index(p, axis, idx)];
            d[c][axis + 1] += pressure[c] * diff / h;
        });
    }
    for (row, f) in d.iter_mut().zip(phi) {
        for x in row.iter_mut() {
            *x /= *f;
        }
    }
    Ok(d)
}

fn for_cells(p: &Problem, mut f: impl FnMut([usize; 3], usize)) {
    for i in 0..p.shape[0] {
        for j in 0..p.shape[1] {
            for k in 0..p.shape[2] {
                f([i, j, k], p.cell([i, j, k]));
            }
        }
    }
}


pub fn outward_boundary_rates<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    reconstruct: bool,
) -> PResult<[[S; 5]; 6]> {
    let mut rates = [[S::zero(); 5]; 6];
    for axis in 0..3 {
        let flux = faces(q, p, axis, reconstruct, false)?.flux;
        let area = p.face_area(axis);
        let m = p.shape[axis];
        let (a1, a2) = others(axis);
        for (slot, (face_layer, cell_layer, sign)) in [(0, 0, -1.0), (m, m - 1, 1.0)].into_iter().enumerate()
        {
            let mut total = [S::zero(); 5];
            for u in 0..p.shape[a1] {
                for v in 0..p.shape[a2] {
                    let mut f = [0; 3];
                    f[axis] = face_layer;
                    f[a1] = u;
                    f[a2] = v;
                    let mut c = f;
                    c[axis] = cell_layer;
                    let w = phi[p.cell(c)];
                    let fl = flux[face_index(p, axis, f)];
                    for comp in 0..5 {
                        total[comp] += w * fl[comp];
                    }
                }
            }
            rates[axis * 2 + slot] = total.map(|x| x * (sign * area));
        }
    }
    Ok(rates)
}

fn axpy<S: Scalar>(q: &[[S; 5]], dt: f64, d: &[[S; 5]]) -> Vec<[S; 5]> {
    q.iter().zip(d).map(|(a, b)| std::array::from_fn(|c| a[c] + b[c] * dt)).collect()
}


pub fn first_order_step<S: Scalar>(
    q: &[[S; 5]],
    p: &Problem,
    phi: Option<&[S]>,
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    let d = match phi {
        None => transport_rhs(q, p, false)?,
        Some(phi) => volume_fraction_rhs(q, p, phi, false)?,
    };
    Ok(axpy(q, dt, &d))
}


pub fn fixed_history(q0: &[[f64; 5]], p: &Problem, dt: f64, count: usize) -> PResult<Vec<Vec<[f64; 5]>>> {
    history(q0, count, |q| first_order_step(q, p, None, dt))
}


pub fn volume_fraction_history(
    q0: &[[f64; 5]],
    p: &Problem,
    phi: &[f64],
    dt: f64,
    count: usize,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    history(q0, count, |q| first_order_step(q, p, Some(phi), dt))
}

fn history(
    q0: &[[f64; 5]],
    count: usize,
    mut step: impl FnMut(&[[f64; 5]]) -> PResult<Vec<[f64; 5]>>,
) -> PResult<Vec<Vec<[f64; 5]>>> {
    let mut states = Vec::with_capacity(count + 1);
    states.push(q0.to_vec());
    for n in 0..count {
        let next = step(&states[n])?;
        states.push(next);
    }
    Ok(states)
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryResponses {
    pub final_boundary_rates: [[f64; 5]; 6],
    pub boundary_exchanges: [[f64; 5]; 6],
    pub initial_storage: [f64; 5],
    pub final_storage: [f64; 5],
    pub fluid_volume_m3: f64,
}

impl HistoryResponses {
    #[must_use]
    pub fn to_value(&self) -> Value {
        let rows = |r: &[[f64; 5]; 6]| Value::Array(r.iter().map(|x| nums(x)).collect());
        json!({"final_boundary_rates": rows(&self.final_boundary_rates), "boundary_exchanges": rows(&self.boundary_exchanges),
               "initial_storage": nums(&self.initial_storage), "final_storage": nums(&self.final_storage),
               "fluid_volume_m3": self.fluid_volume_m3})
    }
}

fn weighted_storage(p: &Problem, q: &[[f64; 5]], phi: &[f64]) -> [f64; 5] {
    let mut out = [0.0; 5];
    for (c, row) in q.iter().enumerate() {
        for k in 0..5 {
            out[k] += phi[c] * row[k];
        }
    }
    out.map(|x| x * p.volume())
}


pub fn history_responses(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    phi: &[f64],
    dt: f64,
) -> PResult<HistoryResponses> {
    let rates: Vec<[[f64; 5]; 6]> =
        states.iter().map(|q| outward_boundary_rates(q, p, phi, false)).collect::<PResult<_>>()?;
    let mut exchanges = [[0.0; 5]; 6];
    for r in &rates[..rates.len() - 1] {
        for (e, x) in exchanges.iter_mut().zip(r) {
            for k in 0..5 {
                e[k] += x[k];
            }
        }
    }
    Ok(HistoryResponses {
        final_boundary_rates: rates[rates.len() - 1],
        boundary_exchanges: exchanges.map(|r| r.map(|x| x * dt)),
        initial_storage: weighted_storage(p, &states[0], phi),
        final_storage: weighted_storage(p, &states[states.len() - 1], phi),
        fluid_volume_m3: phi.iter().sum::<f64>() * p.volume(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct WallForceHistory {
    pub times_s: Vec<f64>,
    pub face_forces_n: Vec<Vec<[f64; 3]>>,
    pub nodal_forces_n: Vec<Vec<[f64; 3]>>,
    pub face_impulse_ns: Vec<[f64; 3]>,
    pub nodal_impulse_ns: Vec<[f64; 3]>,
}

fn impulse(samples: &[Vec<[f64; 3]>], dt: f64) -> Vec<[f64; 3]> {
    let n = samples.first().map_or(0, Vec::len);
    let mut out = vec![[0.0; 3]; n];
    for s in &samples[..samples.len().saturating_sub(1)] {
        for (o, x) in out.iter_mut().zip(s) {
            for a in 0..3 {
                o[a] += x[a];
            }
        }
    }
    out.into_iter().map(|r| r.map(|x| x * dt)).collect()
}

pub type WallForces = (Vec<[f64; 3]>, Vec<[f64; 3]>);


pub fn wall_forces(
    q: &[[f64; 5]],
    p: &Problem,
    layout: &WallLayout,
    reconstruct: bool,
) -> PResult<WallForces> {
    let fluxes: Vec<Vec<[f64; 5]>> =
        (0..3).map(|axis| faces(q, p, axis, reconstruct, false).map(|f| f.flux)).collect::<PResult<_>>()?;
    let face: Vec<[f64; 3]> = (0..layout.face_axes.len())
        .map(|f| {
            let axis = layout.face_axes[f];
            let flux = fluxes[axis][face_index(p, axis, layout.face_indices[f])];
            let scale = -layout.normals[f][axis] * layout.face_areas[f];
            [flux[1] * scale, flux[2] * scale, flux[3] * scale]
        })
        .collect();
    let mut nodal = vec![[0.0; 3]; layout.node_positions.len()];
    for (f, nodes) in layout.face_nodes.iter().enumerate() {
        for node in nodes {
            for a in 0..3 {
                nodal[*node][a] += face[f][a] / 4.0;
            }
        }
    }
    Ok((face, nodal))
}


pub fn wall_force_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    reconstruct: bool,
) -> PResult<WallForceHistory> {
    let mut face = Vec::with_capacity(states.len());
    let mut nodal = Vec::with_capacity(states.len());
    for q in states {
        let (f, n) = wall_forces(q, p, layout, reconstruct)?;
        face.push(f);
        nodal.push(n);
    }
    #[allow(clippy::cast_precision_loss)]
    let times_s = (0..states.len()).map(|n| n as f64 * dt).collect();
    Ok(WallForceHistory {
        times_s,
        face_impulse_ns: impulse(&face, dt),
        nodal_impulse_ns: impulse(&nodal, dt),
        face_forces_n: face,
        nodal_forces_n: nodal,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct SolidLoadHistory {
    pub times_s: Vec<f64>,
    pub nodal_forces_n: Vec<Vec<[f64; 3]>>,
    pub nodal_impulse_ns: Vec<[f64; 3]>,
    pub source_to_solid_node_indices: Vec<usize>,
}


pub fn conforming_solid_load_history(
    states: &[Vec<[f64; 5]>],
    p: &Problem,
    layout: &WallLayout,
    dt: f64,
    solid_nodes: &[[f64; 3]],
) -> PResult<SolidLoadHistory> {
    let indices = conforming_interface_map(&layout.node_positions, solid_nodes)?;
    let loads = wall_force_history(states, p, layout, dt, false)?;
    let n = solid_nodes.len();
    Ok(SolidLoadHistory {
        times_s: loads.times_s,
        nodal_forces_n: loads
            .nodal_forces_n
            .iter()
            .map(|f| transfer_conforming_forces(f, &indices, n))
            .collect(),
        nodal_impulse_ns: transfer_conforming_forces(&loads.nodal_impulse_ns, &indices, n),
        source_to_solid_node_indices: indices,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyMap {
    pub filter_radius_m: f64,
    pub projection_beta: f64,
    pub projection_eta: f64,
    pub minimum_fluid_fraction: f64,
    pub design_region: Vec<bool>,
    pub fixed_design: Vec<f64>,
    pub filter_kernel: Vec<f64>,
    pub reach: [usize; 3],
    pub shape: [usize; 3],
}


pub fn normalize_topology_map(
    settings: &Value,
    shape: [usize; 3],
    spacing: [f64; 3],
) -> PResult<TopologyMap> {
    const REQUIRED: [&str; 6] = [
        "design_region",
        "filter_radius_m",
        "fixed_design",
        "minimum_fluid_fraction",
        "projection_beta",
        "projection_eta",
    ];
    let exact = settings
        .as_object()
        .is_some_and(|m| m.len() == REQUIRED.len() && REQUIRED.iter().all(|k| m.contains_key(*k)));
    if !exact {
        return Err(ModelError::invalid(
            "topology map requires explicit filter, projection, fraction floor and fixed design scope",
        ));
    }
    let radius = scalar(&settings["filter_radius_m"], "filter_radius_m")?;
    let beta = scalar(&settings["projection_beta"], "projection_beta")?;
    let eta = scalar(&settings["projection_eta"], "projection_eta")?;
    let floor = scalar(&settings["minimum_fluid_fraction"], "minimum_fluid_fraction")?;
    if radius < 0.0 || beta < 0.0 || !(0.0 < eta && eta < 1.0) || !(0.0 < floor && floor < 1.0) {
        return Err(ModelError::invalid("invalid topology filter/projection controls"));
    }
    let region_value = &settings["design_region"];
    let region = crate::array::bool_mask(region_value)
        .filter(|(s, _)| s.as_slice() == shape.as_slice() && is_bool_array(region_value))
        .map(|(_, v)| v)
        .ok_or_else(|| ModelError::invalid("design_region must be a boolean cell array"))?;
    let fixed = array(&settings["fixed_design"], "fixed_design", Some(&shape))?.values;
    if fixed.iter().any(|v| *v < 0.0 || *v > 1.0) {
        return Err(ModelError::invalid("fixed_design must lie in [0,1]"));
    }
    if spacing.iter().any(|h| *h <= 0.0) {
        return Err(ModelError::invalid("positive spacing required for physical-radius filter"));
    }
    let (kernel, reach) = if radius == 0.0 {
        (vec![1.0], [0; 3])
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let reach: [usize; 3] =
            std::array::from_fn(|a| ((radius / spacing[a]).ceil() as usize).min(shape[a] - 1));
        let mut kernel = Vec::new();
        let span = |a: usize| -> Vec<f64> {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_wrap)]
            (0..=2 * reach[a]).map(|i| (i as i64 - reach[a] as i64) as f64 * spacing[a]).collect()
        };
        let (xs, ys, zs) = (span(0), span(1), span(2));
        for x in &xs {
            for y in &ys {
                for z in &zs {
                    let distance = (0.0 + x * x + y * y + z * z).sqrt();
                    kernel.push((1.0 - distance / radius).max(0.0));
                }
            }
        }
        (kernel, reach)
    };
    Ok(TopologyMap {
        filter_radius_m: radius,
        projection_beta: beta,
        projection_eta: eta,
        minimum_fluid_fraction: floor,
        design_region: region,
        fixed_design: fixed,
        filter_kernel: kernel,
        reach,
        shape,
    })
}

#[must_use]
pub fn topology_fraction<S: Scalar>(design: &[S], map: &TopologyMap) -> Vec<S> {
    let shape = map.shape;
    let values: Vec<S> = (0..design.len())
        .map(|c| if map.design_region[c] { design[c] } else { S::from_f64(map.fixed_design[c]) })
        .collect();
    let extent = map.reach.map(|r| 2 * r + 1);
    let cell = |i: [usize; 3]| (i[0] * shape[1] + i[1]) * shape[2] + i[2];
    let mut out = Vec::with_capacity(design.len());
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let c = cell([i, j, k]);
                if !map.design_region[c] {
                    out.push(S::from_f64(map.fixed_design[c]));
                    continue;
                }
                let mut num = S::zero();
                let mut den = 0.0;
                for a in 0..extent[0] {
                    for b in 0..extent[1] {
                        for d in 0..extent[2] {
                            let w = map.filter_kernel[(a * extent[1] + b) * extent[2] + d];
                            let at = [i + a, j + b, k + d];
                            let inside =
                                (0..3).all(|x| at[x] >= map.reach[x] && at[x] - map.reach[x] < shape[x]);
                            if inside {
                                let src = cell(std::array::from_fn(|x| at[x] - map.reach[x]));
                                num += values[src] * w;
                                den += w;
                            }
                        }
                    }
                }
                out.push(num / den);
            }
        }
    }
    let (beta, eta, floor) = (map.projection_beta, map.projection_eta, map.minimum_fluid_fraction);
    out.into_iter()
        .map(|filtered| {
            let projected = if beta == 0.0 {
                filtered
            } else {
                let base = (beta * eta).tanh();
                (S::from_f64(base) + ((filtered - eta) * beta).tanh()) / (base + (beta * (1.0 - eta)).tanh())
            };
            projected * (1.0 - floor) + floor
        })
        .collect()
}


pub fn checked_topology_fraction(design: &Field, map: &TopologyMap) -> PResult<Vec<f64>> {
    if design.shape.as_slice() != map.shape.as_slice() || !design.values.iter().all(Scalar::is_finite) {
        return Err(ModelError::invalid(format!(
            "topology design has nonfinite values or incorrect shape; expected {}",
            shape_repr(&map.shape)
        )));
    }
    if design.values.iter().any(|v| *v < 0.0 || *v > 1.0) {
        return Err(ModelError::invalid("topology design must lie in [0,1]"));
    }
    let result = topology_fraction(&design.values, map);
    if result.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 1.0) {
        return Err(ModelError::invalid("invalid filtered/projected fluid fraction"));
    }
    Ok(result)
}

#[must_use]
pub fn exact_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().or(Some(i64::MAX)),
        _ => None,
    }
}

#[must_use]
pub fn schedule_matches(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * b.abs()
}

#[must_use]
pub fn real_step(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite() && *v > 0.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmittedHistory {
    pub states: Vec<Vec<[f64; 5]>>,
    pub fluid_fraction: Vec<f64>,
    pub step_s: f64,
    pub step_count: usize,
    pub maximum_cfl: f64,
    pub scaled_balance_error: [f64; 5],
    pub info: Map<String, Value>,
}


fn fixed_schedule(
    p: &Problem,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
) -> PResult<(f64, usize, i64)> {
    let count = exact_int(step_count).filter(|c| (1..=p.max_steps).contains(c));
    let Some(count) = count else {
        return Err(ModelError::invalid("fixed step count must be an integer within max_steps"));
    };
    let Some(dt) = real_step(step_s) else {
        return Err(ModelError::invalid("fixed time step must be positive finite seconds"));
    };
    #[allow(clippy::cast_precision_loss)]
    if !schedule_matches(dt * count as f64, p.end_time) {
        return Err(ModelError::invalid("fixed schedule must reach exactly the authored end time"));
    }
    let history_bytes = history_bytes(p, count);
    if exact_int(budget).is_none_or(|b| b < history_bytes) {
        return Err(ModelError::invalid(format!(
            "fixed history requires {history_bytes} output bytes within the explicit budget"
        )));
    }
    Ok((dt, usize::try_from(count).unwrap_or(usize::MAX), history_bytes))
}

#[must_use]
pub fn history_bytes(p: &Problem, count: i64) -> i64 {
    (count + 1).saturating_mul(i64::try_from(p.cells() * 40).unwrap_or(i64::MAX))
}

fn max_masked(rate: &[f64], mask: &[bool]) -> f64 {
    let mut m = f64::NEG_INFINITY;
    for (r, k) in rate.iter().zip(mask) {
        if *k {
            if r.is_nan() {
                return f64::NAN;
            }
            m = m.max(*r);
        }
    }
    m
}


#[allow(clippy::too_many_lines)]
pub fn checked_fixed_history(
    problem: &Value,
    step_s: &Value,
    step_count: &Value,
    budget: &Value,
    fluid_fraction: Option<&[f64]>,
) -> PResult<AdmittedHistory> {
    let p = normalize(problem)?;
    let mut phi = vec![1.0; p.cells()];
    if let Some(f) = fluid_fraction {
        if f.len() != p.cells() || !f.iter().all(Scalar::is_finite) {
            return Err(ModelError::invalid(format!(
                "fluid_fraction has nonfinite values or incorrect shape; expected {}",
                shape_repr(&p.shape)
            )));
        }
        if f.iter().any(|v| *v <= 0.0 || *v > 1.0) {
            return Err(ModelError::invalid("fluid fractions must satisfy 0 < phi <= 1"));
        }
        if !p.all_fluid() {
            return Err(ModelError::invalid("fluid-fraction history requires an all-fluid base grid"));
        }
        phi = f.to_vec();
    }
    let (dt, count, bytes) = fixed_schedule(&p, step_s, step_count, budget)?;
    let q0: Vec<[f64; 5]> = crate::euler3d::initial_state(&p);
    let states = if fluid_fraction.is_none() {
        fixed_history(&q0, &p, dt, count)?
    } else {
        volume_fraction_history(&q0, &p, &phi, dt, count)?
    };
    let vol = p.volume();
    let mut exchange = [0.0; 5];
    let mut impulse = [0.0; 5];
    let mut max_cfl: f64 = 0.0;
    for (index, q) in states.iter().enumerate() {
        let w = check_all(q, p.gamma)?;
        let mut rate = vec![0.0; p.cells()];
        let mut out = [0.0; 5];
        let mut source = [0.0; 5];
        for axis in 0..3 {
            let fc = faces(q, &p, axis, false, true)?;
            if fluid_fraction.is_none() {
                add_rate(&p, axis, &fc.speed, None, &mut rate);
                let o = outward(&p, axis, &fc.flux);
                for c in 0..5 {
                    out[c] += o[c];
                }
            } else {
                let fphi = face_fractions(&p, &phi, axis);
                add_rate(&p, axis, &fc.speed, Some((&fphi, &phi)), &mut rate);
                let m = p.shape[axis];
                let area = vol / p.spacing[axis];
                let (a1, a2) = others(axis);
                let layer = |t: usize| {
                    let mut s = [0.0; 5];
                    for u in 0..p.shape[a1] {
                        for v in 0..p.shape[a2] {
                            let mut idx = [0; 3];
                            idx[axis] = t;
                            idx[a1] = u;
                            idx[a2] = v;
                            let fi = face_index(&p, axis, idx);
                            for (sc, f) in s.iter_mut().zip(fc.flux[fi]) {
                                *sc += fphi[fi] * f;
                            }
                        }
                    }
                    s
                };
                let (hi, lo) = (layer(m), layer(0));
                for c in 0..5 {
                    out[c] += (hi[c] - lo[c]) * area;
                }
                let mut total = 0.0;
                for_cells(&p, |idx, c| {
                    let mut next = idx;
                    next[axis] += 1;
                    total += w[c][4] * (fphi[face_index(&p, axis, next)] - fphi[face_index(&p, axis, idx)]);
                });
                source[axis + 1] = total * vol / p.spacing[axis];
            }
        }
        if index < count {
            let cfl = dt * max_masked(&rate, &p.fluid_mask);
            max_cfl = if cfl.is_nan() { max_cfl } else { max_cfl.max(cfl) };
            if !cfl.is_finite() || cfl > p.cfl * (1.0 + 1e-12) {
                return Err(ModelError::invalid(format!(
                    "fixed schedule exceeds CFL at step {index}: {}",
                    repr_float(cfl)
                )));
            }
            for c in 0..5 {
                exchange[c] += dt * out[c];
                impulse[c] += dt * source[c];
            }
        }
    }
    let initial = storage(&p, &states[0], Some(&phi));
    let final_ = storage(&p, &states[count], Some(&phi));
    let mut balance = [0.0; 5];
    let mut scaled = [0.0; 5];
    for c in 0..5 {
        balance[c] = final_[c] - initial[c] + exchange[c] - impulse[c];
        scaled[c] =
            balance[c] / (initial[c].abs() + final_[c].abs() + exchange[c].abs() + impulse[c].abs()).max(1.0);
    }
    if scaled.iter().map(|x| x.abs()).fold(0.0, f64::max) > 1e-10 {
        return Err(ModelError::invalid("fixed-history conservation ledger failed"));
    }
    let mut info = Map::new();
    info.insert("status".into(), json!("completed_admitted_fixed_history"));
    info.insert("step_s".into(), json!(dt));
    info.insert("step_count".into(), json!(count));
    info.insert("maximum_cfl".into(), json!(max_cfl));
    info.insert("history_bytes".into(), json!(bytes));
    info.insert("outward_boundary_integrals".into(), nums(&exchange));
    info.insert("geometry_pressure_impulse".into(), nums(&impulse[1..4]));
    info.insert("initial_mass_momentum_energy".into(), nums(&initial));
    info.insert("final_mass_momentum_energy".into(), nums(&final_));
    info.insert("scaled_balance_error".into(), nums(&scaled));
    info.insert("optimization_supported".into(), json!(false));
    info.insert(
        "scope".into(),
        json!("Trajectory admission only; optimization requires the separate design provider. Physical fraction/design qualification remains pending."),
    );
    Ok(AdmittedHistory {
        states,
        fluid_fraction: phi,
        step_s: dt,
        step_count: count,
        maximum_cfl: max_cfl,
        scaled_balance_error: scaled,
        info,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    FirstOrder,
    Muscl,
}

pub const COORDINATE: &str = "model:control";

pub const RATE_NAMES: [&str; 5] =
    ["mass_flow_kg_s", "momentum_x_N", "momentum_y_N", "momentum_z_N", "energy_flow_W"];

const RATE_UNITS: [&str; 5] = ["kg/s", "N", "N", "N", "W"];

pub const NAME: &str = "cartesian_euler3d_topology";

#[must_use]
pub fn response_names() -> Vec<String> {
    let mut out: Vec<String> = crate::euler3d::FACE_NAMES
        .iter()
        .flat_map(|f| RATE_NAMES.iter().map(move |r| format!("{f}_{r}")))
        .collect();
    out.push("fluid_volume_m3".into());
    out
}

pub fn response_units() -> Vec<&'static str> {
    let mut u: Vec<&str> = (0..6).flat_map(|_| RATE_UNITS).collect();
    u.push("m^3");
    u
}


pub fn scheme_step<S: Scalar>(
    scheme: Scheme,
    q: &[[S; 5]],
    p: &Problem,
    phi: &[S],
    dt: f64,
) -> PResult<Vec<[S; 5]>> {
    match scheme {
        Scheme::FirstOrder => first_order_step(q, p, Some(phi), dt),
        Scheme::Muscl => {
            let stage = axpy(q, dt, &volume_fraction_rhs(q, p, phi, true)?);
            let forward = axpy(&stage, dt, &volume_fraction_rhs(&stage, p, phi, true)?);
            Ok(q.iter()
                .zip(&forward)
                .map(|(a, b)| std::array::from_fn(|c| a[c] * 0.5 + b[c] * 0.5))
                .collect())
        }
    }
}


pub fn response_vector<S: Scalar>(scheme: Scheme, q: &[[S; 5]], p: &Problem, phi: &[S]) -> PResult<Vec<S>> {
    let rates = outward_boundary_rates(q, p, phi, scheme == Scheme::Muscl)?;
    let mut out: Vec<S> = rates.iter().flatten().copied().collect();
    let mut total = S::zero();
    for f in phi {
        total += *f;
    }
    out.push(total * p.volume());
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Parts {
    pub problem: Problem,
    pub map: TopologyMap,
    pub raw: Vec<f64>,
    pub admitted: AdmittedHistory,
}

pub fn exact_keys(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EulerTopology { pub scheme: Scheme }
impl EulerTopology {
 pub const fn id(&self)-> &'static str { match self.scheme { Scheme::FirstOrder=>NAME,Scheme::Muscl=>crate::euler3d_muscl::NAME } }

    pub fn parts(&self, problem: &Value, design: &NamedArrays) -> PResult<Parts> {
        if !exact_keys(problem, &["flow", "time_integration", "topology_map"]) {
            return Err(ModelError::invalid(
                "Euler topology requires flow, topology_map and time_integration",
            ));
        }
        let timing = &problem["time_integration"];
        if !exact_keys(timing, &["history_byte_budget", "step_count", "step_s"]) {
            return Err(ModelError::invalid(
                "explicit fixed time step, count and output history budget required",
            ));
        }
        if design.len() != 1 || !design.contains(COORDINATE) {
            return Err(ModelError::invalid(
                "Euler topology requires exactly the model:control coordinate",
            ));
        }
        let p = normalize(&problem["flow"])?;
        let map = normalize_topology_map(&problem["topology_map"], p.shape, p.spacing)?;
        let raw = design.get(COORDINATE).ok_or_else(|| {
            ModelError::invalid("Euler topology requires exactly the model:control coordinate")
        })?;
        if raw.shape() != p.shape.as_slice() || !raw.iter().all(Scalar::is_finite) {
            return Err(ModelError::invalid(format!(
                "{COORDINATE} has nonfinite values or incorrect shape; expected {}",
                shape_repr(&p.shape)
            )));
        }
        let raw_field = Field::new(p.shape.to_vec(), raw.iter().copied().collect());
        let phi = checked_topology_fraction(&raw_field, &map)?;
        let admitted = match self.scheme {
            Scheme::FirstOrder => checked_fixed_history(
                &problem["flow"],
                &timing["step_s"],
                &timing["step_count"],
                &timing["history_byte_budget"],
                Some(&phi),
            )?,
            Scheme::Muscl => crate::euler3d_muscl::admit_history(&problem["flow"], timing, &phi)?,
        };
        Ok(Parts { problem: p, map, raw: raw_field.values, admitted })
    }

fn diagnostics(self, admitted: &AdmittedHistory) -> Map<String, Value> {
        let mut m = admitted.info.clone();
        m.insert(
            "response_convention".into(),
            json!("Final instantaneous signed outward numerical flux; momentum includes pressure traction."),
        );
        m.insert(
            "derivative_scope".into(),
            json!(match self.scheme {
                Scheme::FirstOrder =>
                    "Fixed time schedule, fixed flow data, filtered/projected relaxed topology.",
                Scheme::Muscl => "Fixed schedule, relaxed topology and branch-local MC limiter derivatives.",
            }),
        );
        m.insert("physical_qualification".into(), json!(false));
        m.insert("experimental_provider".into(), json!(true));
        m
    }


    pub fn preflight_design_value(
        &self,
        problem: &Value,
        design: &NamedArrays,
    ) -> PResult<Map<String, Value>> {
        let parts = self.parts(problem, design)?;
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("issues".into(), json!([]));
        m.insert("maximum_cfl".into(), json!(parts.admitted.maximum_cfl));
        m.insert("physical_qualification".into(), json!(false));
        Ok(m)
    }


    pub fn admission_value(
        &self,
        problem: &Value,
        current: &NamedArrays,
        candidate: &NamedArrays,
    ) -> CaeResult<Value> {
        let mut out = Map::new();
        out.insert("current_design_state_id".into(), json!(design_identity(current)?));
        out.insert("candidate_design_state_id".into(), json!(design_identity(candidate)?));
        match self.parts(problem, candidate) {
            Err(e) if e.is_validation() => {
                out.insert("allow".into(), json!(false));
                out.insert("reason".into(), json!(e.to_string()));
                out.insert("diagnostics".into(), json!({"flow_admitted": false, "failure_code": e.code()}));
            }
            Err(e) => return Err(e.into()),
            Ok(parts) => {
                out.insert("allow".into(), json!(true));
                out.insert("reason".into(), json!("Complete fixed trajectory admitted"));
                out.insert(
                    "diagnostics".into(),
                    json!({"flow_admitted": true, "maximum_cfl": parts.admitted.maximum_cfl,
                           "scaled_balance_error": nums(&parts.admitted.scaled_balance_error)}),
                );
            }
        }
        Ok(Value::Object(out))
    }


    pub fn evaluate_design_value(&self, problem: &Value, design: &NamedArrays) -> PResult<Evaluation> {
        let parts = self.parts(problem, design)?;
        let (p, admitted) = (&parts.problem, &parts.admitted);
        let last = &admitted.states[admitted.step_count];
        let values = response_vector(self.scheme, last, p, &admitted.fluid_fraction)?;
        let responses = response_names().into_iter().zip(values).collect();
        let w: Vec<[f64; 5]> = last.iter().map(|q| primitive(*q, p.gamma)).collect();
        let s = p.shape.to_vec();
        let mut s3 = s.clone();
        s3.push(3);
        let col = |c: usize| w.iter().map(|x| x[c]).collect::<Vec<f64>>();
        let fields = [
            ("fluid_fraction", Field::new(s.clone(), admitted.fluid_fraction.clone())),
            ("density_kg_m3", Field::new(s.clone(), col(0))),
            ("velocity_m_s", Field::new(s3, w.iter().flat_map(|x| [x[1], x[2], x[3]]).collect())),
            ("pressure_Pa", Field::new(s.clone(), col(4))),
            ("temperature_K", Field::new(s, w.iter().map(|x| x[4] / (x[0] * p.gas)).collect())),
        ];
        Ok(Evaluation {
            provider: self.id().into(),
            responses,
            diagnostics: self.diagnostics(admitted),
            fields: fields
                .into_iter()
                .map(|(k, f)| (k.to_string(), FieldValue::Array(f.to_array())))
                .collect(),
        })
    }


    pub fn sensitivities_value(
        &self,
        problem: &Value,
        design: &NamedArrays,
        responses: &[String],
    ) -> PResult<DesignSensitivities> {
        let names = response_names();
        let mut unique = responses.to_vec();
        unique.sort();
        unique.dedup();
        if responses.is_empty()
            || unique.len() != responses.len()
            || responses.iter().any(|r| !names.contains(r))
        {
            return Err(ModelError::invalid("unknown, empty or duplicated Euler topology response"));
        }
        let parts = self.parts(problem, design)?;
        let indices: Vec<usize> =
            responses.iter().filter_map(|r| names.iter().position(|n| n == r)).collect();
        let (values, gradients) = self.reverse_sweep(&parts, &indices)?;
        let mut out = DesignSensitivities {
            diagnostics: self.diagnostics(&parts.admitted),
            ..DesignSensitivities::default()
        };
        for ((name, value), gradient) in responses.iter().zip(values).zip(gradients) {
            if !gradient.iter().all(Scalar::is_finite) {
                return Err(ModelError::invalid("nonfinite Euler topology derivative"));
            }
            out.responses.insert(name.clone(), value);
            let array = ArrayD::from_shape_vec(IxDyn(&parts.problem.shape), gradient)
                .map_err(|e| ModelError::invalid(e.to_string()))?;
            out.gradients.insert(name.clone(), NamedArrays::single(COORDINATE, array));
        }
        Ok(out)
    }

    fn reverse_sweep(self, parts: &Parts, indices: &[usize]) -> PResult<(Vec<f64>, Vec<Vec<f64>>)> {
        let p = &parts.problem;
        let admitted = &parts.admitted;
        let phi = &admitted.fluid_fraction;
        let n_state = p.cells() * 5;
        let pack = |q: &[[f64; 5]]| -> Vec<f64> {
            let mut v: Vec<f64> = q.iter().flatten().copied().collect();
            v.extend_from_slice(phi);
            v
        };
        let unpack = |x: &[Rv]| -> (Vec<[Rv; 5]>, Vec<Rv>) {
            let q = x[..n_state].chunks(5).map(|c| [c[0], c[1], c[2], c[3], c[4]]).collect();
            (q, x[n_state..].to_vec())
        };
        let r = indices.len();
        let mut lam: Vec<Vec<f64>> = Vec::with_capacity(r);
        let mut gphi: Vec<Vec<f64>> = Vec::with_capacity(r);
        let mut values = Vec::with_capacity(r);
        {
            let rec = Recording::start()?;
            let x = rec.inputs(&pack(&admitted.states[admitted.step_count]));
            let (q, f) = unpack(&x);
            let out = response_vector(self.scheme, &q, p, &f)?;
            let selected: Vec<Rv> = indices.iter().map(|i| out[*i]).collect();
            for k in 0..r {
                values.push(selected[k].value());
                let mut cot = vec![0.0; r];
                cot[k] = 1.0;
                let g = rec.vjp(&selected, &cot, &x);
                lam.push(g[..n_state].to_vec());
                gphi.push(g[n_state..].to_vec());
            }
        }
        for n in (0..admitted.step_count).rev() {
            let rec = Recording::start()?;
            let x = rec.inputs(&pack(&admitted.states[n]));
            let (q, f) = unpack(&x);
            let next = scheme_step(self.scheme, &q, p, &f, admitted.step_s)?;
            let flat: Vec<Rv> = next.into_iter().flatten().collect();
            for k in 0..r {
                let g = rec.vjp(&flat, &lam[k], &x);
                lam[k] = g[..n_state].to_vec();
                for (a, b) in gphi[k].iter_mut().zip(&g[n_state..]) {
                    *a += b;
                }
            }
        }
        let rec = Recording::start()?;
        let x = rec.inputs(&parts.raw);
        let mapped = topology_fraction(&x, &parts.map);
        let gradients = gphi.iter().map(|g| rec.vjp(&mapped, g, &x)).collect();
        Ok((values, gradients))
    }
}

impl EulerTopology {

    pub fn validate(problem: &Value) -> PResult<()> {
        if !exact_keys(problem, &["flow", "time_integration", "topology_map"]) {
            return Err(ModelError::invalid(
                "Euler topology requires flow, topology_map and time_integration",
            ));
        }
        let p = normalize(&problem["flow"])?;
        if !p.all_fluid() {
            return Err(ModelError::invalid(
                "Euler topology uses relaxed fractions, not a fixed solid mask",
            ));
        }
        normalize_topology_map(&problem["topology_map"], p.shape, p.spacing)?;
        let timing = &problem["time_integration"];
        if !exact_keys(timing, &["history_byte_budget", "step_count", "step_s"]) {
            return Err(ModelError::invalid(
                "explicit fixed time step, count and output history budget required",
            ));
        }
        let dt = scalar(&timing["step_s"], "step_s")?;
        let count = exact_int(&timing["step_count"]).filter(|c| (1..=p.max_steps).contains(c));
        let Some(count) = count.filter(|_| dt > 0.0) else {
            return Err(ModelError::invalid(
                "positive fixed time step and valid integer count required",
            ));
        };
        #[allow(clippy::cast_precision_loss)]
        if !schedule_matches(dt * count as f64, p.end_time) {
            return Err(ModelError::invalid("fixed schedule must reach the authored end time"));
        }
        if exact_int(&timing["history_byte_budget"]).is_none_or(|b| b < history_bytes(&p, count)) {
            return Err(ModelError::invalid("output history exceeds the explicit memory budget"));
        }
        Ok(())
    }
}
