// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use implexity_ad::{Dual, Scalar};
use implexity_core::CaeResult;
use implexity_core::contracts::FieldValue;
use implexity_core::history_field_sources::HistorySourceComponent;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{CoupledHistoryAssembly, HistoryBlock, HistoryInterface};
use implexity_solve::local_assembly::{Kind, LocalResidual, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;

use crate::common::{
    assembly_options, design_incidence, err, extend_coupling, grid_of, incidence, py_int, real_array,
    thermal_rows, times_match, zero_csr,
};
use crate::host::{
    BoundFieldSource, BoundSource, FieldHost, FieldSourceAuthoring, ResponseVjp, SourceEnergy, block_start,
    host_of,
};
use crate::prescribed_joule::{array, strings};

pub const NAME: &str = "prescribed_volumetric_heating";
pub const TIME_INTEGRATION: &str = "backward_Euler_endpoint_sum_P_n_dt_n_matching_host_history";
pub const PHASE: &str = "linear_endmember_mixture_as_solid_properties";
pub const OCCUPANCY: &str = "solid_endmembers_times_occupancy_fluid_times_complementary_fraction";
pub const ENERGY: &str = "authored_heat_excludes_prescribed_volumetric_heating";
pub const PLACEMENT: &str = "solid_T4_element_row_sum_nodal_dual_volume_via_local_assembly";
pub const SETTINGS: [&str; 10] = [
    "name",
    "provenance",
    "times_s",
    "time_factor",
    "solid_endmember_power_W_m3",
    "fluid_power_W_m3",
    "attenuation",
    "phase_interpolation",
    "occupancy_convention",
    "energy_convention",
];
const STATE_CONTRACT: &str = "prescribed_zero_state_source_v1";
const DATA: &str = include_str!("data/prescribed_volumetric_heating.json");

fn data() -> Value {
    serde_json::from_str(DATA).unwrap_or(Value::Null)
}

#[must_use]
pub fn limitations() -> Vec<String> {
    strings(&data()["limitations"])
}

#[must_use]
pub fn py_shape(shape: &[usize]) -> String {
    match shape.len() {
        0 => "()".into(),
        1 => format!("({},)", shape[0]),
        _ => format!("({})", shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")),
    }
}

fn text(value: &Value, label: &str) -> CaeResult<String> {
    match value.as_str() {
        Some(s)
            if !s.trim().is_empty()
                && s.chars().count() <= 4096
                && !s.chars().any(|c| (c as u32) < 32 && c != '\n' && c != '\t') =>
        {
            Ok(s.to_string())
        }
        _ => Err(err(format!("prescribed volumetric heating requires a nonempty bounded {label}"))),
    }
}

fn nonnegative(value: &Value, shape: &[usize], label: &str) -> CaeResult<Vec<f64>> {
    let (s, v) = real_array(value, label)?;
    if s != shape || v.iter().any(|x| *x < 0.0) {
        return Err(err(format!("{label} requires finite nonnegative values of shape {}", py_shape(shape))));
    }
    Ok(v)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Attenuation {
    pub axis: usize,
    pub hi: bool,
    pub length_m: f64,
}

fn attenuation(value: &Value) -> CaeResult<Option<Attenuation>> {
    if value.is_null() {
        return Ok(None);
    }
    let keys = ["axis", "side", "e_folding_length_m"];
    let Some(m) = value.as_object().filter(|m| m.len() == 3 && keys.iter().all(|k| m.contains_key(*k)))
    else {
        return Err(err("attenuation must be null or declare exactly axis, side and e_folding_length_m"));
    };
    let Some(axis) = py_int(&m["axis"]).filter(|a| (0..3).contains(a)) else {
        return Err(err("attenuation axis must be the integer 0, 1 or 2"));
    };
    let hi = match m["side"].as_str() {
        Some("lo") => false,
        Some("hi") => true,
        _ => return Err(err("attenuation side must be lo or hi")),
    };
    let length = nonnegative(&m["e_folding_length_m"], &[], "attenuation e-folding length")?[0];
    if !(length > 0.0) {
        return Err(err("attenuation e-folding length must be positive"));
    }
    Ok(Some(Attenuation { axis: usize::try_from(axis).unwrap_or(0), hi, length_m: length }))
}


pub fn normalise(settings: &Value, context: &Value) -> CaeResult<Value> {
    let ok = settings
        .as_object()
        .is_some_and(|m| m.len() == SETTINGS.len() && SETTINGS.iter().all(|k| m.contains_key(*k)));
    let Some(s) = settings.as_object().filter(|_| ok) else {
        return Err(err(format!("prescribed volumetric heating requires exactly: {}", SETTINGS.join(", "))));
    };
    let Some(solid) = context.get("solid").filter(|v| v.is_object()) else {
        return Err(err("prescribed volumetric heating requires a native solid context"));
    };
    if grid_of(&solid["grid"]).is_none() {
        return Err(err("prescribed volumetric heating requires the native solid grid"));
    }
    let expected = real_array(solid.get("times_s").unwrap_or(&Value::Null), "native solid times")?;
    let times = real_array(&s["times_s"], "prescribed volumetric heating times")?;
    if !times_match(&times, &expected) {
        return Err(err(
            "prescribed volumetric heating times must match every host time node; no interpolation",
        ));
    }
    let factor = nonnegative(&s["time_factor"], &times.0, "prescribed volumetric heating time factor")?;
    if factor[0] != 0.0 {
        return Err(err("prescribed volumetric heating time factor must be zero at the initial node"));
    }
    let solid_power =
        nonnegative(&s["solid_endmember_power_W_m3"], &[2], "ordered solid endmember power densities")?;
    let fluid_power = nonnegative(&s["fluid_power_W_m3"], &[], "fluid power density")?[0];
    if s["phase_interpolation"] != json!(PHASE)
        || s["occupancy_convention"] != json!(OCCUPANCY)
        || s["energy_convention"] != json!(ENERGY)
    {
        return Err(err("unsupported prescribed volumetric heating phase, occupancy or energy convention"));
    }
    let peak_factor = factor.iter().fold(f64::NEG_INFINITY, |a, b| a.max(*b));
    let peak = peak_factor * solid_power[0].max(solid_power[1]).max(fluid_power);
    if !peak.is_finite() {
        return Err(err("prescribed volumetric heating power lies outside the finite floating-point range"));
    }
    let name = text(&s["name"], "name")?;
    let provenance = text(&s["provenance"], "provenance")?;
    let att = attenuation(&s["attenuation"])?;
    Ok(json!({"name": name, "provenance": provenance, "times_s": times.1, "time_factor": factor,
        "solid_endmember_power_W_m3": solid_power, "fluid_power_W_m3": fluid_power,
        "attenuation": att.map(|a| json!({"axis": a.axis, "side": if a.hi { "hi" } else { "lo" }, "e_folding_length_m": a.length_m})),
        "phase_interpolation": PHASE, "occupancy_convention": OCCUPANCY, "energy_convention": ENERGY}))
}

#[must_use]
pub fn depth_index(grid: [usize; 3], att: Option<Attenuation>) -> Vec<f64> {
    let nc: usize = grid.iter().product();
    let Some(a) = att else { return vec![0.0; nc] };
    (0..nc)
        .map(|c| {
            let ijk = [c / (grid[1] * grid[2]), (c / grid[2]) % grid[1], c % grid[2]];
            let i = ijk[a.axis];
            (if a.hi { grid[a.axis] - 1 - i } else { i }) as f64
        })
        .collect()
}

pub fn attenuation_factor<S: Scalar>(spacing_m: &[S; 3], depth: f64, att: Option<Attenuation>) -> S {
    let Some(a) = att else { return S::one() };
    let r = spacing_m[a.axis] / S::from_f64(a.length_m);
    let expm1 = r.chain((-r.value()).exp_m1(), -(-r.value()).exp(), (-r.value()).exp());
    (r * S::from_f64(-depth)).exp() * (-expm1) / r
}

pub fn material_power_density<S: Scalar>(occupancy: S, phase: S, power: [f64; 2], fluid: f64) -> [S; 3] {
    [
        occupancy * (S::one() - phase) * S::from_f64(power[0]),
        occupancy * phase * S::from_f64(power[1]),
        (S::one() - occupancy) * S::from_f64(fluid),
    ]
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    let Some(s) = settings.as_object() else { return json!({}) };
    let times: Vec<Value> = s.get("times_s").and_then(Value::as_array).cloned().unwrap_or_default();
    let title = |i: usize, t: &Value| match t.as_f64().filter(|v| t.is_number() && f64::is_finite(*v)) {
        Some(v) => format!("Time {} s", implexity_core::extensions::format_g6(v)),
        None => format!("Time node {i}"),
    };
    let items: Vec<Value> = times
        .iter()
        .enumerate()
        .map(|(i, t)| json!({"title": title(i, t), "type": "number", "minimum": 0}))
        .collect();
    json!({"properties": {
        "name": {"title": "Heating data name"},
        "provenance": {"title": "Power-density data source and provenance"},
        "time_factor": {"title": "Time factor", "units": "1", "type": "array",
            "description": "Dimensionless multiplier at every host time node; zero at the initial node.", "prefixItems": items},
        "solid_endmember_power_W_m3": {"title": "Solid endmember power densities", "units": "W/m³", "type": "array", "minItems": 2, "maxItems": 2,
            "prefixItems": [{"title": "Solid endmember 1", "type": "number", "minimum": 0}, {"title": "Solid endmember 2", "type": "number", "minimum": 0}],
            "description": "Power per unit volume of fully dense material, in the order of solid.materials; mixed linearly by the phase fraction and weighted by occupancy."},
        "fluid_power_W_m3": {"title": "Fluid power density", "units": "W/m³", "type": "number", "minimum": 0,
            "description": "Power per unit fluid volume, weighted by the complementary fluid fraction."},
        "attenuation": {"title": "Depth attenuation", "description": "Null for spatially uniform densities, or one exponential decay from a declared analysis-domain face.",
            "properties": {"axis": {"title": "Face normal axis", "enum": [0, 1, 2]}, "side": {"title": "Face side", "enum": ["lo", "hi"]},
                "e_folding_length_m": {"title": "e-folding length", "units": "m", "type": "number", "exclusiveMinimum": 0}}}}})
}

#[must_use]
pub fn study_templates(context: &Value) -> Vec<Value> {
    let Some(ctx) = context.as_object() else { return Vec::new() };
    let existing = match ctx.get("field_sources") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Vec::new(),
    };
    if existing.iter().any(|r| !r.is_object() || r.get("component") == Some(&json!(NAME))) {
        return Vec::new();
    }
    let Some(solid) = ctx.get("solid").filter(|v| v.is_object()) else { return Vec::new() };
    let (grid, times) = (&solid["grid"], &solid["times_s"]);
    if grid_of(grid).is_none() || times.as_array().is_none_or(|a| a.len() < 2) {
        return Vec::new();
    }
    let nt = times.as_array().map_or(0, Vec::len);
    let settings = json!({"name": "Prescribed volumetric heating",
        "provenance": "Zero-load authoring starter; replace with sourced power densities",
        "times_s": times, "time_factor": vec![0.0; nt], "solid_endmember_power_W_m3": [0.0, 0.0], "fluid_power_W_m3": 0.0,
        "attenuation": null, "phase_interpolation": PHASE, "occupancy_convention": OCCUPANCY, "energy_convention": ENERGY});
    let Ok(settings) = normalise(&settings, context) else { return Vec::new() };
    let d = data();
    let t = &d["study_template_text"][0];
    let source_schema = json!({"title": d["editor_label"], "properties": {"component": {"title": "Source component", "enum": [NAME]},
        "settings": editor_schema(&settings, context)}});
    let mut prefix: Vec<Value> = existing.iter().map(|_| json!({})).collect();
    prefix.push(source_schema);
    let mut patch = existing.clone();
    patch.push(json!({"component": NAME, "settings": settings}));
    vec![json!({"schema": "implexity-provider-study-template/1", "id": t["id"], "label": t["label"],
        "description": t["description"], "truth_status": t["truth_status"],
        "editor_schema_patch": {"properties": {"field_sources": {"title": "Coupled field sources", "type": "array", "prefixItems": prefix}}},
        "problem_requirements": [{"path": ["solid", "grid"], "value": grid}, {"path": ["solid", "times_s"], "value": times},
            {"path": ["field_sources"], "value": ctx.get("field_sources").cloned().unwrap_or(Value::Null), "missing_equals_null": true}],
        "problem_patch": {"field_sources": patch}})]
}

pub struct BoundVolumetricHeating {
    p: Value,
    solid: Arc<SolidKernel>,
    factor: Vec<f64>,
    power: [f64; 2],
    fluid_power: f64,
    attenuation: Option<Attenuation>,
    depth: Vec<f64>,
    dt: Vec<f64>,
    face_layer: Option<Vec<usize>>,
    exchange: OnceLock<Arc<VolumetricHeatingResidual>>,
}

fn f64s(v: &Value) -> Vec<f64> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()
}

impl BoundVolumetricHeating {

    pub fn new(p: Value, host: &dyn FieldHost) -> CaeResult<Self> {
        let solid = Arc::clone(host.solid());
        if host.problem()["solid"]["grid"] != json!(solid.grid) {
            return Err(err("prescribed volumetric heating grid disagrees with the bound solid"));
        }
        let factor = f64s(&p["time_factor"]);
        let sp = f64s(&p["solid_endmember_power_W_m3"]);
        let att = attenuation(&p["attenuation"])?;
        let depth = depth_index(solid.grid, att);
        let times = f64s(&p["times_s"]);
        let mut dt = vec![0.0];
        dt.extend(times.windows(2).map(|w| w[1] - w[0]));
        let face_layer = att.map(|_| (0..solid.nc).filter(|c| depth[*c] == 0.0).collect());
        Ok(Self {
            fluid_power: p["fluid_power_W_m3"].as_f64().unwrap_or(0.0),
            power: [sp[0], sp[1]],
            p,
            solid,
            factor,
            attenuation: att,
            depth,
            dt,
            face_layer,
            exchange: OnceLock::new(),
        })
    }

    fn spacing<S: Scalar>(x: &[S], nc: usize) -> [S; 3] {
        std::array::from_fn(|a| x[nc + a] * S::from_f64(1e-3))
    }

    #[must_use]
    pub fn densities(&self, n: usize, x: &[f64]) -> Vec<[f64; 3]> {
        let nc = self.solid.nc;
        let h = Self::spacing(x, nc);
        (0..nc)
            .map(|c| {
                let g = attenuation_factor(&h, self.depth[c], self.attenuation);
                material_power_density(x[c], x[nc + 3 + c], self.power, self.fluid_power)
                    .map(|v| self.factor[n] * g * v)
            })
            .collect()
    }

    #[must_use]
    pub fn material_power(&self, n: usize, x: &[f64]) -> [f64; 3] {
        let nc = self.solid.nc;
        let v: f64 = Self::spacing(x, nc).iter().product();
        let d = self.densities(n, x);
        std::array::from_fn(|m| d.iter().map(|r| r[m]).sum::<f64>() * v)
    }

    fn face_occupancy(&self, x: &[f64]) -> Value {
        self.face_layer
            .as_ref()
            .map_or(Value::Null, |l| json!(l.iter().map(|c| x[*c]).sum::<f64>() / l.len() as f64))
    }

    fn power_gradient(&self, n: usize, x: &[f64], weight: f64, out: &mut [f64]) {
        let nc = self.solid.nc;
        for c in 0..nc {
            let v = [x[c], x[nc + 3 + c], x[nc], x[nc + 1], x[nc + 2]];
            let d: [Dual<5>; 5] = std::array::from_fn(|k| Dual::variable(v[k], k));
            let h: [Dual<5>; 3] = std::array::from_fn(|a| d[2 + a] * Dual::from_f64(1e-3));
            let g = attenuation_factor(&h, self.depth[c], self.attenuation);
            let dens = material_power_density(d[0], d[1], self.power, self.fluid_power);
            let p = (dens[0] + dens[1] + dens[2]) * g * Dual::from_f64(self.factor[n]) * (h[0] * h[1] * h[2]);
            out[c] += weight * p.eps[0];
            out[nc + 3 + c] += weight * p.eps[1];
            for a in 0..3 {
                out[nc + a] += weight * p.eps[2 + a];
            }
        }
    }
}

impl BoundFieldSource for BoundVolumetricHeating {
    fn response_units(&self) -> Vec<(String, String)> {
        vec![
            ("prescribed_volumetric_heating_final_W".into(), "W".into()),
            ("prescribed_volumetric_heating_energy_J".into(), "J".into()),
        ]
    }

    fn state_contract(&self) -> Option<&'static str> {
        Some(STATE_CONTRACT)
    }

    fn thermal_placement(&self) -> Option<&'static str> {
        Some(PLACEMENT)
    }

    fn blocks(&self) -> Vec<HistoryBlock> {
        Vec::new()
    }

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()> {
        let residual = Arc::new(VolumetricHeatingResidual::new(self, assembly)?);
        assembly.add_interface(Arc::clone(&residual) as Arc<dyn HistoryInterface>)?;
        self.exchange.set(residual).map_err(|_| err("prescribed volumetric heating attached twice"))
    }

    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>> {
        self.exchange.get().map(|e| Arc::clone(e) as Arc<dyn HistoryInterface>)
    }

    fn energy(&self, n: usize, _z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy> {
        let power: f64 = self.material_power(n, x).iter().sum();
        Ok(SourceEnergy { deposition_w: power, sensible_deposition_w: power, material_production_w: 0.0 })
    }

    fn diagnostics(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>> {
        let parts = self.material_power(n, x);
        let e = self.energy(n, z, x)?;
        let all =
            [parts[0], parts[1], parts[2], e.deposition_w, e.sensible_deposition_w, e.material_production_w];
        if !all.iter().all(|v| f64::is_finite(*v) && *v >= 0.0) {
            return Err(err("invalid prescribed volumetric heating ledger"));
        }
        let mut m = e.to_map();
        m.insert("solid_endmember_0_W".into(), json!(parts[0]));
        m.insert("solid_endmember_1_W".into(), json!(parts[1]));
        m.insert("fluid_W".into(), json!(parts[2]));
        m.insert("time_factor".into(), json!(self.factor[n]));
        m.insert("scope".into(), json!("prescribed_power_density_not_radiation_transport"));
        m.insert("sampling".into(), json!("endpoint"));
        m.insert("energy_increment_J".into(), json!(e.deposition_w * self.dt[n]));
        m.insert("time_integration".into(), json!(TIME_INTEGRATION));
        m.insert(
            "attenuation_depth_origin".into(),
            if self.attenuation.is_some() { json!("analysis_domain_face") } else { Value::Null },
        );
        m.insert("attenuation_face_occupancy".into(), self.face_occupancy(x));
        m.insert("placement".into(), json!(PLACEMENT));
        m.insert("data_name".into(), self.p["name"].clone());
        m.insert("data_provenance".into(), self.p["provenance"].clone());
        m.insert("experimental_qualification_verified".into(), json!(false));
        Ok(m)
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        let final_step = history.len() - 1;
        let power: Vec<f64> =
            (1..=final_step).map(|n| self.material_power(n, x).iter().sum::<f64>()).collect();
        let energy: f64 = power.iter().zip(&self.dt[1..=final_step]).map(|(p, d)| p * d).sum();
        Ok(vec![*power.last().unwrap_or(&0.0), energy])
    }

    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp> {
        let final_step = history.len() - 1;
        let mut xb = vec![0.0; x.len()];
        for n in 1..=final_step {
            let w = weights.get(1).copied().unwrap_or(0.0) * self.dt[n]
                + if n == final_step { weights.first().copied().unwrap_or(0.0) } else { 0.0 };
            if w != 0.0 {
                self.power_gradient(n, x, w, &mut xb);
            }
        }
        Ok((history.iter().map(|z| vec![0.0; z.len()]).collect(), xb))
    }

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)> {
        let nc = self.solid.nc;
        let rows: Vec<f64> = (0..history.len())
            .flat_map(|n| self.densities(n, x).into_iter().map(|d| d[0] + d[1] + d[2]))
            .collect();
        let grid: Vec<usize> = self.solid.grid.to_vec();
        let terminal = history.len() - 1;
        let history_name = "prescribed_volumetric_heating_density_history_W_m3";
        let terminal_name = "prescribed_volumetric_heating_density_W_m3";
        let mut values = BTreeMap::new();
        values
            .insert(history_name.to_string(), FieldValue::Array(array(rows.clone(), &[history.len(), nc])?));
        values.insert(
            terminal_name.to_string(),
            FieldValue::Array(array(rows[terminal * nc..].to_vec(), &grid)?),
        );
        let times: Vec<Value> =
            self.p["times_s"].as_array().map(|t| t[..history.len()].to_vec()).unwrap_or_default();
        let mut meta = Map::new();
        meta.insert(history_name.into(), json!({"units": "W/m^3", "source": NAME, "association": "exact_prescribed_field_history",
            "axes": ["time", "cell"], "rank": "scalar", "grid": grid, "cell_order": "native_C_order", "times_s": times,
            "meaning": "power per unit cell volume summed over solid endmembers and fluid"}));
        meta.insert(terminal_name.into(), json!({"units": "W/m^3", "source": NAME, "association": "cell", "rank": "scalar",
            "temporal_association": "final_stored_state", "time_index": terminal, "time_s": self.p["times_s"][terminal]}));
        Ok((values, meta))
    }
}

struct VolumetricElement {
    power: [f64; 2],
    fluid: f64,
    attenuation: Option<Attenuation>,
    depth: Vec<f64>,
    scale: f64,
}

impl LocalResidual for VolumetricElement {
    fn residual<S: Scalar>(&self, item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let milli = S::from_f64(1e-3);
        let h: [S; 3] = std::array::from_fn(|a| design[1 + a] * milli);
        let d = material_power_density(design[0], design[4], self.power, self.fluid);
        let density = d[0] + d[1] + d[2];
        let heat = current[4] * attenuation_factor(&h, self.depth[item], self.attenuation) * density;
        let value = -(heat * (h[0] * h[1] * h[2])) / S::from_f64(24.0 * self.scale);
        for v in out.iter_mut().take(4) {
            *v = value;
        }
    }
}

pub struct VolumetricHeatingResidual {
    local: LocalResidualAssembly<VolumetricElement>,
    factor: Vec<f64>,
    elements: usize,
    state_size: usize,
}

impl VolumetricHeatingResidual {
    fn new(owner: &BoundVolumetricHeating, assembly: &CoupledHistoryAssembly) -> CaeResult<Self> {
        let s = &owner.solid;
        let offset = block_start(assembly, "solid")?;
        let rows = thermal_rows(s, offset);
        let current: Vec<i64> = rows.chunks(4).flat_map(|r| r.iter().copied().chain([-1])).collect();
        let m = &s.model;
        let kernel = VolumetricElement {
            power: owner.power,
            fluid: owner.fluid_power,
            attenuation: owner.attenuation,
            depth: s.mesh.owners.iter().map(|o| owner.depth[*o]).collect(),
            scale: m.ks * m.ts * m.ls,
        };
        let local = LocalResidualAssembly::new(
            kernel,
            incidence(s.ne, 4, rows)?,
            incidence(s.ne, 5, current.clone())?,
            incidence(s.ne, 5, current)?,
            incidence(s.ne, 5, design_incidence(s))?,
            assembly.state_size(),
            assembly.design_size(),
            assembly_options(s),
        )?;
        Ok(Self { local, factor: owner.factor.clone(), elements: s.ne, state_size: assembly.state_size() })
    }

    fn data(&self, n: usize) -> Vec<f64> {
        (0..self.elements).flat_map(|_| [0.0, 0.0, 0.0, 0.0, self.factor[n]]).collect()
    }
}

impl HistoryInterface for VolumetricHeatingResidual {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let d = self.data(n);
        self.local.residual(z, old, x, &d, &d)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        if kind != Kind::Design {
            return Ok(Jacobian::Csr(zero_csr(self.state_size, self.state_size)));
        }
        let d = self.data(n);
        Ok(Jacobian::Csr(self.local.jacobian(kind, z, old, x, &d, &d)?))
    }

    fn current_action(
        &self,
        _n: usize,
        _z: &[f64],
        _old: &[f64],
        _x: &[f64],
        _v: &[f64],
        _transpose: bool,
    ) -> Option<CaeResult<Vec<f64>>> {
        Some(Ok(vec![0.0; self.state_size]))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PrescribedVolumetricHeating;

impl HistorySourceComponent for PrescribedVolumetricHeating {
    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value> {
        let context = context
            .downcast_ref::<Value>()
            .ok_or_else(|| err("prescribed volumetric heating context must be a mapping"))?;
        normalise(settings, context)
    }

    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>> {
        let host = host_of(host)?;
        let p = normalise(settings, host.problem())?;
        Ok(Box::new(BoundSource(Arc::new(BoundVolumetricHeating::new(p, host.as_ref())?))))
    }

    fn coupling(&self, base: Value, _settings: &Value) -> CaeResult<Value> {
        extend_coupling(
            &base,
            &[NAME],
            &[(
                NAME,
                "thermal",
                "prescribed_material_resolved_volumetric_power",
                "monolithic",
                "occupancy-, phase- and spacing-dependent prescribed load assembled into the shared temperature residual",
            )],
            &[],
            &limitations(),
        )
    }

    fn owns_material_forcing(&self, _settings: &Value) -> bool {
        false
    }
}

impl FieldSourceAuthoring for PrescribedVolumetricHeating {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value {
        editor_schema(settings, context)
    }

    fn study_templates(&self, context: &Value) -> Vec<Value> {
        study_templates(context)
    }
}
