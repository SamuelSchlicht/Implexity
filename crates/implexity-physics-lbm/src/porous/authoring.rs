// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::{CaeError, CaeResult};
use implexity_physics_solid::solid_history::{SolidKernel, normalise as solid_normalise};
use serde_json::{Map, Value, json};

use super::catalog::{self, WALL};
use super::geometry::{GeometryBinding, SOURCE_PROFILE};
use super::lattice::Grid;
use super::owner::{Owner, OwnerSpec, Viscosity};
use super::sp::{self, Sp};
use super::trace::PressureTrace;
use super::viscous::{normalise_initialization, normalise_selection as viscous_selection};
use super::wall::{self, ReferenceWall, pressure_array};
use crate::nparray::{Kind, NdArray, asarray};

fn err(msg: impl Into<String>) -> CaeError {
    CaeError::contract(msg.into())
}

fn sorted_keys(keys: &[&str]) -> String {
    let mut k = keys.to_vec();
    k.sort_unstable();
    format!("[{}]", k.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", "))
}

fn object(value: &Value, keys: &[&str], label: &str) -> CaeResult<Map<String, Value>> {
    match value.as_object() {
        Some(m) if m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)) => Ok(m.clone()),
        _ => Err(err(format!("{label} requires exact fields {}", sorted_keys(keys)))),
    }
}

fn real(value: &Value, label: &str, shape: Option<&[usize]>) -> CaeResult<NdArray> {
    let a = asarray(value);
    if !matches!(a.kind, Kind::Int | Kind::Float) || !a.all_finite() || shape.is_some_and(|s| a.shape != s) {
        return Err(err(format!("{label} requires finite real exact-shape values")));
    }
    Ok(a)
}

fn scalar(value: &Value, label: &str) -> CaeResult<f64> {
    Ok(real(value, label, Some(&[]))?.data[0])
}

fn text(value: &Value, label: &str) -> CaeResult<()> {
    if value.as_str().is_some_and(|s| !s.trim().is_empty()) {
        Ok(())
    } else {
        Err(err(format!("{label} requires nonempty text")))
    }
}


pub fn trace_matrix(value: &Value, shape: (usize, usize)) -> CaeResult<Sp> {
    if value.is_object() {
        let a = object(value, &["format", "shape", "row", "col", "data"], "sparse trace")?;
        let declared: Option<Vec<usize>> = a["shape"]
            .as_array()
            .map(|s| s.iter().filter_map(|v| v.as_u64().and_then(|v| usize::try_from(v).ok())).collect());
        if a["format"] != json!("coo") || declared != Some(vec![shape.0, shape.1]) {
            return Err(err("COO trace shape mismatch"));
        }
        let row = asarray(&a["row"]);
        let col = asarray(&a["col"]);
        let data = real(&a["data"], "trace data", None)?;
        let valid = row.kind == Kind::Int
            && col.kind == Kind::Int
            && row.shape.len() == 1
            && row.shape == col.shape
            && row.shape == data.shape
            && row.data.iter().all(|v| *v >= 0.0 && *v < shape.0 as f64)
            && col.data.iter().all(|v| *v >= 0.0 && *v < shape.1 as f64);
        if !valid {
            return Err(err("invalid COO indices"));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let r: Vec<usize> = row.data.iter().map(|v| *v as usize).collect();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let c: Vec<usize> = col.data.iter().map(|v| *v as usize).collect();
        return sp::triplets(shape.0, shape.1, &r, &c, &data.data);
    }
    let a = real(value, "trace matrix", Some(&[shape.0, shape.1]))?;
    Sp::from_dense(shape.0, shape.1, &a.data).map_err(|e| err(e.to_string()))
}


pub fn history_selection(value: &Value) -> CaeResult<Vec<String>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let names = catalog::history();
    let bad = || err("history_responses requires unique supported response names");
    let items = value.as_array().ok_or_else(bad)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(name) = item.as_str() else { return Err(bad()) };
        if !names.iter().any(|h| h.name == name) || out.iter().any(|o: &String| o == name) {
            return Err(bad());
        }
        out.push(name.to_string());
    }
    Ok(out)
}

fn py_list(names: &[String]) -> String {
    let mut n = names.to_vec();
    n.sort();
    format!("[{}]", n.iter().map(|v| format!("'{v}'")).collect::<Vec<_>>().join(", "))
}

fn wall_response_selection(names: &[String], wall: &Value) -> CaeResult<()> {
    let mut missing = vec!["history_pressure_trace_solid_work_J", "history_viscous_trace_solid_work_J"];
    if wall["face"] == json!("x_min") {
        missing.extend(["history_min_inlet_mass_flow_kg_s", "history_inlet_transported_mass_kg"]);
    } else {
        missing.extend(["history_min_outlet_mass_flow_kg_s", "history_outlet_transported_mass_kg"]);
    }
    let hit: Vec<String> = names.iter().filter(|n| missing.contains(&n.as_str())).cloned().collect();
    if !hit.is_empty() {
        return Err(err(format!(
            "selected reference wall replaces the requested trace or pressure port: {}",
            py_list(&hit)
        )));
    }
    Ok(())
}

fn work_selection(names: &[String], viscous: Option<&Value>) -> CaeResult<()> {
    for (name, flag) in [
        ("history_viscous_trace_solid_work_J", "trace_traction"),
        ("history_viscous_dissipation_J", "heating"),
    ] {
        if names.iter().any(|n| n == name) && !viscous.is_some_and(|v| v[flag] == json!(true)) {
            return Err(err(format!("{name} requires explicitly selected viscous {flag}")));
        }
    }
    Ok(())
}

fn optional_bool(raw: &mut Map<String, Value>, key: &str) -> CaeResult<Option<bool>> {
    match raw.remove(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(b)),
        Some(_) => Err(err(format!("{key} must be a boolean"))),
    }
}


pub fn normalise(config: &Value) -> CaeResult<Value> {
    let Some(raw) = config.as_object() else {
        return Err(err("porous problem must be a mapping"));
    };
    let mut raw = raw.clone();
    let wall = wall::normalise_selection(&raw.remove("reference_wall").unwrap_or(Value::Null))?;
    let initialization = raw.remove("flow_initialization").unwrap_or(Value::Null);
    let energy_audit = optional_bool(&mut raw, "energy_audit_diagnostics")?;
    let mechanical = optional_bool(&mut raw, "mechanical_work_diagnostics")?;
    let viscous = viscous_selection(&raw.remove("viscous_coupling").unwrap_or(Value::Null))?;
    let history = history_selection(&raw.remove("history_responses").unwrap_or(Value::Null))?;
    let keys = [
        "name",
        "provenance",
        "solid",
        "spacing_m",
        "step_s",
        "reference_density_kg_m3",
        "fluid_specific_heat_J_kgK",
        "viscosity",
        "drag",
        "ports",
        "geometry",
        "pressure_trace",
    ];
    let mut p = object(&Value::Object(raw), &keys, "problem")?;
    for k in ["name", "provenance"] {
        text(&p[k], k)?;
    }
    let solid = solid_normalise(&p["solid"])?;
    if solid.get("structural_dynamics").is_some_and(|v| !v.is_null()) {
        return Err(err(
            "structural_dynamics is supported by native_solid_history only; the porous reference-wall coupling is quasistatic and has no inertial interface closure",
        ));
    }
    let grid: Vec<usize> = solid["grid"]
        .as_array()
        .map(|g| g.iter().filter_map(|v| v.as_u64().and_then(|v| usize::try_from(v).ok())).collect())
        .unwrap_or_default();
    let times: Vec<f64> =
        solid["times_s"].as_array().map(|t| t.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let nt = times.len();
    p.insert("solid".into(), solid.clone());
    if grid[0] < 5 {
        return Err(err("two clear collars and at least one interior x cell required"));
    }
    for k in ["spacing_m", "step_s", "reference_density_kg_m3", "fluid_specific_heat_J_kgK"] {
        let a = scalar(&p[k], k)?;
        if a <= 0.0 {
            return Err(err(format!("{k} must be positive")));
        }
        p.insert(k.into(), json!(a));
    }
    let f = |k: &str, p: &Map<String, Value>| p[k].as_f64().unwrap_or(f64::NAN);
    let (h, dt, rho) = (f("spacing_m", &p), f("step_s", &p), f("reference_density_kg_m3", &p));
    if times.windows(2).any(|w| ((w[1] - w[0]) - dt).abs() > 1e-13 * dt.abs()) {
        return Err(err("native and lattice time intervals differ"));
    }
    if !p["viscosity"].is_object() {
        return Err(err("viscosity object required"));
    }
    let kind = p["viscosity"].get("kind").cloned().unwrap_or(Value::Null);
    let vkeys: &[&str] = if kind == json!("constant") {
        &["kind", "nu_m2_s", "temperature_interval_K", "provenance"]
    } else if kind == json!("exponential_temperature") {
        &[
            "kind",
            "reference_nu_m2_s",
            "reference_temperature_K",
            "log_slope_per_K",
            "temperature_interval_K",
            "provenance",
        ]
    } else {
        return Err(err("unsupported viscosity law"));
    };
    let v = object(&p["viscosity"], vkeys, "viscosity")?;
    text(&v["provenance"], "viscosity provenance")?;
    let constant = kind == json!("constant");
    let nu = scalar(if constant { &v["nu_m2_s"] } else { &v["reference_nu_m2_s"] }, "viscosity")?;
    let interval = real(&v["temperature_interval_K"], "temperature interval", Some(&[2]))?.data;
    if nu <= 0.0 || interval[0] <= 0.0 || interval[1] <= interval[0] {
        return Err(err("positive viscosity and ordered positive temperature interval required"));
    }
    if !constant {
        let reference = scalar(&v["reference_temperature_K"], "reference temperature")?;
        let slope = scalar(&v["log_slope_per_K"], "log slope")?;
        if reference <= 0.0 {
            return Err(err("positive reference temperature required"));
        }
        let ends: Vec<f64> = interval.iter().map(|t| nu * (slope * (t - reference)).exp()).collect();
        if ends.iter().any(|e| !e.is_finite() || *e <= 0.0) {
            return Err(err("viscosity law uncomputable within authored interval"));
        }
    }
    p.insert("viscosity".into(), Value::Object(v));
    let d = object(&p["drag"], &["kind", "coefficient_kg_m3_s", "provenance"], "drag")?;
    if d["kind"] != json!("linear_solid_fraction") {
        return Err(err("unsupported drag law"));
    }
    text(&d["provenance"], "drag provenance")?;
    if scalar(&d["coefficient_kg_m3_s"], "drag")? < 0.0 {
        return Err(err("negative drag"));
    }
    p.insert("drag".into(), Value::Object(d));
    let b = object(
        &p["ports"],
        &[
            "profile",
            "gauge_pressure_Pa",
            "reservoir_temperature_K",
            "reference_pressure_Pa",
            "exterior_pressure_Pa",
        ],
        "ports",
    )?;
    let expected_profile = if wall.is_none() { "planar_x_clear_periodic_yz" } else { wall::PROFILE };
    if b["profile"] != json!(expected_profile) {
        return Err(err("port/halo profile must match the explicit wall selection"));
    }
    let pressure: Vec<f64> = match &wall {
        None => real(&b["gauge_pressure_Pa"], "pressure", Some(&[nt, 2]))?.data,
        Some(w) => pressure_array(&b["gauge_pressure_Pa"], nt, w)?.into_iter().flatten().collect(),
    };
    let reference = scalar(&b["reference_pressure_Pa"], "pressure reference")?;
    scalar(&b["exterior_pressure_Pa"], "exterior pressure")?;
    if reference <= 0.0 || pressure.iter().any(|v| reference + v <= 0.0) {
        return Err(err("nonpositive absolute physical pressure"));
    }
    if pressure.iter().any(|v| 1.0 / 3.0 + v * dt.powi(2) / (rho * h.powi(2)) <= 0.0) {
        return Err(err("nonpositive lattice EOS pressure"));
    }
    let reservoir = real(&b["reservoir_temperature_K"], "reservoir", None)?;
    if !(reservoir.shape.is_empty() || reservoir.shape == grid)
        || reservoir.data.iter().any(|t| *t < interval[0] || *t > interval[1])
    {
        return Err(err("reservoir outside exact shape/evaluation interval"));
    }
    p.insert("ports".into(), Value::Object(b.clone()));
    let g = object(&p["geometry"], &["profile", "fixed_clear_mask", "fixed_ghost_composition"], "geometry")?;
    if g["profile"] != json!(SOURCE_PROFILE) {
        return Err(err("explicit common quadrature profile required"));
    }
    let mask = asarray(&g["fixed_clear_mask"]);
    let plane = grid[1] * grid[2];
    let collars = |m: &NdArray| {
        let nx = grid[0];
        (0..plane).all(|k| m.data[k] != 0.0 && m.data[plane + k] != 0.0)
            && (0..plane).all(|k| m.data[(nx - 1) * plane + k] != 0.0 && m.data[(nx - 2) * plane + k] != 0.0)
    };
    if mask.shape != grid || mask.kind != Kind::Bool || !collars(&mask) {
        return Err(err("two authored clear collars required"));
    }
    let ghost = real(&g["fixed_ghost_composition"], "ghost mixture", Some(&grid))?;
    if ghost.data.iter().any(|v| *v < 0.0 || *v > 1.0) {
        return Err(err("ghost mixture outside [0,1]"));
    }
    if solid.get("inactive_phase_numerical_material").is_none() {
        return Err(err("explicit native inactive-phase numerical material required"));
    }
    p.insert("geometry".into(), Value::Object(g));
    if let Some(w) = &wall {
        if !p["pressure_trace"].is_null() {
            return Err(err(
                "reference_wall replaces pressure_trace; set pressure_trace to null to avoid duplicated interface loads",
            ));
        }
        if viscous.as_ref().is_some_and(|v| v["trace_traction"] == json!(true)) {
            return Err(err(
                "wall momentum exchange already contains traction; a separate viscous trace would double count it",
            ));
        }
        if let Some(v) = &viscous {
            p.insert("viscous_coupling".into(), v.clone());
        }
        p.insert("reference_wall".into(), w.clone());
        wall_response_selection(&history, w)?;
    } else {
        let tkeys = [
            "left_to_interface",
            "right_to_interface",
            "interface_measure_m2",
            "interface_coordinates_m",
            "solid_outward_normals",
            "viscous_traction_Pa",
            "coordinate_tolerance_m",
            "provenance",
        ];
        let tr = object(&p["pressure_trace"], &tkeys, "pressure trace")?;
        text(&tr["provenance"], "trace provenance")?;
        let measures = real(&tr["interface_measure_m2"], "interface measure", None)?;
        if measures.shape.len() != 1 || measures.data.is_empty() || measures.data.iter().any(|v| *v <= 0.0) {
            return Err(err("positive interface measures required"));
        }
        let ni = measures.data.len();
        let nn: usize = grid.iter().map(|v| v + 1).product();
        trace_matrix(&tr["left_to_interface"], (ni, nn))?;
        trace_matrix(&tr["right_to_interface"], (ni, grid.iter().product()))?;
        for key in ["interface_coordinates_m", "solid_outward_normals", "viscous_traction_Pa"] {
            real(&tr[key], key, Some(&[ni, 3]))?;
        }
        if scalar(&tr["coordinate_tolerance_m"], "coordinate tolerance")? <= 0.0 {
            return Err(err("positive coordinate tolerance required"));
        }
        let normals = real(&tr["solid_outward_normals"], "solid_outward_normals", Some(&[ni, 3]))?.data;
        if normals.chunks(3).any(|n| ((n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt() - 1.0).abs() > 1e-12) {
            return Err(err("unit interface normals required"));
        }
        let shear = real(&tr["viscous_traction_Pa"], "viscous_traction_Pa", Some(&[ni, 3]))?.data;
        p.insert("pressure_trace".into(), Value::Object(tr));
        if let Some(v) = &viscous {
            if v["trace_traction"] == json!(true) && shear.iter().any(|s| *s != 0.0) {
                return Err(err("resolved viscous traction cannot silently add to authored shear"));
            }
            p.insert("viscous_coupling".into(), v.clone());
        }
        let wall_names: Vec<&str> = WALL.iter().map(|d| d.0).collect();
        if history.iter().any(|n| wall_names.contains(&n.as_str())) {
            return Err(err("wall history responses require reference_wall"));
        }
    }
    if !history.is_empty() {
        p.insert("history_responses".into(), json!(history));
    }
    let init = normalise_initialization(&initialization, [grid[0], grid[1], grid[2]], rho, h, dt, reference)?;
    if let Some(i) = init {
        p.insert("flow_initialization".into(), i);
    }
    if let Some(m) = mechanical {
        p.insert("mechanical_work_diagnostics".into(), json!(m));
    }
    if let Some(e) = energy_audit {
        p.insert("energy_audit_diagnostics".into(), json!(e));
    }
    work_selection(&history, viscous.as_ref())?;
    Ok(Value::Object(p))
}

pub struct Binding {
    pub owner: Arc<Owner>,
    pub design: Vec<f64>,
    pub raw_design_indices: Vec<usize>,
    pub geometry_receipt: Map<String, Value>,
}


pub fn build(p: &Value, rho: &[f64], composition: &[f64], batch: usize) -> CaeResult<Binding> {
    if batch == 0 {
        return Err(err("explicit positive derivative batch size required"));
    }
    let solid = Arc::new(SolidKernel::new(p["solid"].clone())?);
    let grid = Grid { n: solid.grid };
    let nc = grid.cells();
    let mask: Vec<bool> = asarray(&p["geometry"]["fixed_clear_mask"]).bools();
    let ghost = asarray(&p["geometry"]["fixed_ghost_composition"]).data;
    if mask.len() != nc || rho.len() != nc || composition.len() != nc {
        return Err(err("exact-grid raw channels and fixed mask required"));
    }
    if (0..nc).any(|i| mask[i] && (rho[i] != 0.0 || composition[i] != ghost[i])) {
        return Err(err("authored fixed coordinates differ from supplied geometry"));
    }
    let free: Vec<usize> = (0..nc).filter(|i| !mask[*i]).collect();
    let h = p["spacing_m"].as_f64().unwrap_or(f64::NAN);
    let dt = p["step_s"].as_f64().unwrap_or(f64::NAN);
    let rho_ref = p["reference_density_kg_m3"].as_f64().unwrap_or(f64::NAN);
    let ports = &p["ports"];
    let reference = asarray(&ports["reference_pressure_Pa"]).data[0];
    let exterior = asarray(&ports["exterior_pressure_Pa"]).data[0];
    let nt = solid.times.len();
    let geometry = GeometryBinding::new(grid, mask.clone(), ghost.clone(), h)?;
    let wall_sel = p.get("reference_wall").filter(|v| !v.is_null());
    let trace = if wall_sel.is_none() {
        let t = &p["pressure_trace"];
        let ni = asarray(&t["interface_measure_m2"]).data.len();
        #[allow(clippy::cast_precision_loss)]
        let solid_coords: Vec<[f64; 3]> = solid.mesh.ijk.iter().map(|q| q.map(|v| v as f64 * h)).collect();
        #[allow(clippy::cast_precision_loss)]
        let fluid_coords: Vec<[f64; 3]> =
            (0..nc).map(|c| grid.ijk(c).map(|v| (v as f64 + 0.5) * h)).collect();
        let rows3 = |key: &str| -> Vec<[f64; 3]> {
            asarray(&t[key]).data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
        };
        Some(PressureTrace::new(
            trace_matrix(&t["left_to_interface"], (ni, solid.nn))?,
            trace_matrix(&t["right_to_interface"], (ni, nc))?,
            asarray(&t["interface_measure_m2"]).data,
            &solid_coords,
            &fluid_coords,
            &rows3("interface_coordinates_m"),
            rows3("solid_outward_normals"),
            reference,
            exterior,
            rows3("viscous_traction_Pa"),
            asarray(&t["coordinate_tolerance_m"]).data[0],
            t["provenance"].as_str().unwrap_or_default(),
        )?)
    } else {
        None
    };
    let v = &p["viscosity"];
    let viscosity = if v["kind"] == json!("constant") {
        Viscosity::Constant(asarray(&v["nu_m2_s"]).data[0])
    } else if v["kind"] == json!("exponential_temperature") {
        Viscosity::Exponential {
            nu: asarray(&v["reference_nu_m2_s"]).data[0],
            reference: asarray(&v["reference_temperature_K"]).data[0],
            slope: asarray(&v["log_slope_per_K"]).data[0],
        }
    } else {
        return Err(err("unsupported authored viscosity law"));
    };
    if p["drag"]["kind"] != json!("linear_solid_fraction") {
        return Err(err("unsupported authored drag law"));
    }
    let beta = asarray(&p["drag"]["coefficient_kg_m3_s"]).data[0] * dt / rho_ref;
    let (wall, face_pressure) = match wall_sel {
        Some(sel) => (
            Some(ReferenceWall::new(grid, solid.nn, sel, h, dt, rho_ref, reference, exterior)?),
            pressure_array(&ports["gauge_pressure_Pa"], nt, sel)?,
        ),
        None => (None, asarray(&ports["gauge_pressure_Pa"]).data.chunks(2).map(|c| [c[0], c[1]]).collect()),
    };
    let res = asarray(&ports["reservoir_temperature_K"]).data;
    let reservoir = if res.len() == 1 { vec![res[0]; nc] } else { res };
    let interval = asarray(&v["temperature_interval_K"]).data;
    let owner = Owner::new(OwnerSpec {
        s: solid,
        h,
        dt,
        rho: rho_ref,
        cp: p["fluid_specific_heat_J_kgK"].as_f64().unwrap_or(f64::NAN),
        viscosity,
        beta,
        face_pressure,
        reservoir,
        trace,
        wall,
        viscous: p.get("viscous_coupling").filter(|v| !v.is_null()).cloned(),
        flow_initialization: p.get("flow_initialization").filter(|v| !v.is_null()).cloned(),
        geometry,
        temperature_interval: (interval[0], interval[1]),
        reference_pressure: reference,
        batch,
    })?;
    let mut design: Vec<f64> = free.iter().map(|i| rho[*i]).collect();
    design.extend(free.iter().map(|i| composition[*i]));
    let receipt = owner.validate_design(&design)?;
    let mut raw_design_indices = free.clone();
    raw_design_indices.extend(free.iter().map(|i| nc + i));
    Ok(Binding { owner: Arc::new(owner), design, raw_design_indices, geometry_receipt: receipt })
}
