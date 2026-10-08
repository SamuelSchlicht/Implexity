// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use implexity_core::contracts::{
    Evaluation, FieldValue,
};
use implexity_linalg::dense::DenseMatrix;
use implexity_physics_solid::structural_dynamics::{
    LinearAssembly, assemble_linear_tetrahedra, checked_linear_history, conforming_interface_map,
    recover_linear_stress_history,
};
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};
use crate::errors::{PResult, ModelError};
use crate::euler3d::{Problem, WallLayout, normalize, wall_loads};
use crate::euler3d_ad::{checked_fixed_history, exact_int, history_bytes, schedule_matches};
use crate::pyval::strs;
use crate::quasi1d_euler::{array, scalar};
pub const LIMITS: [&str; 5] = [
    "Evaluation only: fixed voxel slip walls, conforming tetrahedral solid mesh, one-way numerical pressure loads.",
    "Small-strain isotropic linear elasticity with fixed supports; optional calibrated rainflow usage, no thermal strain or damage/deformation feedback.",
    "Forces are held constant per CFD interval: step-start for first_order, RK stage-average for muscl_ssprk2. Structural substeps do not improve CFD pressure-wave resolution.",
    "All exported wall nodes must match solid nodes. Outer reflecting walls are included; use transmissive boundaries where no physical wall exists.",
    "No topology sensitivities for this composed study. Material calibration and spatial/time convergence remain user responsibilities.",
];

type FlowLoads = (Map<String, Value>, Vec<Vec<[f64; 3]>>, Vec<Vec<[f64; 3]>>);

pub const NAME: &str = "cartesian_euler3d_structure";

pub const RESPONSE_UNITS: [(&str, &str); 3] =
    [("peak_displacement_m", "m"), ("peak_von_mises_Pa", "Pa"), ("peak_strain_norm", "1")];

fn contract(message: &str) -> ModelError {
    ModelError::contract(message)
}

#[derive(Debug, Clone)]
pub struct Prepared {
    pub flow: Problem,
    pub step_s: f64,
    pub step_count: usize,
    pub solid_substeps: usize,
    pub flow_scheme: String,
    pub flow_history_byte_budget: i64,
    pub solid_history_byte_budget: i64,
    pub nodes: Vec<[f64; 3]>,
    pub tets: Vec<[usize; 4]>,
    pub young: Vec<f64>,
    pub poisson: Vec<f64>,
    pub assembly: LinearAssembly,
    pub free: Vec<usize>,
    pub mass: DenseMatrix,
    pub stiffness: DenseMatrix,
    pub u0: Vec<f64>,
    pub v0: Vec<f64>,
    pub layout: WallLayout,
    pub maximum_strain: f64,
}

fn keys_equal(value: &Value, keys: &[&str], optional: &[&str]) -> bool {
    value.as_object().is_some_and(|m| {
        keys.iter().all(|k| m.contains_key(*k))
            && m.keys().all(|k| keys.contains(&k.as_str()) || optional.contains(&k.as_str()))
    })
}

fn int_rows(value: &Value) -> Option<Vec<[i64; 4]>> {
    value
        .as_array()?
        .iter()
        .map(|row| {
            let r = row.as_array()?;
            if r.len() != 4 {
                return None;
            }
            let v: Option<Vec<i64>> = r.iter().map(exact_int).collect();
            v.map(|v| [v[0], v[1], v[2], v[3]])
        })
        .collect()
}

fn real_vector(value: &Value, n: usize) -> Option<Vec<f64>> {
    let a = value.as_array()?;
    if a.len() != n {
        return None;
    }
    a.iter().map(|v| if v.is_boolean() { None } else { v.as_f64() }).collect()
}

fn submatrix(full: &[f64], n: usize, free: &[usize]) -> DenseMatrix {
    let m = free.len();
    let mut data = Vec::with_capacity(m * m);
    for i in free {
        for j in free {
            data.push(full[i * n + j]);
        }
    }
    DenseMatrix { nrows: m, ncols: m, data }
}


#[allow(clippy::too_many_lines)]
pub fn prepare(problem: &Value) -> PResult<Prepared> {
    if !keys_equal(problem, &["flow", "provenance", "solid", "time_integration"], &["fatigue"]) {
        return Err(contract("Required: flow, time_integration, solid, provenance"));
    }
    if problem["provenance"].as_str().is_none_or(|s| s.trim().is_empty()) {
        return Err(contract("Study provenance is required"));
    }
    if let Some(f) = problem.get("fatigue") {
        implexity_physics_solid::fatigue::validate_rainflow_settings(f)?;
    }
    let p = normalize(&problem["flow"])?;
    let time = &problem["time_integration"];
    let s = &problem["solid"];
    if !keys_equal(
        time,
        &["flow_history_byte_budget", "solid_history_byte_budget", "solid_substeps", "step_count", "step_s"],
        &["flow_scheme"],
    ) {
        return Err(contract(
            "Explicit fixed flow schedule, both history byte budgets and solid_substeps are required",
        ));
    }
    let scheme = match time.get("flow_scheme") {
        None => "first_order".to_string(),
        Some(Value::String(v)) if v == "first_order" || v == "muscl_ssprk2" => v.clone(),
        Some(_) => return Err(contract("flow_scheme must be first_order or muscl_ssprk2")),
    };
    let dt = scalar(&time["step_s"], "step_s")?;
    let count = exact_int(&time["step_count"]).filter(|c| (1..=p.max_steps).contains(c));
    let sub = exact_int(&time["solid_substeps"]).filter(|c| (1..=100).contains(c));
    let (Some(count), Some(sub)) = (count, sub) else {
        return Err(contract(
            "Positive step_s, integer step_count within max_steps and solid_substeps 1..100 required",
        ));
    };
    if dt <= 0.0 {
        return Err(contract(
            "Positive step_s, integer step_count within max_steps and solid_substeps 1..100 required",
        ));
    }
    #[allow(clippy::cast_precision_loss)]
    if !schedule_matches(dt * count as f64, p.end_time) {
        return Err(contract("step_s times step_count must equal flow.end_time_s"));
    }
    let mut budgets = [0_i64; 2];
    for (slot, key) in ["flow_history_byte_budget", "solid_history_byte_budget"].iter().enumerate() {
        match exact_int(&time[*key]) {
            Some(b) if b > 0 => budgets[slot] = b,
            _ => return Err(contract("History budgets must be positive integer bytes")),
        }
    }
    if !keys_equal(
        s,
        &[
            "density_kg_m3",
            "fixed_dofs",
            "initial_displacement_m",
            "initial_velocity_m_s",
            "maximum_strain",
            "nodes_m",
            "poisson",
            "tetrahedra",
            "young_Pa",
        ],
        &[],
    ) {
        return Err(contract(
            "Solid requires explicit mesh, cell material arrays, fixed DOFs, initial states and maximum_strain",
        ));
    }
    let nodes_field = array(&s["nodes_m"], "nodes_m", None)?;
    if nodes_field.shape.len() != 2 || nodes_field.shape[1] != 3 || !(4..=256).contains(&nodes_field.shape[0])
    {
        return Err(contract("nodes_m requires 4..256 XYZ nodes for this dense transient route"));
    }
    let nodes: Vec<[f64; 3]> = nodes_field.values.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
    let fixed = crate::array::bool_mask(&s["fixed_dofs"])
        .filter(|(shape, _)| *shape == nodes_field.shape && crate::array::is_bool_array(&s["fixed_dofs"]))
        .map(|(_, v)| v)
        .filter(|v| !v.iter().all(|b| *b))
        .ok_or_else(|| contract("fixed_dofs must be a node-by-XYZ boolean mask with free DOFs"))?;
    let u0 = array(&s["initial_displacement_m"], "initial_displacement_m", Some(&nodes_field.shape))?.values;
    let v0 = array(&s["initial_velocity_m_s"], "initial_velocity_m_s", Some(&nodes_field.shape))?.values;
    if fixed.iter().zip(u0.iter().zip(&v0)).any(|(f, (u, v))| *f && (*u != 0.0 || *v != 0.0)) {
        return Err(contract("Fixed DOFs require zero initial displacement and velocity"));
    }
    let limit = scalar(&s["maximum_strain"], "maximum_strain")?;
    if !(0.0 < limit && limit <= 0.05) {
        return Err(contract("maximum_strain must be positive and at most 0.05 for this small-strain route"));
    }
    let raw_tets = int_rows(&s["tetrahedra"])
        .filter(|t| !t.is_empty())
        .ok_or_else(|| contract("elements require integer [element,4] node indices"))?;
    if raw_tets.iter().flatten().any(|v| *v < 0) {
        return Err(contract("invalid connectivity or unused mesh nodes"));
    }
    let tets: Vec<[usize; 4]> =
        raw_tets.iter().map(|t| t.map(|v| usize::try_from(v).unwrap_or(usize::MAX))).collect();
    let ne = tets.len();
    let material = |key: &str, name: &str| {
        real_vector(&s[key], ne)
            .ok_or_else(|| contract(&format!("{name} requires one finite real value per tetrahedron")))
    };
    let density = material("density_kg_m3", "density");
    let young = material("young_Pa", "Young modulus");
    let poisson = material("poisson", "Poisson ratio");
    let (density, young, poisson) = (density?, young?, poisson?);
    let assembly = assemble_linear_tetrahedra(&nodes, &tets, &density, &young, &poisson)?;
    let free: Vec<usize> = fixed.iter().enumerate().filter(|(_, f)| !**f).map(|(i, _)| i).collect();
    let layout = wall_loads(&crate::euler3d::initial_state(&p), &p)?;
    if layout.node_positions.is_empty() {
        return Err(contract("No physical wall faces found"));
    }
    conforming_interface_map(&layout.node_positions, &nodes)?;
    let ndof = 3 * nodes.len();
    let mass = submatrix(&assembly.mass.to_dense(), ndof, &free);
    let stiffness = submatrix(&assembly.stiffness.to_dense(), ndof, &free);
    if history_bytes(&p, count) > budgets[0] {
        return Err(contract("Flow history exceeds its declared byte budget"));
    }
    let solid_bytes =
        (4 * (count * sub + 1) - 2).saturating_mul(i64::try_from(free.len() * 8).unwrap_or(i64::MAX));
    if solid_bytes > budgets[1] {
        return Err(contract("Structural history exceeds its declared byte budget"));
    }
    Ok(Prepared {
        flow: p,
        step_s: dt,
        step_count: usize::try_from(count).unwrap_or(usize::MAX),
        solid_substeps: usize::try_from(sub).unwrap_or(usize::MAX),
        flow_scheme: scheme,
        flow_history_byte_budget: budgets[0],
        solid_history_byte_budget: budgets[1],
        nodes,
        tets,
        young,
        poisson,
        assembly,
        free,
        mass,
        stiffness,
        u0,
        v0,
        layout,
        maximum_strain: limit,
    })
}

fn rows_of(value: &Value) -> Vec<Vec<f64>> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    r.as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn array3(rows: &[Vec<[f64; 3]>]) -> ArrayD<f64> {
    let n = rows.first().map_or(0, Vec::len);
    let data: Vec<f64> = rows.iter().flatten().flatten().copied().collect();
    ArrayD::from_shape_vec(IxDyn(&[rows.len(), n, 3]), data).unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
}

fn flat_array(shape: &[usize], data: Vec<f64>) -> ArrayD<f64> {
    ArrayD::from_shape_vec(IxDyn(shape), data).unwrap_or_else(|_| ArrayD::zeros(IxDyn(&[0])))
}

    #[allow(clippy::too_many_lines)]
    pub fn evaluate_value(problem: &Value) -> PResult<Evaluation> {
        let prep = prepare(problem)?;
        let p = &prep.flow;
        let (dt, count, sub) = (prep.step_s, prep.step_count, prep.solid_substeps);
        let n_nodes = prep.nodes.len();
        let ndof = 3 * n_nodes;
        let time = &problem["time_integration"];
        let (gas, forces, applied): FlowLoads = if prep.flow_scheme == "muscl_ssprk2" {
            let (states, ledger) = crate::euler3d_muscl::checked_history(
                &problem["flow"],
                &time["step_s"],
                &time["step_count"],
                &time["flow_history_byte_budget"],
                None,
            )?;
            let loads = crate::euler3d_muscl::conforming_solid_load_history(
                &states,
                p,
                &prep.layout,
                dt,
                &prep.nodes,
            )?;
            (ledger.to_map(), loads.instantaneous_nodal_forces_n, loads.interval_nodal_forces_n)
        } else {
            let admitted = checked_fixed_history(
                &problem["flow"],
                &time["step_s"],
                &time["step_count"],
                &time["flow_history_byte_budget"],
                None,
            )?;
            let loads = crate::euler3d_ad::conforming_solid_load_history(
                &admitted.states,
                p,
                &prep.layout,
                dt,
                &prep.nodes,
            )?;
            let forces = loads.nodal_forces_n;
            let applied = forces[..forces.len() - 1].to_vec();
            (admitted.info, forces, applied)
        };
        #[allow(clippy::cast_precision_loss)]
        let times = crate::quasi1d_euler::linspace(0.0, dt * count as f64, count * sub + 1);
        let interval_force: Vec<Vec<f64>> = applied
            .iter()
            .flat_map(|row| std::iter::repeat_n(row.iter().flatten().copied().collect::<Vec<f64>>(), sub))
            .collect();
        let free_forces: Vec<Vec<f64>> =
            interval_force.iter().map(|r| prep.free.iter().map(|i| r[*i]).collect()).collect();
        let free_u0: Vec<f64> = prep.free.iter().map(|i| prep.u0[*i]).collect();
        let free_v0: Vec<f64> = prep.free.iter().map(|i| prep.v0[*i]).collect();
        let nf = prep.free.len();
        let zeros = DenseMatrix { nrows: nf, ncols: nf, data: vec![0.0; nf * nf] };
        let result = checked_linear_history(
            &prep.mass,
            &zeros,
            &prep.stiffness,
            &free_forces,
            &times,
            &free_u0,
            &free_v0,
            prep.solid_history_byte_budget,
            "interval_constant",
        )?;
        let history = &result["history"];
        let nt = times.len();
        let mut displacement = vec![vec![0.0; ndof]; nt];
        for (row, solved) in displacement.iter_mut().zip(rows_of(&history["displacement_m"])) {
            for (i, v) in prep.free.iter().zip(solved) {
                row[*i] = v;
            }
        }
        let mut reactions: Vec<(String, Vec<Vec<[f64; 3]>>)> = Vec::new();
        for (side, offset) in [("start", 0usize), ("end", 1usize)] {
            let acc_rows = rows_of(&history[format!("interval_{side}_acceleration_m_s2").as_str()]);
            let mut out = Vec::with_capacity(nt - 1);
            let mut worst: f64 = 0.0;
            for (step, acc_free) in acc_rows.iter().enumerate() {
                let mut acc = vec![0.0; ndof];
                for (i, v) in prep.free.iter().zip(acc_free) {
                    acc[*i] = *v;
                }
                let inertia =
                    prep.assembly.mass.matvec(&acc).map_err(|e| ModelError::contract(e.to_string()))?;
                let elastic = prep
                    .assembly
                    .stiffness
                    .matvec(&displacement[step + offset])
                    .map_err(|e| ModelError::contract(e.to_string()))?;
                let f = &interval_force[step];
                let mut residual: Vec<f64> = (0..ndof).map(|i| inertia[i] + elastic[i] - f[i]).collect();
                for i in &prep.free {
                    let scale = (inertia[*i].abs() + elastic[*i].abs() + f[*i].abs()).max(1.0);
                    worst = worst.max(residual[*i].abs() / scale);
                }
                for i in &prep.free {
                    residual[*i] = 0.0;
                }
                out.push(residual.chunks(3).map(|c| [c[0], c[1], c[2]]).collect());
            }
            if worst > 1e-10 {
                return Err(contract("Full-mesh free-DOF equilibrium failed after load transfer"));
            }
            reactions.push((format!("interval_{side}_support_reactions_N"), out));
        }
        let stress_value = recover_linear_stress_history(&prep.assembly.stress, &displacement)?;
        let stress: Vec<Vec<[f64; 6]>> = stress_value["stress_physical_Pa"]
            .as_array()
            .map(|t| {
                t.iter()
                    .map(|e| {
                        e.as_array()
                            .map(|rows| {
                                rows.iter()
                                    .map(|r| {
                                        let v: Vec<f64> = r
                                            .as_array()
                                            .map(|x| x.iter().filter_map(Value::as_f64).collect())
                                            .unwrap_or_default();
                                        std::array::from_fn(|c| v.get(c).copied().unwrap_or(f64::NAN))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ne = prep.tets.len();
        let mut von_mises = vec![vec![0.0; ne]; nt];
        let mut strain = vec![vec![0.0; ne]; nt];
        for t in 0..nt {
            for e in 0..ne {
                let s = stress[t][e];
                let mean = (s[0] + s[1] + s[2]) / 3.0;
                let dev = [s[0] - mean, s[1] - mean, s[2] - mean];
                let dev2 = dev[0] * dev[0] + dev[1] * dev[1] + dev[2] * dev[2];
                let shear2 = s[3] * s[3] + s[4] * s[4] + s[5] * s[5];
                von_mises[t][e] = (1.5 * (dev2 + 2.0 * shear2)).sqrt();
                let (young, nu) = (prep.young[e], prep.poisson[e]);
                let trace = s[0] + s[1] + s[2];
                let normal: [f64; 3] = std::array::from_fn(|c| ((1.0 + nu) * s[c] - nu * trace) / young);
                let shear: [f64; 3] = std::array::from_fn(|c| s[c + 3] * (1.0 + nu) / young);
                strain[t][e] = (normal.iter().map(|x| x * x).sum::<f64>()
                    + 2.0 * shear.iter().map(|x| x * x).sum::<f64>())
                .sqrt();
            }
        }
        let peak = |rows: &[Vec<f64>]| {
            rows.iter().map(|r| r.iter().copied().fold(f64::NEG_INFINITY, f64::max)).collect::<Vec<f64>>()
        };
        let (vm_peak, strain_peak) = (peak(&von_mises), peak(&strain));
        let global = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if global(&strain_peak) > prep.maximum_strain {
            return Err(contract("Solved strain exceeds declared small-strain applicability limit"));
        }
        let disp_peak: Vec<f64> = displacement
            .iter()
            .map(|row| {
                row.chunks(3)
                    .map(|c| (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt())
                    .fold(f64::NEG_INFINITY, f64::max)
            })
            .collect();
        let fatigue = match problem.get("fatigue") {
            Some(settings) => {
                Some(implexity_physics_solid::fatigue::evaluate_rainflow(settings, &stress, &times, None)?)
            }
            None => None,
        };
        let mut responses = std::collections::BTreeMap::new();
        responses.insert("peak_displacement_m".into(), global(&disp_peak));
        responses.insert("peak_von_mises_Pa".into(), global(&vm_peak));
        responses.insert("peak_strain_norm".into(), global(&strain_peak));
        let mut structure = result.as_object().cloned().unwrap_or_default();
        structure.shift_remove("history");
        let mut diagnostics = Map::new();
        diagnostics.insert("limitations".into(), strs(&LIMITS));
        diagnostics.insert("physical_qualification".into(), json!(false));
        diagnostics.insert("fatigue".into(), fatigue.clone().unwrap_or_else(|| json!({})));
        diagnostics.insert("flow_scheme".into(), json!(prep.flow_scheme));
        diagnostics.insert(
            "force_sampling".into(),
            json!(if prep.flow_scheme == "muscl_ssprk2" {
                "RK_stage_average_held_constant"
            } else {
                "step_start_held_constant"
            }),
        );
        diagnostics.insert("support_reaction_convention".into(), json!("Force exerted by stationary supports on the solid. Separate interval start/end values retain jumps in the piecewise-constant CFD load. Free DOFs have zero reaction."));
        diagnostics.insert("flow".into(), Value::Object(gas));
        diagnostics.insert("structure".into(), Value::Object(structure));
        let mut fields = std::collections::BTreeMap::new();
        fields.insert("time_s".to_string(), FieldValue::Array(flat_array(&[nt], times.clone())));
        fields.insert(
            "solid_nodes_m".into(),
            FieldValue::Array(flat_array(&[n_nodes, 3], prep.nodes.iter().flatten().copied().collect())),
        );
        fields.insert("solid_tetrahedra".into(), FieldValue::Json(json!(prep.tets)));
        if let Some(f) = fatigue.as_ref().filter(|f| f.as_object().is_some_and(|m| !m.is_empty())) {
            fields
                .insert("fatigue_usage_per_element".into(), FieldValue::Json(f["usage_per_element"].clone()));
        }
        fields.insert(
            "displacement_history_m".into(),
            FieldValue::Array(flat_array(&[nt, n_nodes, 3], displacement.concat())),
        );
        fields.insert(
            "stress_history_Pa".into(),
            FieldValue::Array(flat_array(&[nt, ne, 6], stress.iter().flatten().flatten().copied().collect())),
        );
        fields.insert("stress_order".into(), FieldValue::Json(json!(["xx", "yy", "zz", "xy", "yz", "xz"])));
        #[allow(clippy::cast_precision_loss)]
        fields.insert(
            "force_time_s".into(),
            FieldValue::Array(flat_array(&[count + 1], (0..=count).map(|n| n as f64 * dt).collect())),
        );
        fields.insert("nodal_force_history_N".into(), FieldValue::Array(array3(&forces)));
        fields.insert("interval_nodal_forces_N".into(), FieldValue::Array(array3(&applied)));
        for (key, rows) in reactions {
            fields.insert(key, FieldValue::Array(array3(&rows)));
        }
        fields.insert("peak_displacement_history_m".into(), FieldValue::Array(flat_array(&[nt], disp_peak)));
        fields.insert("peak_von_mises_history_Pa".into(), FieldValue::Array(flat_array(&[nt], vm_peak)));
        fields.insert("peak_strain_history".into(), FieldValue::Array(flat_array(&[nt], strain_peak)));
        Ok(Evaluation { provider: NAME.into(), responses, diagnostics, fields })
    }
