// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::contracts::FieldValue;
use implexity_core::history_field_sources::HistorySourceComponent;
use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{
    CoupledHistoryAssembly, HistoryBlock, HistoryBlockCallbacks, HistoryInterface,
};
use implexity_solve::local_assembly::{Kind, LocalResidual, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;

use crate::common::{
    assembly_options, design_incidence, err, extend_coupling, grid_of, incidence, py_int, real_array,
    times_match, zero_csr,
};
use crate::host::{
    BoundFieldSource, BoundSource, FieldHost, FieldSourceAuthoring, ResponseVjp, SourceEnergy, block_start,
    host_of,
};
use crate::prescribed_joule::{array, strings};

pub const NAME: &str = "native_resolved_electrothermal";
pub const BLOCK: &str = "electrical_potential";
const DATA: &str = include_str!("data/electrothermal.json");
const STATE_CONTRACT: &str = "prescribed_zero_state_source_v1";

fn data() -> Value {
    serde_json::from_str(DATA).unwrap_or(Value::Null)
}

#[must_use]
pub fn limitations() -> Vec<String> {
    strings(&data()["runtime_support"]["limitations"])
}

#[derive(Clone, Copy, Debug)]
pub struct ElementTerms<S> {
    pub potential_residual: [S; 4],
    pub thermal_load: [S; 4],
    pub electric_field: [S; 3],
    pub current_density: [S; 3],
    pub joule_power: S,
    pub conductivity: S,
    pub volume: S,
    pub gradient_partition_error: f64,
}

pub fn electrothermal_element<S: Scalar>(
    potential: &[S; 4],
    conductivity: S,
    gradients: &[[S; 3]; 4],
    volume: S,
    impressed: Option<[S; 3]>,
) -> ElementTerms<S> {
    let mut electric: [S; 3] =
        std::array::from_fn(|a| -(0..4).fold(S::zero(), |acc, i| acc + potential[i] * gradients[i][a]));
    if let Some(e) = impressed {
        for a in 0..3 {
            electric[a] = electric[a] + e[a];
        }
    }
    let current: [S; 3] = std::array::from_fn(|a| conductivity * electric[a]);
    let residual: [S; 4] = std::array::from_fn(|i| {
        -(volume * (0..3).fold(S::zero(), |acc, a| acc + gradients[i][a] * current[a]))
    });
    let power = volume * (0..3).fold(S::zero(), |acc, a| acc + current[a] * electric[a]);
    let quarter = power / S::from_f64(4.0);
    let partition =
        (0..3).map(|a| (0..4).map(|i| gradients[i][a].value()).sum::<f64>().abs()).fold(0.0_f64, f64::max);
    ElementTerms {
        potential_residual: residual,
        thermal_load: [quarter; 4],
        electric_field: electric,
        current_density: current,
        joule_power: power,
        conductivity,
        volume,
        gradient_partition_error: partition,
    }
}

#[derive(Clone, Debug)]
pub struct ElectrothermalMesh {
    pub n: usize,
    pub cells: Vec<[usize; 4]>,
    pub electrodes: Vec<usize>,
    pub free_nodes: Vec<usize>,
    pub gradients: Vec<[[f64; 3]; 4]>,
    pub volumes: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshAssembly {
    pub charge_residual_a: Vec<f64>,
    pub thermal_load_w: Vec<f64>,
    pub free_charge_residual_a: Vec<f64>,
    pub electrode_error_v: Vec<f64>,
    pub electrode_current_into_domain_a: Vec<f64>,
    pub joule_power_w: f64,
    pub conductivity_margin: f64,
    pub nodal_power_identity_error_w: f64,
}

fn inv3(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let c = |r0: usize, c0: usize, r1: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    Some([
        [c(1, 1, 2, 2) / det, -c(0, 1, 2, 2) / det, c(0, 1, 1, 2) / det],
        [-c(1, 0, 2, 2) / det, c(0, 0, 2, 2) / det, -c(0, 0, 1, 2) / det],
        [c(1, 0, 2, 1) / det, -c(0, 0, 2, 1) / det, c(0, 0, 1, 1) / det],
    ])
}

fn det3(m: [[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

impl ElectrothermalMesh {

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn new(vertices: &Value, tetrahedra: &Value, electrode_nodes: &Value) -> CaeResult<Self> {
        let v = implexity_physics_solid::util::real_array(vertices);
        let Some((vs, vd)) = v.filter(|(s, d)| s.len() == 2 && s[1] == 3 && d.iter().all(|x| x.is_finite()))
        else {
            return Err(err("vertices must be finite real [node,3] SI coordinates"));
        };
        let n = vs[0];
        let int_array = |value: &Value| implexity_physics_solid::util::int_array(value);
        let cells = int_array(tetrahedra).filter(|(s, d)| {
            s.len() == 2 && s[1] == 4 && s[0] > 0 && d.iter().all(|x| *x >= 0 && (*x as usize) < n)
        });
        let Some((cs, cd)) = cells else {
            return Err(err("tetrahedra must contain valid integer node indices"));
        };
        let electrodes = int_array(electrode_nodes).filter(|(s, d)| {
            s.len() == 1
                && !d.is_empty()
                && d.iter().all(|x| *x >= 0 && (*x as usize) < n)
                && d.iter().collect::<BTreeSet<_>>().len() == d.len()
        });
        let Some((_, ed)) = electrodes else {
            return Err(err("explicit distinct electrode node indices required"));
        };
        let cells: Vec<[usize; 4]> =
            cd.chunks(4).map(|c| [c[0] as usize, c[1] as usize, c[2] as usize, c[3] as usize]).collect();
        let sorted: BTreeSet<[usize; 4]> = cells
            .iter()
            .map(|c| {
                let mut s = *c;
                s.sort_unstable();
                s
            })
            .collect();
        if sorted.len() != cs[0] {
            return Err(err("duplicate tetrahedra"));
        }
        let point = |k: usize| [vd[3 * k], vd[3 * k + 1], vd[3 * k + 2]];
        let mut adjacency = vec![BTreeSet::new(); n];
        let mut gradients = Vec::with_capacity(cells.len());
        let mut volumes = Vec::with_capacity(cells.len());
        for row in &cells {
            if row.iter().collect::<BTreeSet<_>>().len() != 4 {
                return Err(err("tetrahedron repeats a node"));
            }
            let p0 = point(row[0]);
            let edges: [[f64; 3]; 3] = std::array::from_fn(|r| {
                let p = point(row[r + 1]);
                std::array::from_fn(|a| p[a] - p0[a])
            });
            let determinant = det3(edges);
            if !determinant.is_finite() || determinant.abs() / 6.0 == 0.0 {
                return Err(err("degenerate tetrahedron"));
            }
            let inverse = inv3(edges).ok_or_else(|| err("degenerate tetrahedron"))?;
            let mut grad = [[0.0; 3]; 4];
            for a in 0..3 {
                grad[0][a] = -(inverse[a][0] + inverse[a][1] + inverse[a][2]);
            }
            for i in 0..3 {
                for a in 0..3 {
                    grad[i + 1][a] = inverse[a][i];
                }
            }
            if !grad.iter().flatten().all(|x| x.is_finite()) {
                return Err(err("nonfinite tetrahedral gradients"));
            }
            gradients.push(grad);
            volumes.push(determinant.abs() / 6.0);
            for node in row {
                adjacency[*node].extend(row.iter().copied());
            }
        }
        let anchors: BTreeSet<usize> = ed.iter().map(|v| *v as usize).collect();
        let mut unseen: BTreeSet<usize> = (0..n).collect();
        while let Some(&start) = unseen.iter().next() {
            let mut pending = vec![start];
            let mut component = BTreeSet::new();
            while let Some(node) = pending.pop() {
                if !component.insert(node) {
                    continue;
                }
                pending.extend(adjacency[node].iter().filter(|m| !component.contains(*m)).copied());
            }
            if component.is_disjoint(&anchors) {
                return Err(err("every electrical connected component needs a potential electrode"));
            }
            if component.iter().any(|node| adjacency[*node].is_empty()) {
                return Err(err("unused electrical mesh node"));
            }
            for node in &component {
                unseen.remove(node);
            }
        }
        Ok(Self {
            n,
            free_nodes: (0..n).filter(|k| !anchors.contains(k)).collect(),
            electrodes: ed.iter().map(|v| *v as usize).collect(),
            cells,
            gradients,
            volumes,
        })
    }


    pub fn assemble(
        &self,
        potential: &[f64],
        conductivity: &[f64],
        electrode_potentials: &[f64],
    ) -> CaeResult<MeshAssembly> {
        if potential.len() != self.n
            || conductivity.len() != self.cells.len()
            || electrode_potentials.len() != self.electrodes.len()
        {
            return Err(err("potential, cell conductivity or electrode voltage shape mismatch"));
        }
        if !potential.iter().chain(conductivity).chain(electrode_potentials).all(|v| v.is_finite()) {
            return Err(err("finite electrical state required"));
        }
        if conductivity.iter().any(|s| *s <= 0.0) {
            return Err(err("positive cell conductivity required"));
        }
        let mut charge = vec![0.0; self.n];
        let mut heat = vec![0.0; self.n];
        let mut power = 0.0;
        for (e, cell) in self.cells.iter().enumerate() {
            let phi: [f64; 4] = std::array::from_fn(|i| potential[cell[i]]);
            let t = electrothermal_element(&phi, conductivity[e], &self.gradients[e], self.volumes[e], None);
            for (i, node) in cell.iter().enumerate() {
                charge[*node] += t.potential_residual[i];
                heat[*node] += t.thermal_load[i];
            }
            power += t.joule_power;
        }
        let dot: f64 = potential.iter().zip(&charge).map(|(a, b)| a * b).sum();
        Ok(MeshAssembly {
            free_charge_residual_a: self.free_nodes.iter().map(|k| charge[*k]).collect(),
            electrode_error_v: self
                .electrodes
                .iter()
                .zip(electrode_potentials)
                .map(|(k, v)| potential[*k] - v)
                .collect(),
            electrode_current_into_domain_a: self.electrodes.iter().map(|k| charge[*k]).collect(),
            joule_power_w: power,
            conductivity_margin: conductivity.iter().copied().fold(f64::INFINITY, f64::min),
            nodal_power_identity_error_w: dot - heat.iter().sum::<f64>(),
            charge_residual_a: charge,
            thermal_load_w: heat,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElectricalSettings {
    pub temperature_reference_k: f64,
    pub temperature_scale_k: f64,
    pub potential_scale_v: f64,
    pub void_conductivity: f64,
    pub penalty: f64,
    pub thermal_residual_scale_w: f64,
    pub current_residual_scale_a: f64,
    pub conductivity: [f64; 2],
    pub slope: [f64; 2],
}

fn positive_scalar(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite() && *x > 0.0),
        _ => None,
    }
}


pub fn validate_settings(s: &Value, bounds: Option<[f64; 2]>) -> CaeResult<ElectricalSettings> {
    let mut scalars = BTreeMap::new();
    for key in [
        "temperature_reference_K",
        "temperature_scale_K",
        "potential_scale_V",
        "void_conductivity_S_m",
        "penalty",
        "thermal_residual_scale_W",
        "current_residual_scale_A",
    ] {
        match s.get(key).and_then(positive_scalar) {
            Some(v) => scalars.insert(key, v),
            None => return Err(err(format!("{key} must be a finite positive real scalar"))),
        };
    }
    if scalars["penalty"] < 1.0 {
        return Err(err("electrical occupancy penalty must be at least one"));
    }
    let mut pairs = BTreeMap::new();
    for key in ["conductivity_S_m", "conductivity_slope_S_m_K"] {
        let pair = s.get(key).and_then(|v| v.as_array()).filter(|a| a.len() == 2).and_then(|a| {
            let v: Vec<f64> =
                a.iter().filter_map(|x| x.as_f64().filter(|f| x.is_number() && f.is_finite())).collect();
            (v.len() == 2).then(|| [v[0], v[1]])
        });
        match pair {
            Some(p) => pairs.insert(key, p),
            None => return Err(err(format!("{key} requires two finite real endmember values"))),
        };
    }
    let (conductivity, slope) = (pairs["conductivity_S_m"], pairs["conductivity_slope_S_m_K"]);
    if conductivity[0].min(conductivity[1]) <= 0.0 {
        return Err(err("positive reference endmember conductivities required"));
    }
    let tref = scalars["temperature_reference_K"];
    if let Some(b) = bounds {
        if !b.iter().all(|v| v.is_finite()) || b[0] <= 0.0 || b[1] <= b[0] {
            return Err(err("finite positive ordered temperature bounds required"));
        }
        for t in b {
            for k in 0..2 {
                let e = conductivity[k] + (t - tref) * slope[k];
                if !e.is_finite() || e <= 0.0 {
                    return Err(err(
                        "electrical conductivity must stay finite and positive across the host temperature range",
                    ));
                }
            }
        }
    }
    Ok(ElectricalSettings {
        temperature_reference_k: tref,
        temperature_scale_k: scalars["temperature_scale_K"],
        potential_scale_v: scalars["potential_scale_V"],
        void_conductivity: scalars["void_conductivity_S_m"],
        penalty: scalars["penalty"],
        thermal_residual_scale_w: scalars["thermal_residual_scale_W"],
        current_residual_scale_a: scalars["current_residual_scale_A"],
        conductivity,
        slope,
    })
}

pub fn effective_conductivity<S: Scalar>(s: &ElectricalSettings, temperatures: &[S; 4], design: &[S]) -> S {
    let mean = (temperatures[0] + temperatures[1] + temperatures[2] + temperatures[3]) / S::from_f64(4.0);
    let dt = mean - S::from_f64(s.temperature_reference_k);
    let e0 = S::from_f64(s.conductivity[0]) + S::from_f64(s.slope[0]) * dt;
    let e1 = S::from_f64(s.conductivity[1]) + S::from_f64(s.slope[1]) * dt;
    let solid = (S::one() - design[4]) * e0 + design[4] * e1;
    let void = S::from_f64(s.void_conductivity);
    void + (solid - void) * design[0].powf(s.penalty)
}

pub fn native_terms<S: Scalar>(
    s: &ElectricalSettings,
    current: &[S],
    design: &[S],
    grad0: &[[f64; 3]; 4],
) -> ElementTerms<S> {
    let temperatures: [S; 4] = std::array::from_fn(|i| {
        S::from_f64(s.temperature_reference_k) + S::from_f64(s.temperature_scale_k) * current[i]
    });
    let phi: [S; 4] = std::array::from_fn(|i| S::from_f64(s.potential_scale_v) * current[4 + i]);
    let h: [S; 3] = std::array::from_fn(|a| design[1 + a] * S::from_f64(1e-3));
    let sigma = effective_conductivity(s, &temperatures, design);
    let gradients: [[S; 3]; 4] =
        std::array::from_fn(|i| std::array::from_fn(|a| S::from_f64(grad0[i][a]) / h[a]));
    let volume = h[0] * h[1] * h[2] / S::from_f64(6.0);
    electrothermal_element(&phi, sigma, &gradients, volume, None)
}

pub struct NativeElectrothermalElement {
    pub settings: ElectricalSettings,
    pub grad0: Arc<Vec<[[f64; 3]; 4]>>,
}

impl LocalResidual for NativeElectrothermalElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let t = native_terms(&self.settings, current, design, &self.grad0[item]);
        let ts = S::from_f64(self.settings.thermal_residual_scale_w);
        let cs = S::from_f64(self.settings.current_residual_scale_a);
        for i in 0..4 {
            out[i] = -t.thermal_load[i] / ts;
            out[4 + i] = t.potential_residual[i] / cs;
        }
    }
}

fn face_nodes(grid: [usize; 3], axis: usize) -> (Vec<usize>, Vec<usize>) {
    let shape = [grid[0] + 1, grid[1] + 1, grid[2] + 1];
    let mut lo = Vec::new();
    let mut hi = Vec::new();
    for i in 0..shape[0] {
        for j in 0..shape[1] {
            for k in 0..shape[2] {
                let id = (i * shape[1] + j) * shape[2] + k;
                let c = [i, j, k][axis];
                if c == 0 {
                    lo.push(id);
                }
                if c == shape[axis] - 1 {
                    hi.push(id);
                }
            }
        }
    }
    (lo, hi)
}


#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn electrode_voltage_history(settings: &Value) -> CaeResult<Vec<Vec<f64>>> {
    let value = &settings["voltage_history_V"];
    let Some(m) = value.as_object() else {
        let (shape, data) = real_array(value, "electrode voltage history")?;
        if shape.len() != 2 {
            return Err(err("electrode voltages require [host time, electrode] values"));
        }
        return Ok(data.chunks(shape[1].max(1)).map(<[f64]>::to_vec).take(shape[0]).collect());
    };
    let axis = m.get("axis").and_then(py_int).filter(|a| (0..3).contains(a));
    let ok = m.len() == 3
        && m.contains_key("values")
        && m.get("layout") == Some(&json!("opposing_face_pair"))
        && axis.is_some();
    let Some(axis) = axis.filter(|_| ok) else {
        return Err(err("compact voltages require opposing_face_pair, axis and [time,min/max face] values"));
    };
    let grid = grid_of(&settings["grid"]).unwrap_or([1, 1, 1]);
    let (lo, hi) = face_nodes(grid, axis as usize);
    let nodes: Vec<i64> = settings["electrode_nodes"]
        .as_array()
        .map(|a| a.iter().filter_map(py_int).collect())
        .unwrap_or_default();
    let expected: Vec<i64> = lo.iter().chain(&hi).map(|v| i64::try_from(*v).unwrap_or(-1)).collect();
    if nodes != expected || settings["electrode_nodes"].as_array().map_or(0, Vec::len) != expected.len() {
        return Err(err("compact face voltages require min-face then max-face electrode nodes"));
    }
    let (shape, data) = real_array(&m["values"], "face voltage history")?;
    let nt = settings["times_s"].as_array().map_or(0, Vec::len);
    if shape != [nt, 2] {
        return Err(err("one min/max voltage pair required per host time"));
    }
    Ok(data
        .chunks(2)
        .map(|pair| {
            std::iter::repeat_n(pair[0], lo.len()).chain(std::iter::repeat_n(pair[1], hi.len())).collect()
        })
        .collect())
}

fn scaled_voltage(voltage: &[Vec<f64>], scale: f64) -> CaeResult<Vec<Vec<f64>>> {
    let scaled: Vec<Vec<f64>> = voltage.iter().map(|r| r.iter().map(|v| v / scale).collect()).collect();
    if !scaled.iter().flatten().all(|v| v.is_finite()) {
        return Err(err(
            "electrode voltages divided by potential_scale_V must remain finite; choose a compatible potential scale",
        ));
    }
    Ok(scaled)
}

const KEYS: [&str; 14] = [
    "grid",
    "times_s",
    "node_order",
    "electrode_nodes",
    "voltage_history_V",
    "conductivity_S_m",
    "conductivity_slope_S_m_K",
    "void_conductivity_S_m",
    "penalty",
    "potential_scale_V",
    "current_residual_scale_A",
    "charge_tolerance_A",
    "power_tolerance_W",
    "energy_convention",
];

const CONTROLS: [&str; 6] = [
    "conductivity_S_m",
    "conductivity_slope_S_m_K",
    "void_conductivity_S_m",
    "penalty",
    "potential_scale_V",
    "current_residual_scale_A",
];

fn to_list(shape: &[usize], data: &[f64]) -> Value {
    if shape.is_empty() {
        return json!(data[0]);
    }
    let inner: usize = shape[1..].iter().product();
    Value::Array((0..shape[0]).map(|i| to_list(&shape[1..], &data[i * inner..(i + 1) * inner])).collect())
}

fn material_bounds(solid: &Value) -> [f64; 2] {
    let materials = solid["materials"].as_array().cloned().unwrap_or_default();
    let lo = materials.iter().filter_map(|m| m["T_min"].as_f64()).fold(f64::NEG_INFINITY, f64::max);
    let hi = materials.iter().filter_map(|m| m["T_max"].as_f64()).fold(f64::INFINITY, f64::min);
    [lo, hi]
}

#[must_use]
pub fn derived_scales(solid: &Value) -> [f64; 3] {
    let n = &solid["numerics"];
    let f = |v: &Value| v.as_f64().unwrap_or(f64::NAN);
    [
        f(&solid["temperature_initial_K"]),
        f(&n["temperature_scale_K"]),
        f(&n["conductivity_scale_W_mK"]) * f(&n["temperature_scale_K"]) * f(&n["length_scale_m"]),
    ]
}


#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn normalise(settings: &Value, context: &Value) -> CaeResult<Value> {
    let ok = settings
        .as_object()
        .is_some_and(|m| m.len() == KEYS.len() && KEYS.iter().all(|k| m.contains_key(*k)));
    let Some(s) = settings.as_object().filter(|_| ok) else {
        return Err(err(
            "resolved electrothermal source requires explicit grid, time, electrodes, conductivity, scales and energy convention",
        ));
    };
    let Some(solid) = context.get("solid").filter(|v| v.is_object()) else {
        return Err(err("native thermal solid context required"));
    };
    let grid = grid_of(&s["grid"]).filter(|_| s["grid"] == solid["grid"]);
    let Some(grid) = grid else { return Err(err("electrode grid must match the native solid grid")) };
    let times = real_array(&s["times_s"], "electrode times")?;
    let expected = real_array(solid.get("times_s").unwrap_or(&Value::Null), "host times")?;
    if !(times.0.len() == 1
        && times.1.len() >= 2
        && times.1.windows(2).all(|w| w[1] > w[0])
        && times == expected)
    {
        return Err(err("electrode voltages require every host time, without interpolation"));
    }
    let count = (grid[0] + 1) * (grid[1] + 1) * (grid[2] + 1);
    let nodes = s["electrode_nodes"].as_array();
    let valid_nodes = nodes.is_some_and(|a| {
        !a.is_empty()
            && a.iter().all(|v| py_int(v).is_some_and(|k| k >= 0 && (k as usize) < count))
            && a.iter().filter_map(py_int).collect::<BTreeSet<_>>().len() == a.len()
    });
    if !valid_nodes {
        return Err(err("distinct in-range native electrode node indices required"));
    }
    let n_electrodes = nodes.map_or(0, Vec::len);
    let voltage = electrode_voltage_history(settings)?;
    if voltage.len() != times.1.len() || voltage.iter().any(|r| r.len() != n_electrodes) {
        return Err(err("electrode voltages require [host time, electrode] values"));
    }
    if s["node_order"] != json!("native_solid_node_order")
        || s["energy_convention"] != json!("authored_heat_excludes_resolved_joule_heat")
    {
        return Err(err("explicit native-node binding and nonduplicated Joule heat convention required"));
    }
    let [tref, tscale, thermal] = derived_scales(solid);
    let mut controls = Map::new();
    for key in CONTROLS {
        let (shape, values) = real_array(&s[key], key)?;
        controls.insert(key.into(), to_list(&shape, &values));
    }
    let mut merged = json!({"temperature_reference_K": tref, "temperature_scale_K": tscale, "thermal_residual_scale_W": thermal});
    for (k, v) in &controls {
        merged[k] = v.clone();
    }
    let checked = validate_settings(&merged, Some(material_bounds(solid)))?;
    scaled_voltage(&voltage, checked.potential_scale_v)?;
    for key in ["charge_tolerance_A", "power_tolerance_W"] {
        let (shape, values) = real_array(&s[key], key)?;
        if !shape.is_empty() || values[0] <= 0.0 {
            return Err(err("positive scalar SI electrical tolerances required"));
        }
        controls.insert(key.into(), json!(values[0]));
    }
    let authored =
        if s["voltage_history_V"].is_object() { s["voltage_history_V"].clone() } else { json!(voltage) };
    let mut out = s.clone();
    for (k, v) in controls {
        out.insert(k, v);
    }
    out.insert("grid".into(), json!(grid));
    out.insert("times_s".into(), json!(times.1));
    out.insert("electrode_nodes".into(), s["electrode_nodes"].clone());
    out.insert("voltage_history_V".into(), authored);
    Ok(Value::Object(out))
}


pub fn native_electrical_history_block(
    solid: &SolidKernel,
    electrodes: &[usize],
    name: &str,
) -> CaeResult<Option<HistoryBlock>> {
    if electrodes.is_empty()
        || electrodes.iter().any(|e| *e >= solid.nn)
        || electrodes.iter().collect::<BTreeSet<_>>().len() != electrodes.len()
    {
        return Err(err("distinct native electrode nodes required"));
    }
    let size = solid.nn - electrodes.len();
    if size == 0 {
        return Ok(None);
    }
    let width = 2 * solid.nc + 3;
    Ok(Some(HistoryBlock {
        name: name.into(),
        initial: vec![0.0; size],
        design_indices: (0..width).collect(),
        callbacks: Arc::new(ZeroBlock { size, width }),
        field: None,
    }))
}

pub struct ZeroBlock {
    pub size: usize,
    pub width: usize,
}

impl HistoryBlockCallbacks for ZeroBlock {
    fn residual(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(vec![0.0; self.size])
    }
    fn current_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(zero_csr(self.size, self.size)))
    }
    fn previous_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(zero_csr(self.size, self.size)))
    }
    fn design_jacobian(&self, _n: usize, _z: &[f64], _old: &[f64], _x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(zero_csr(self.size, self.width)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Quantities {
    pub potential_v: Vec<f64>,
    pub charge_residual_a: Vec<f64>,
    pub thermal_load_w: Vec<f64>,
    pub thermal_load_on_free_temperature_nodes_w: f64,
    pub thermal_load_on_prescribed_temperature_nodes_w: f64,
    pub electrode_current_into_domain_a: Vec<f64>,
    pub terminal_power_w: f64,
    pub joule_power_w: f64,
    pub free_charge_power_defect_w: f64,
    pub power_identity_error_w: f64,
    pub thermal_partition_error_w: f64,
    pub conductivity_margin_s_m: f64,
}

pub struct NativeElectrothermalInterface {
    solid: Arc<SolidKernel>,
    pub settings: ElectricalSettings,
    electrodes: Vec<usize>,
    free_nodes: Vec<usize>,
    electrical: (usize, usize),
    fixed: Vec<Vec<f64>>,
    fixed_thermal: Vec<Vec<[f64; 4]>>,
    current_incidence: Vec<i64>,
    design: Vec<i64>,
    local: LocalResidualAssembly<NativeElectrothermalElement>,
}

impl NativeElectrothermalInterface {

    pub fn new(
        solid: Arc<SolidKernel>,
        assembly: &CoupledHistoryAssembly,
        settings: ElectricalSettings,
        electrodes: &[usize],
        voltage: &[Vec<f64>],
        block_name: &str,
    ) -> CaeResult<Self> {
        let s = &solid;
        let settings = revalidate(&settings, [s.t_min, s.t_max])?;
        if electrodes.is_empty()
            || electrodes.iter().any(|e| *e >= s.nn)
            || electrodes.iter().collect::<BTreeSet<_>>().len() != electrodes.len()
        {
            return Err(err("distinct native electrode nodes required"));
        }
        if voltage.len() != s.times.len()
            || voltage.iter().any(|r| r.len() != electrodes.len() || r.iter().any(|v| !v.is_finite()))
        {
            return Err(err("finite electrode voltages required at each host time"));
        }
        let anchors: BTreeSet<usize> = electrodes.iter().copied().collect();
        let free_nodes: Vec<usize> = (0..s.nn).filter(|k| !anchors.contains(k)).collect();
        if free_nodes.is_empty() && assembly.slice(block_name).is_some() {
            return Err(err("fully prescribed potential requires no electrical history block"));
        }
        let electrical = if free_nodes.is_empty() {
            (assembly.state_size(), assembly.state_size())
        } else {
            assembly
                .slice(block_name)
                .ok_or_else(|| err(format!("coupled history assembly has no {block_name} block")))?
        };
        let thermal_start = block_start(assembly, "solid")?;
        if electrical.1 - electrical.0 != free_nodes.len() {
            return Err(err("electrical block must match free potential nodes"));
        }
        let m = &s.model;
        if settings.temperature_reference_k != m.t0
            || settings.temperature_scale_k != m.ts
            || settings.thermal_residual_scale_w != m.ks * m.ts * m.ls
        {
            return Err(err("electrothermal scaling must match native thermal host"));
        }
        let scaled = scaled_voltage(voltage, settings.potential_scale_v)?;
        let fixed: Vec<Vec<f64>> = scaled
            .iter()
            .map(|row| {
                let mut f = vec![0.0; s.nn];
                for (k, e) in electrodes.iter().enumerate() {
                    f[*e] = row[k];
                }
                f
            })
            .collect();
        let fixed_thermal: Vec<Vec<[f64; 4]>> = s
            .fixed_t
            .iter()
            .map(|row| {
                s.mesh.tets.iter().map(|t| std::array::from_fn(|i| (row[t[i]] - m.t0) / m.ts)).collect()
            })
            .collect();
        let mut potential_map = vec![-1_i64; s.nn];
        for (k, node) in free_nodes.iter().enumerate() {
            potential_map[*node] = i64::try_from(electrical.0 + k).unwrap_or(-1);
        }
        let off = i64::try_from(thermal_start).unwrap_or(0);
        let current_incidence: Vec<i64> = s
            .mesh
            .tets
            .iter()
            .flat_map(|t| {
                let thermal: Vec<i64> =
                    t.iter().map(|n| if s.tmap[*n] < 0 { -1 } else { s.tmap[*n] + off }).collect();
                thermal.into_iter().chain(t.iter().map(|n| potential_map[*n])).collect::<Vec<_>>()
            })
            .collect();
        let design = design_incidence(s);
        let kernel = NativeElectrothermalElement { settings, grad0: Arc::new(s.mesh.gradients.clone()) };
        let local = LocalResidualAssembly::new(
            kernel,
            incidence(s.ne, 8, current_incidence.clone())?,
            incidence(s.ne, 8, current_incidence.clone())?,
            incidence(s.ne, 8, current_incidence.clone())?,
            incidence(s.ne, 5, design.clone())?,
            assembly.state_size(),
            assembly.design_size(),
            assembly_options(s),
        )?;
        Ok(Self {
            electrodes: electrodes.to_vec(),
            free_nodes,
            electrical,
            fixed,
            fixed_thermal,
            current_incidence,
            design,
            local,
            settings,
            solid,
        })
    }

    fn data(&self, n: usize) -> Vec<f64> {
        self.solid
            .mesh
            .tets
            .iter()
            .enumerate()
            .flat_map(|(e, t)| {
                let thermal = self.fixed_thermal[n][e];
                thermal.into_iter().chain(t.iter().map(|node| self.fixed[n][*node])).collect::<Vec<_>>()
            })
            .collect()
    }

    fn local_current(&self, n: usize, z: &[f64]) -> Vec<f64> {
        let fixed = self.data(n);
        self.current_incidence
            .iter()
            .zip(&fixed)
            .map(|(i, f)| usize::try_from(*i).map_or(*f, |k| z[k]))
            .collect()
    }

    fn local_design(&self, x: &[f64], e: usize) -> [f64; 5] {
        std::array::from_fn(|k| x[usize::try_from(self.design[5 * e + k]).unwrap_or(0)])
    }

    #[must_use]
    pub fn quantities(&self, n: usize, z: &[f64], x: &[f64]) -> Quantities {
        let s = &self.solid;
        let current = self.local_current(n, z);
        let mut charge = vec![0.0; s.nn];
        let mut heat = vec![0.0; s.nn];
        let mut joule = 0.0;
        let mut margin = f64::INFINITY;
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let d = self.local_design(x, e);
            let t = native_terms(&self.settings, &current[8 * e..8 * e + 8], &d, &s.mesh.gradients[e]);
            for (i, node) in tet.iter().enumerate() {
                charge[*node] += t.potential_residual[i];
                heat[*node] += t.thermal_load[i];
            }
            joule += t.joule_power;
            margin = margin.min(t.conductivity);
        }
        let mut potential = self.fixed[n].clone();
        for (k, node) in self.free_nodes.iter().enumerate() {
            potential[*node] = z[self.electrical.0 + k];
        }
        for p in &mut potential {
            *p *= self.settings.potential_scale_v;
        }
        let terminal: f64 = self.electrodes.iter().map(|k| potential[*k] * charge[*k]).sum();
        let free_power: f64 = self.free_nodes.iter().map(|k| potential[*k] * charge[*k]).sum();
        let total_heat: f64 = heat.iter().sum();
        let free_heat: f64 = s.free_t.iter().map(|k| heat[*k]).sum();
        Quantities {
            thermal_load_on_free_temperature_nodes_w: free_heat,
            thermal_load_on_prescribed_temperature_nodes_w: total_heat - free_heat,
            electrode_current_into_domain_a: self.electrodes.iter().map(|k| charge[*k]).collect(),
            terminal_power_w: terminal,
            joule_power_w: joule,
            free_charge_power_defect_w: free_power,
            power_identity_error_w: terminal + free_power - joule,
            thermal_partition_error_w: total_heat - joule,
            conductivity_margin_s_m: margin,
            potential_v: potential,
            charge_residual_a: charge,
            thermal_load_w: heat,
        }
    }

    #[must_use]
    pub fn power_vjp(&self, n: usize, z: &[f64], x: &[f64], w: [f64; 2]) -> (Vec<f64>, Vec<f64>) {
        let s = &self.solid;
        let current = self.local_current(n, z);
        let mut electrode_weight = vec![0.0; s.nn];
        for k in &self.electrodes {
            electrode_weight[*k] = self.fixed[n][*k] * self.settings.potential_scale_v;
        }
        let mut zb = vec![0.0; z.len()];
        let mut xb = vec![0.0; x.len()];
        for (e, tet) in s.mesh.tets.iter().enumerate() {
            let d = self.local_design(x, e);
            let vars: Vec<Dual<13>> = current[8 * e..8 * e + 8]
                .iter()
                .chain(d.iter())
                .enumerate()
                .map(|(k, v)| Dual::variable(*v, k))
                .collect();
            let t = native_terms(&self.settings, &vars[..8], &vars[8..], &s.mesh.gradients[e]);
            let mut objective = t.joule_power * Dual::from_f64(w[0]);
            for (i, node) in tet.iter().enumerate() {
                if electrode_weight[*node] != 0.0 {
                    objective =
                        objective + t.potential_residual[i] * Dual::from_f64(w[1] * electrode_weight[*node]);
                }
            }
            for k in 0..8 {
                if let Ok(idx) = usize::try_from(self.current_incidence[8 * e + k]) {
                    zb[idx] += objective.eps[k];
                }
            }
            for k in 0..5 {
                xb[usize::try_from(self.design[5 * e + k]).unwrap_or(0)] += objective.eps[8 + k];
            }
        }
        (zb, xb)
    }


    pub fn validate_state(
        &self,
        n: usize,
        z: &[f64],
        x: &[f64],
        charge_tolerance: f64,
        power_tolerance: f64,
    ) -> CaeResult<(Quantities, f64)> {
        let s = &self.solid;
        if !(1..s.times.len()).contains(&n) {
            return Err(err("electrothermal acceptance requires a valid noninitial host time index"));
        }
        for (name, value) in
            [("charge_tolerance_A", charge_tolerance), ("power_tolerance_W", power_tolerance)]
        {
            if !value.is_finite() || value <= 0.0 {
                return Err(err(format!("{name} must be finite and positive")));
            }
        }
        if z.len() != self.local.state_size()
            || x.len() != self.local.design_size()
            || !z.iter().chain(x).all(|v| v.is_finite())
        {
            return Err(err("finite native electrothermal state and design required"));
        }
        for e in 0..s.ne {
            let d = self.local_design(x, e);
            if d[0] < 0.0 || d[0] > 1.0 || d[4] < 0.0 || d[4] > 1.0 || d[1..4].iter().any(|h| *h <= 0.0) {
                return Err(CaeError::convergence(
                    "electrothermal design outside occupancy/mixture/positive-spacing domain",
                ));
            }
        }
        let current = self.local_current(n, z);
        for e in 0..s.ne {
            for i in 0..4 {
                let t = self.settings.temperature_reference_k
                    + self.settings.temperature_scale_k * current[8 * e + i];
                if !t.is_finite() || t < s.t_min || t > s.t_max {
                    return Err(CaeError::convergence(
                        "electrothermal state outside host temperature interval",
                    ));
                }
            }
        }
        let q = self.quantities(n, z, x);
        let scalars = [
            q.thermal_load_on_free_temperature_nodes_w,
            q.thermal_load_on_prescribed_temperature_nodes_w,
            q.terminal_power_w,
            q.joule_power_w,
            q.free_charge_power_defect_w,
            q.power_identity_error_w,
            q.thermal_partition_error_w,
            q.conductivity_margin_s_m,
        ];
        let arrays = q
            .potential_v
            .iter()
            .chain(&q.charge_residual_a)
            .chain(&q.thermal_load_w)
            .chain(&q.electrode_current_into_domain_a);
        if !scalars.iter().chain(arrays).all(|v| v.is_finite()) || q.conductivity_margin_s_m <= 0.0 {
            return Err(CaeError::convergence("nonfinite or nonpositive electrothermal ledger"));
        }
        let free_error =
            self.free_nodes.iter().map(|k| q.charge_residual_a[*k].abs()).fold(0.0_f64, f64::max);
        let net: f64 = q.electrode_current_into_domain_a.iter().sum();
        if free_error > charge_tolerance || net.abs() > charge_tolerance {
            return Err(CaeError::convergence("electrical charge balance has not converged"));
        }
        let errors =
            [q.power_identity_error_w, q.thermal_partition_error_w, q.terminal_power_w - q.joule_power_w];
        if errors.iter().any(|v| v.abs() > power_tolerance) {
            return Err(CaeError::convergence(
                "electrical terminal and thermal power balance has not converged",
            ));
        }
        Ok((q, free_error))
    }
}

fn revalidate(s: &ElectricalSettings, bounds: [f64; 2]) -> CaeResult<ElectricalSettings> {
    validate_settings(&settings_value(s), Some(bounds))
}

fn settings_value(s: &ElectricalSettings) -> Value {
    json!({"temperature_reference_K": s.temperature_reference_k, "temperature_scale_K": s.temperature_scale_k,
        "potential_scale_V": s.potential_scale_v, "void_conductivity_S_m": s.void_conductivity, "penalty": s.penalty,
        "thermal_residual_scale_W": s.thermal_residual_scale_w, "current_residual_scale_A": s.current_residual_scale_a,
        "conductivity_S_m": s.conductivity, "conductivity_slope_S_m_K": s.slope})
}

impl HistoryInterface for NativeElectrothermalInterface {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.local.residual(z, old, x, &self.data(n), &self.data(n - 1))
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.local.jacobian(kind, z, old, x, &self.data(n), &self.data(n - 1))?))
    }
}

pub struct BoundNativeElectrothermal {
    solid: Arc<SolidKernel>,
    settings: ElectricalSettings,
    electrodes: Vec<usize>,
    voltage: Vec<Vec<f64>>,
    charge_tolerance: f64,
    power_tolerance: f64,
    block: Option<HistoryBlock>,
    exchange: OnceLock<Arc<NativeElectrothermalInterface>>,
}

impl BoundNativeElectrothermal {

    pub fn new(
        settings: &ElectricalSettings,
        host: &dyn FieldHost,
        electrodes: Vec<usize>,
        voltage: Vec<Vec<f64>>,
        charge_tolerance: f64,
        power_tolerance: f64,
    ) -> CaeResult<Self> {
        let solid = Arc::clone(host.solid());
        let settings = revalidate(settings, [solid.t_min, solid.t_max])?;
        for v in [charge_tolerance, power_tolerance] {
            if !v.is_finite() || v <= 0.0 {
                return Err(err("finite positive SI electrical acceptance tolerances required"));
            }
        }
        let block = native_electrical_history_block(&solid, &electrodes, BLOCK)?;
        Ok(Self {
            solid,
            settings,
            electrodes,
            voltage,
            charge_tolerance,
            power_tolerance,
            block,
            exchange: OnceLock::new(),
        })
    }

    fn exchange_ref(&self) -> CaeResult<&Arc<NativeElectrothermalInterface>> {
        self.exchange.get().ok_or_else(|| err("resolved electrothermal source is not attached"))
    }
}

impl BoundFieldSource for BoundNativeElectrothermal {
    fn response_units(&self) -> Vec<(String, String)> {
        vec![
            ("resolved_joule_power_W".into(), "W".into()),
            ("electrical_terminal_power_W".into(), "W".into()),
        ]
    }

    fn state_contract(&self) -> Option<&'static str> {
        self.block.is_none().then_some(STATE_CONTRACT)
    }

    fn blocks(&self) -> Vec<HistoryBlock> {
        self.block.iter().cloned().collect()
    }

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()> {
        let exchange = Arc::new(NativeElectrothermalInterface::new(
            Arc::clone(&self.solid),
            assembly,
            self.settings,
            &self.electrodes,
            &self.voltage,
            BLOCK,
        )?);
        assembly.add_interface(Arc::clone(&exchange) as Arc<dyn HistoryInterface>)?;
        self.exchange.set(exchange).map_err(|_| err("resolved electrothermal source attached twice"))
    }

    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>> {
        self.exchange.get().map(|e| Arc::clone(e) as Arc<dyn HistoryInterface>)
    }

    fn energy(&self, n: usize, z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy> {
        let p = self.exchange_ref()?.quantities(n, z, x).joule_power_w;
        Ok(SourceEnergy { deposition_w: p, sensible_deposition_w: p, material_production_w: 0.0 })
    }

    fn diagnostics(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>> {
        let (q, free_error) =
            self.exchange_ref()?.validate_state(n, z, x, self.charge_tolerance, self.power_tolerance)?;
        let mut m = Map::new();
        for (k, v) in [
            ("terminal_power_W", q.terminal_power_w),
            ("joule_power_W", q.joule_power_w),
            ("free_charge_power_defect_W", q.free_charge_power_defect_w),
            ("power_identity_error_W", q.power_identity_error_w),
            ("thermal_partition_error_W", q.thermal_partition_error_w),
            ("conductivity_margin_S_m", q.conductivity_margin_s_m),
            ("maximum_free_charge_residual_A", free_error),
            ("thermal_load_on_free_temperature_nodes_W", q.thermal_load_on_free_temperature_nodes_w),
            (
                "thermal_load_on_prescribed_temperature_nodes_W",
                q.thermal_load_on_prescribed_temperature_nodes_w,
            ),
        ] {
            m.insert(k.into(), json!(v));
        }
        m.extend(self.energy(n, z, x)?.to_map());
        m.insert("scope".into(), json!("quasistatic_isotropic_electrothermal"));
        m.insert("sampling".into(), json!("endpoint"));
        m.insert("experimental_qualification_verified".into(), json!(false));
        Ok(m)
    }

    fn validate(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<()> {
        if history.len() != self.solid.times.len() {
            return Err(err("complete electrothermal host history required"));
        }
        for n in 1..history.len() {
            self.diagnostics(n, &history[n], &history[n - 1], x)?;
        }
        Ok(())
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        let n = history.len() - 1;
        let q = self.exchange_ref()?.quantities(n, &history[n], x);
        Ok(vec![q.joule_power_w, q.terminal_power_w])
    }

    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp> {
        let n = history.len() - 1;
        let w = [weights.first().copied().unwrap_or(0.0), weights.get(1).copied().unwrap_or(0.0)];
        let (zb, xb) = self.exchange_ref()?.power_vjp(n, &history[n], x, w);
        let mut states: Vec<Vec<f64>> = history.iter().map(|z| vec![0.0; z.len()]).collect();
        states[n] = zb;
        Ok((states, xb))
    }

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)> {
        let e = self.exchange_ref()?;
        let q: Vec<Quantities> = history.iter().enumerate().map(|(n, z)| e.quantities(n, z, x)).collect();
        let nn = self.solid.nn;
        let times: Vec<f64> = self.solid.times[..history.len()].to_vec();
        let mut values = BTreeMap::new();
        let mut meta = Map::new();
        for (key, unit) in [("potential_V", "V"), ("charge_residual_A", "A"), ("thermal_load_W", "W")] {
            let name = format!("electrothermal_{key}_history");
            let rows: Vec<f64> = q
                .iter()
                .flat_map(|item| match key {
                    "potential_V" => item.potential_v.clone(),
                    "charge_residual_A" => item.charge_residual_a.clone(),
                    _ => item.thermal_load_w.clone(),
                })
                .collect();
            values.insert(name.clone(), FieldValue::Array(array(rows, &[history.len(), nn])?));
            meta.insert(name, json!({"units": unit, "association": "node_history", "axes": ["time", "node"], "rank": "scalar",
                "source": NAME, "times_s": times, "node_order": "native_solid_node_order",
                "initial_state": "prescribed initialization; electrical equilibrium not asserted at time zero"}));
        }
        Ok((values, meta))
    }
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    let Some(s) = settings.as_object() else { return json!({}) };
    let times: Vec<Value> = s.get("times_s").and_then(Value::as_array).cloned().unwrap_or_default();
    let nodes: Vec<Value> = s.get("electrode_nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    let history = |columns: &[Value]| -> Value {
        let rows: Vec<Value> = times
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let title = match t.as_f64().filter(|v| t.is_number() && f64::is_finite(*v)) {
                    Some(v) => format!("Time {} s", implexity_core::extensions::format_g6(v)),
                    None => format!("Time node {i}"),
                };
                json!({"title": title, "type": "array", "minItems": columns.len(), "maxItems": columns.len(), "prefixItems": columns})
            })
            .collect();
        json!({"type": "array", "minItems": times.len(), "maxItems": times.len(), "prefixItems": rows})
    };
    let electrode_columns: Vec<Value> = nodes
        .iter()
        .map(|n| json!({"title": format!("Electrode node {}", implexity_core::pyobj::py_str(n)), "units": "V", "type": "number"}))
        .collect();
    let mut voltage = json!({"title": "Electrode voltage history", "units": "V"});
    if let (Some(v), Value::Object(h)) = (voltage.as_object_mut(), history(&electrode_columns)) {
        v.extend(h);
        v.insert(
            "description".into(),
            json!("One row per host time and one column per electrode. No interpolation."),
        );
    }
    let mut props = Map::new();
    props.insert(
        "electrode_nodes".into(),
        json!({"title": "Electrode node indices", "type": "array", "items": {"type": "integer", "minimum": 0},
        "description": "Native solid node order; one voltage column per listed node."}),
    );
    props.insert("voltage_history_V".into(), voltage);
    props.insert(
        "conductivity_S_m".into(),
        json!({"title": "Endmember conductivities at initial temperature", "units": "S/m", "type": "array",
        "minItems": 2, "maxItems": 2, "items": {"type": "number", "exclusiveMinimum": 0}}),
    );
    props.insert(
        "conductivity_slope_S_m_K".into(),
        json!({"title": "Endmember conductivity temperature slopes", "units": "S/(m K)", "type": "array",
        "minItems": 2, "maxItems": 2, "items": {"type": "number"}}),
    );
    for (key, title, unit) in [
        ("void_conductivity_S_m", "Void regularization conductivity", "S/m"),
        ("potential_scale_V", "Potential scale", "V"),
        ("current_residual_scale_A", "Charge residual scale", "A"),
        ("charge_tolerance_A", "Charge balance tolerance", "A"),
        ("power_tolerance_W", "Power balance tolerance", "W"),
    ] {
        props.insert(
            key.into(),
            json!({"title": title, "units": unit, "type": "number", "exclusiveMinimum": 0}),
        );
    }
    props.insert(
        "penalty".into(),
        json!({"title": "Conductivity occupancy exponent", "type": "number", "minimum": 1}),
    );
    if let Some(v) = s.get("voltage_history_V").filter(|v| v.is_object()) {
        let face = [
            json!({"title": "Minimum face", "units": "V", "type": "number"}),
            json!({"title": "Maximum face", "units": "V", "type": "number"}),
        ];
        let mut values = json!({"title": "Voltage at each host time"});
        if let (Some(o), Value::Object(h)) = (values.as_object_mut(), history(&face)) {
            o.extend(h);
        }
        props.insert("voltage_history_V".into(), json!({"title": "Opposing-face electrode voltages", "properties": {
            "layout": {"enum": ["opposing_face_pair"]},
            "axis": {"title": "Electrode face axis", "enum": [v["axis"]],
                "description": "Bound to the authored face nodes. Use a matching setup template to choose a different face pair."},
            "values": values}}));
    }
    json!({"properties": props})
}

#[must_use]
pub fn study_templates(context: &Value) -> Vec<Value> {
    let Some(ctx) = context.as_object() else { return Vec::new() };
    let solid = ctx.get("solid").cloned().unwrap_or(json!({}));
    if !solid.is_object() {
        return Vec::new();
    }
    let (grid_v, times) = (solid["grid"].clone(), solid["times_s"].clone());
    let existing = match ctx.get("field_sources") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Vec::new(),
    };
    let Some(grid) = grid_of(&grid_v) else { return Vec::new() };
    if times.as_array().is_none_or(|t| t.len() < 2) {
        return Vec::new();
    }
    if existing.iter().any(|r| {
        !r.is_object()
            || r.get("component") == Some(&json!(NAME))
            || r.get("component") == Some(&json!("native_electromagnetic_loads"))
    }) {
        return Vec::new();
    }
    let nt = times.as_array().map_or(0, Vec::len);
    let d = data();
    let text = &d["study_template_text"];
    let mut out = Vec::new();
    for axis in 0..3 {
        let (lo, hi) = face_nodes(grid, axis);
        let settings = json!({"grid": grid_v, "times_s": times, "node_order": "native_solid_node_order",
            "electrode_nodes": lo.iter().chain(&hi).collect::<Vec<_>>(),
            "voltage_history_V": {"layout": "opposing_face_pair", "axis": axis, "values": vec![[0.0, 0.0]; nt]},
            "conductivity_S_m": [1.0, 1.0], "conductivity_slope_S_m_K": [0.0, 0.0], "void_conductivity_S_m": 1e-6, "penalty": 3.0,
            "potential_scale_V": 1.0, "current_residual_scale_A": 1.0, "charge_tolerance_A": 1e-8, "power_tolerance_W": 1e-8,
            "energy_convention": "authored_heat_excludes_resolved_joule_heat"});
        let Ok(settings) = normalise(&settings, context) else { return Vec::new() };
        let t = &text[axis];
        let mut prefix: Vec<Value> = existing.iter().map(|_| json!({})).collect();
        prefix.push(json!({"title": d["editor_label"], "properties": {"component": {"enum": [NAME]}, "settings": editor_schema(&settings, context)}}));
        let mut patch = existing.clone();
        patch.push(json!({"component": NAME, "settings": settings}));
        out.push(json!({"schema": "implexity-provider-study-template/1", "id": t["id"], "label": t["label"],
            "description": t["description"], "truth_status": t["truth_status"],
            "problem_requirements": [{"path": ["solid", "grid"], "value": grid_v}, {"path": ["solid", "times_s"], "value": times},
                {"path": ["field_sources"], "value": ctx.get("field_sources").cloned().unwrap_or(Value::Null), "missing_equals_null": true}],
            "problem_patch": {"field_sources": patch},
            "editor_schema_patch": {"properties": {"field_sources": {"type": "array", "prefixItems": prefix}}}}));
    }
    out
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NativeElectrothermalSource;

impl HistorySourceComponent for NativeElectrothermalSource {
    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value> {
        let context =
            context.downcast_ref::<Value>().ok_or_else(|| err("native thermal solid context required"))?;
        normalise(settings, context)
    }

    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>> {
        let host = host_of(host)?;
        let p = normalise(settings, host.problem())?;
        let s = host.solid();
        let m = &s.model;
        let mut electrical = json!({"temperature_reference_K": m.t0, "temperature_scale_K": m.ts, "thermal_residual_scale_W": m.ks * m.ts * m.ls});
        for key in CONTROLS {
            electrical[key] = p[key].clone();
        }
        let settings = validate_settings(&electrical, None)?;
        let electrodes: Vec<usize> = p["electrode_nodes"]
            .as_array()
            .map(|a| a.iter().filter_map(py_int).map(|v| v as usize).collect())
            .unwrap_or_default();
        let voltage = electrode_voltage_history(&p)?;
        let bound = BoundNativeElectrothermal::new(
            &settings,
            host.as_ref(),
            electrodes,
            voltage,
            p["charge_tolerance_A"].as_f64().unwrap_or(0.0),
            p["power_tolerance_W"].as_f64().unwrap_or(0.0),
        )?;
        Ok(Box::new(BoundSource(Arc::new(bound))))
    }

    fn coupling(&self, base: Value, _settings: &Value) -> CaeResult<Value> {
        extend_coupling(
            &base,
            &["electrical_potential"],
            &[
                (
                    "electrical_potential",
                    "thermal",
                    "resolved_joule_heat",
                    "monolithic",
                    "charge and Joule heat share the native field assembly",
                ),
                (
                    "thermal",
                    "electrical_potential",
                    "temperature_dependent_conductivity",
                    "monolithic",
                    "current temperature determines electrical conductivity",
                ),
            ],
            &[["electrical_potential", "thermal"]],
            &limitations(),
        )
    }

    fn owns_material_forcing(&self, _settings: &Value) -> bool {
        false
    }
}

impl FieldSourceAuthoring for NativeElectrothermalSource {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value {
        editor_schema(settings, context)
    }

    fn study_templates(&self, context: &Value) -> Vec<Value> {
        study_templates(context)
    }
}

#[must_use]
pub fn same_times(a: &crate::common::RealArray, b: &crate::common::RealArray) -> bool {
    times_match(a, b)
}
