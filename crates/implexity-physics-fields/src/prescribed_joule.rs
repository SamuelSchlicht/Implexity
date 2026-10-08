// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::{Map, Value, json};

use implexity_ad::Scalar;
use implexity_core::CaeResult;
use implexity_core::contracts::FieldValue;
use implexity_core::history_field_sources::HistorySourceComponent;
use implexity_physics_solid::solid_history::SolidKernel;
use implexity_solve::coupled_history::{CoupledHistoryAssembly, HistoryBlock, HistoryInterface};
use implexity_solve::local_assembly::{Kind, LocalResidual, LocalResidualAssembly};
use implexity_solve::matrix::Jacobian;

use crate::common::{
    assembly_options, design_incidence, err, extend_coupling, grid_of, incidence, py_real, real_array,
    thermal_rows, times_match,
};
use crate::host::{
    BoundFieldSource, BoundSource, FieldHost, FieldSourceAuthoring, ResponseVjp, SourceEnergy, block_start,
    host_of,
};

pub const NAME: &str = "prescribed_real_field_joule_heat";
pub const RESPONSE: &str = "prescribed_joule_power_W";
pub const CONVENTION: &str = "whole_cell_effective_conductivity_no_extra_occupancy";
pub const ENERGY: &str = "authored_heat_excludes_prescribed_joule_heat";
pub const SETTINGS: [&str; 10] = [
    "grid",
    "cell_order",
    "field_binding",
    "times_s",
    "electric_field_V_m",
    "solid_conductivity_S_m",
    "void_conductivity_S_m",
    "penalty",
    "occupancy_convention",
    "energy_convention",
];
const STATE_CONTRACT: &str = "prescribed_zero_state_source_v1";
const DATA: &str = include_str!("data/prescribed_joule.json");

pub(crate) fn data() -> Value {
    serde_json::from_str(DATA).unwrap_or(Value::Null)
}

#[must_use]
pub fn limitations() -> Vec<String> {
    strings(&data()["limitations"])
}

pub(crate) fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}


pub fn power_coefficient(e: &[[f64; 3]], conductivity: f64) -> CaeResult<Vec<f64>> {
    let mut out = Vec::with_capacity(e.len());
    for row in e {
        let scale = row.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        let d = if scale == 0.0 { 1.0 } else { scale };
        let dir: f64 = row.iter().map(|v| (v / d) * (v / d)).sum();
        let power = ((conductivity * scale) * scale) * dir;
        if !power.is_finite() || (power < f64::MIN_POSITIVE && scale > 0.0 && conductivity > 0.0) {
            return Err(err(
                "prescribed Joule power coefficient lies outside finite normal floating-point range",
            ));
        }
        out.push(power);
    }
    Ok(out)
}

fn rows3(data: &[f64]) -> Vec<[f64; 3]> {
    data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect()
}


#[allow(clippy::too_many_lines)]
pub fn normalise(settings: &Value, context: &Value) -> CaeResult<Value> {
    let ok = settings
        .as_object()
        .is_some_and(|m| m.len() == SETTINGS.len() && SETTINGS.iter().all(|k| m.contains_key(*k)));
    let Some(s) = settings.as_object().filter(|_| ok) else {
        return Err(err(
            "prescribed Joule source requires explicit field/grid/time/conductivity and energy conventions",
        ));
    };
    let times = real_array(&s["times_s"], "prescribed Joule times")?;
    let Some(solid) = context.get("solid").filter(|v| v.is_object()) else {
        return Err(err("prescribed Joule source requires a native solid context"));
    };
    let grid = grid_of(&s["grid"]);
    let native = &solid["grid"];
    let same = grid.is_some() && native.as_array().is_some_and(|a| a.len() == 3) && s["grid"] == *native;
    let Some(grid) = grid.filter(|_| same) else {
        return Err(err("prescribed Joule grid must match the native solid grid exactly"));
    };
    let expected = real_array(solid.get("times_s").unwrap_or(&Value::Null), "native solid times")?;
    if !times_match(&times, &expected) {
        return Err(err("prescribed Joule times must match every host time node; no interpolation"));
    }
    if s["cell_order"] != json!("native_C_order")
        || s["field_binding"] != json!("native_cell_attached_values_no_spatial_remapping")
        || s["occupancy_convention"] != json!(CONVENTION)
        || s["energy_convention"] != json!(ENERGY)
    {
        return Err(err("unsupported prescribed Joule cell, occupancy or energy convention"));
    }
    let field = &s["electric_field_V_m"];
    let uniform = field.is_object();
    if uniform {
        let m = field.as_object().map_or(0, Map::len);
        if m != 2 || field.get("values").is_none() || field.get("layout") != Some(&json!("uniform_per_time"))
        {
            return Err(err("compact electric field requires layout uniform_per_time and values [time,3]"));
        }
    }
    let e = real_array(if uniform { &field["values"] } else { field }, "prescribed electric field")?;
    let nt = times.1.len();
    let nc: usize = grid.iter().product();
    let expected_shape = if uniform { vec![nt, 3] } else { vec![nt, nc, 3] };
    if e.0 != expected_shape {
        return Err(err(
            "electric field requires real V/m components: compact uniform_per_time [time,3] or expanded [time,native_cell,3]",
        ));
    }
    let scalar_keys = ["solid_conductivity_S_m", "void_conductivity_S_m", "penalty"];
    let values: Vec<Option<f64>> = scalar_keys.iter().map(|k| py_real(&s[*k])).collect();
    if values.iter().any(Option::is_none) {
        return Err(err("conductivity and penalty must be real numeric scalars"));
    }
    let (a, b, p) = (values[0].unwrap_or(0.0), values[1].unwrap_or(0.0), values[2].unwrap_or(0.0));
    if !(a.is_finite() && b.is_finite() && p.is_finite()) || !(0.0 <= b && b <= a) || p < 1.0 {
        return Err(err("conductivity requires 0 <= void <= solid and finite penalty >= 1"));
    }
    let rows = rows3(&e.1);
    power_coefficient(&rows, a)?;
    power_coefficient(&rows, b)?;
    power_coefficient(&rows, a - b)?;
    let canonical = if uniform {
        json!({"layout": "uniform_per_time", "values": field["values"].clone()})
    } else {
        field.clone()
    };
    let mut out = s.clone();
    out.insert("grid".into(), json!(grid));
    out.insert("times_s".into(), json!(times.1));
    out.insert("electric_field_V_m".into(), canonical_numbers(&canonical));
    out.insert("solid_conductivity_S_m".into(), json!(a));
    out.insert("void_conductivity_S_m".into(), json!(b));
    out.insert("penalty".into(), json!(p));
    Ok(Value::Object(out))
}

fn canonical_numbers(v: &Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64().unwrap_or(f64::NAN)),
        Value::Array(a) => Value::Array(a.iter().map(canonical_numbers).collect()),
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), canonical_numbers(x))).collect()),
        other => other.clone(),
    }
}

fn time_title(i: usize, t: &Value) -> String {
    match t {
        Value::Number(n) => match n.as_f64().filter(|v| f64::is_finite(*v)) {
            Some(v) => format!("Time {} s", implexity_core::extensions::format_g6(v)),
            None => format!("Time node {i}"),
        },
        _ => format!("Time node {i}"),
    }
}

#[must_use]
pub fn editor_schema(settings: &Value, _context: &Value) -> Value {
    let Some(s) = settings.as_object() else { return json!({}) };
    let times: Vec<Value> = s.get("times_s").and_then(Value::as_array).cloned().unwrap_or_default();
    let vector = json!({"type": "array", "minItems": 3, "maxItems": 3, "prefixItems": [
        {"title": "Ex", "units": "V/m", "type": "number"}, {"title": "Ey", "units": "V/m", "type": "number"},
        {"title": "Ez", "units": "V/m", "type": "number"}]});
    let field = s.get("electric_field_V_m").cloned().unwrap_or(Value::Null);
    let uniform = field.get("layout") == Some(&json!("uniform_per_time")) && field.is_object();
    let items: Vec<Value> = times
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut row = if uniform {
                vector.as_object().cloned().unwrap_or_default()
            } else {
                json!({"type": "array", "items": vector.clone(), "description": "One vector per native C-order cell."})
                    .as_object()
                    .cloned()
                    .unwrap_or_default()
            };
            row.insert("title".into(), json!(time_title(i, t)));
            Value::Object(row)
        })
        .collect();
    let history = json!({"title": "Electric-field time history", "units": "V/m", "type": "array", "prefixItems": items});
    let mut electric = json!({"title": "Prescribed electric field", "description": "Instantaneous real vectors; no electrical field solve."});
    if uniform {
        electric["description"] = json!(
            "Uniform in space; one instantaneous vector at every host time node. No electrical field solve."
        );
        electric["properties"] =
            json!({"layout": {"title": "Spatial layout", "enum": ["uniform_per_time"]}, "values": history});
    } else if field.is_array()
        && let (Some(e), Some(h)) = (electric.as_object_mut(), history.as_object())
    {
        {
            for (k, v) in h {
                e.insert(k.clone(), v.clone());
            }
            e.insert(
                "description".into(),
                json!("Spatially resolved [time, native C-order cell, component] values; no electrical field solve."),
            );
        }
    }
    json!({"properties": {
        "solid_conductivity_S_m": {"title": "Solid electrical conductivity", "units": "S/m", "minimum": 0},
        "void_conductivity_S_m": {"title": "Void electrical conductivity", "units": "S/m", "minimum": 0, "description": "Must not exceed solid conductivity."},
        "penalty": {"title": "Conductivity interpolation exponent", "minimum": 1},
        "electric_field_V_m": electric}})
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
    if existing.iter().any(|r| {
        r.get("component") == Some(&json!("native_electromagnetic_loads"))
            && r.get("settings")
                .is_some_and(|s| s.is_object() && s.get("joule_heating") != Some(&json!(false)))
    }) {
        return Vec::new();
    }
    let Some(solid) = ctx.get("solid").filter(|v| v.is_object()) else { return Vec::new() };
    let (grid, times) = (&solid["grid"], &solid["times_s"]);
    if grid_of(grid).is_none() || times.as_array().is_none_or(|a| a.len() < 2) {
        return Vec::new();
    }
    let nt = times.as_array().map_or(0, Vec::len);
    let settings = json!({"grid": grid, "times_s": times, "cell_order": "native_C_order",
        "field_binding": "native_cell_attached_values_no_spatial_remapping",
        "electric_field_V_m": {"layout": "uniform_per_time", "values": vec![[0.0, 0.0, 0.0]; nt]},
        "solid_conductivity_S_m": 0.0, "void_conductivity_S_m": 0.0, "penalty": 1.0,
        "occupancy_convention": CONVENTION, "energy_convention": ENERGY});
    let Ok(settings) = normalise(&settings, context) else { return Vec::new() };
    let d = data();
    let text = &d["study_template_text"][0];
    let label = d["editor_label"].clone();
    let source_schema = json!({"title": label, "properties": {"component": {"title": "Source component", "enum": [NAME]},
        "settings": editor_schema(&settings, context)}});
    let mut prefix: Vec<Value> = existing.iter().map(|_| json!({})).collect();
    prefix.push(source_schema);
    let mut patch = existing.clone();
    patch.push(json!({"component": NAME, "settings": settings}));
    vec![json!({"schema": "implexity-provider-study-template/1", "id": text["id"], "label": text["label"],
        "description": text["description"], "truth_status": text["truth_status"],
        "editor_schema_patch": {"properties": {"field_sources": {"title": "Coupled field sources", "type": "array", "prefixItems": prefix}}},
        "problem_requirements": [{"path": ["solid", "grid"], "value": grid}, {"path": ["solid", "times_s"], "value": times},
            {"path": ["field_sources"], "value": ctx.get("field_sources").cloned().unwrap_or(Value::Null), "missing_equals_null": true}],
        "problem_patch": {"field_sources": patch}})]
}

pub struct BoundJoule {
    p: Value,
    solid: Arc<SolidKernel>,
    pub contrast_power: Vec<Vec<f64>>,
    pub void_power: Vec<Vec<f64>>,
    penalty: f64,
    exchange: OnceLock<Arc<JouleResidual>>,
}

impl BoundJoule {

    pub fn new(p: Value, host: &dyn FieldHost) -> CaeResult<Self> {
        let solid = Arc::clone(host.solid());
        let field = &p["electric_field_V_m"];
        let uniform = field.is_object();
        let values = real_array(if uniform { &field["values"] } else { field }, "prescribed electric field")?;
        let rows = rows3(&values.1);
        let nt = p["times_s"].as_array().map_or(0, Vec::len);
        let nc = solid.nc;
        let coefficient = |sigma: f64| -> CaeResult<Vec<Vec<f64>>> {
            let power = power_coefficient(&rows, sigma)?;
            Ok(if uniform {
                power.iter().map(|v| vec![*v; nc]).collect()
            } else {
                power.chunks(nc).map(<[f64]>::to_vec).collect()
            })
        };
        let a = p["solid_conductivity_S_m"].as_f64().unwrap_or(0.0);
        let b = p["void_conductivity_S_m"].as_f64().unwrap_or(0.0);
        let contrast_power = coefficient(a - b)?;
        let void_power = coefficient(b)?;
        if contrast_power.len() != nt {
            return Err(err("prescribed Joule field history does not match the host times"));
        }
        let penalty = p["penalty"].as_f64().unwrap_or(1.0);
        Ok(Self { p, solid, contrast_power, void_power, penalty, exchange: OnceLock::new() })
    }

    #[must_use]
    pub fn density(&self, n: usize, x: &[f64]) -> Vec<f64> {
        (0..self.solid.nc)
            .map(|c| self.void_power[n][c] + self.contrast_power[n][c] * x[c].powf(self.penalty))
            .collect()
    }

    fn volume(&self, x: &[f64]) -> f64 {
        let nc = self.solid.nc;
        (x[nc] * 1e-3) * (x[nc + 1] * 1e-3) * (x[nc + 2] * 1e-3)
    }

    fn power(&self, n: usize, x: &[f64]) -> f64 {
        self.density(n, x).iter().sum::<f64>() * self.volume(x)
    }
}

impl BoundFieldSource for BoundJoule {
    fn response_units(&self) -> Vec<(String, String)> {
        vec![(RESPONSE.into(), "W".into())]
    }

    fn state_contract(&self) -> Option<&'static str> {
        Some(STATE_CONTRACT)
    }

    fn blocks(&self) -> Vec<HistoryBlock> {
        Vec::new()
    }

    fn attach(&self, assembly: &CoupledHistoryAssembly) -> CaeResult<()> {
        let residual = Arc::new(JouleResidual::new(self, assembly)?);
        assembly.add_interface(Arc::clone(&residual) as Arc<dyn HistoryInterface>)?;
        self.exchange.set(residual).map_err(|_| err("prescribed Joule source attached twice"))
    }

    fn exchange(&self) -> Option<Arc<dyn HistoryInterface>> {
        self.exchange.get().map(|e| Arc::clone(e) as Arc<dyn HistoryInterface>)
    }

    fn energy(&self, n: usize, _z: &[f64], x: &[f64]) -> CaeResult<SourceEnergy> {
        let power = self.power(n, x);
        Ok(SourceEnergy { deposition_w: power, sensible_deposition_w: power, material_production_w: 0.0 })
    }

    fn diagnostics(&self, n: usize, z: &[f64], _old: &[f64], x: &[f64]) -> CaeResult<Map<String, Value>> {
        let e = self.energy(n, z, x)?;
        let values = [e.deposition_w, e.sensible_deposition_w, e.material_production_w];
        if !values.iter().all(|v| f64::is_finite(*v) && *v >= 0.0) {
            return Err(err("invalid prescribed Joule power ledger"));
        }
        let mut m = e.to_map();
        m.insert("scope".into(), json!("prescribed_field_not_electrical_solve"));
        m.insert("sampling".into(), json!("endpoint"));
        m.insert("experimental_qualification_verified".into(), json!(false));
        Ok(m)
    }

    fn responses(&self, history: &[Vec<f64>], x: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(vec![self.power(history.len() - 1, x)])
    }

    fn response_vjp(&self, history: &[Vec<f64>], x: &[f64], weights: &[f64]) -> CaeResult<ResponseVjp> {
        let n = history.len() - 1;
        let nc = self.solid.nc;
        let w = weights.first().copied().unwrap_or(0.0);
        let volume = self.volume(x);
        let total: f64 = self.density(n, x).iter().sum();
        let mut xb = vec![0.0; x.len()];
        for c in 0..nc {
            let d = if self.penalty == 1.0 { 1.0 } else { self.penalty * x[c].powf(self.penalty - 1.0) };
            xb[c] = w * self.contrast_power[n][c] * d * volume;
        }
        for a in 0..3 {
            let others: f64 = (0..3).filter(|b| *b != a).map(|b| x[nc + b] * 1e-3).product();
            xb[nc + a] = w * total * others * 1e-3;
        }
        Ok((history.iter().map(|z| vec![0.0; z.len()]).collect(), xb))
    }

    fn fields(
        &self,
        history: &[Vec<f64>],
        x: &[f64],
    ) -> CaeResult<(BTreeMap<String, FieldValue>, Map<String, Value>)> {
        let nc = self.solid.nc;
        let rows: Vec<f64> = (0..history.len()).flat_map(|n| self.density(n, x)).collect();
        let grid: Vec<usize> = self.solid.grid.to_vec();
        let terminal = history.len() - 1;
        let history_name = "prescribed_joule_density_history_W_m3";
        let terminal_name = "prescribed_joule_density_W_m3";
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
            "axes": ["time", "cell"], "rank": "scalar", "grid": grid, "cell_order": "native_C_order", "times_s": times}));
        meta.insert(terminal_name.into(), json!({"units": "W/m^3", "source": NAME, "association": "cell", "rank": "scalar",
            "temporal_association": "final_stored_state", "time_index": terminal, "time_s": self.p["times_s"][terminal]}));
        Ok((values, meta))
    }
}


pub fn array(values: Vec<f64>, shape: &[usize]) -> CaeResult<ndarray::ArrayD<f64>> {
    ndarray::ArrayD::from_shape_vec(ndarray::IxDyn(shape), values).map_err(|e| err(e.to_string()))
}

struct JouleElement {
    penalty: f64,
    scale: f64,
}

impl LocalResidual for JouleElement {
    fn residual<S: Scalar>(&self, _item: usize, current: &[S], _previous: &[S], design: &[S], out: &mut [S]) {
        let heat = current[5] + current[4] * design[0].powf(self.penalty);
        let milli = S::from_f64(1e-3);
        let volume = design[1] * milli * (design[2] * milli) * (design[3] * milli) / S::from_f64(6.0);
        let value = -(heat * volume) / S::from_f64(4.0 * self.scale);
        for v in out.iter_mut().take(4) {
            *v = value;
        }
    }
}

pub struct JouleResidual {
    local: LocalResidualAssembly<JouleElement>,
    contrast: Vec<Vec<f64>>,
    void: Vec<Vec<f64>>,
    owners: Vec<usize>,
}

impl JouleResidual {
    fn new(owner: &BoundJoule, assembly: &CoupledHistoryAssembly) -> CaeResult<Self> {
        let s = &owner.solid;
        let offset = block_start(assembly, "solid")?;
        let rows = thermal_rows(s, offset);
        let current: Vec<i64> = rows.chunks(4).flat_map(|r| r.iter().copied().chain([-1, -1])).collect();
        let m = &s.model;
        let local = LocalResidualAssembly::new(
            JouleElement { penalty: owner.penalty, scale: m.ks * m.ts * m.ls },
            incidence(s.ne, 4, rows)?,
            incidence(s.ne, 6, current.clone())?,
            incidence(s.ne, 6, current)?,
            incidence(s.ne, 5, design_incidence(s))?,
            assembly.state_size(),
            assembly.design_size(),
            assembly_options(s),
        )?;
        Ok(Self {
            local,
            contrast: owner.contrast_power.clone(),
            void: owner.void_power.clone(),
            owners: s.mesh.owners.clone(),
        })
    }

    fn data(&self, n: usize) -> Vec<f64> {
        self.owners
            .iter()
            .flat_map(|o| [0.0, 0.0, 0.0, 0.0, self.contrast[n][*o], self.void[n][*o]])
            .collect()
    }
}

impl HistoryInterface for JouleResidual {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        let d = self.data(n);
        self.local.residual(z, old, x, &d, &d)
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        let d = self.data(n);
        Ok(Jacobian::Csr(self.local.jacobian(kind, z, old, x, &d, &d)?))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PrescribedJoule;

impl HistorySourceComponent for PrescribedJoule {
    fn validate(&self, settings: &Value, context: &dyn Any) -> CaeResult<Value> {
        let context = context
            .downcast_ref::<Value>()
            .ok_or_else(|| err("prescribed Joule context must be a mapping"))?;
        normalise(settings, context)
    }

    fn create(&self, settings: &Value, host: &dyn Any) -> CaeResult<Box<dyn Any + Send + Sync>> {
        let host = host_of(host)?;
        let p = normalise(settings, host.problem())?;
        Ok(Box::new(BoundSource(Arc::new(BoundJoule::new(p, host.as_ref())?))))
    }

    fn coupling(&self, base: Value, _settings: &Value) -> CaeResult<Value> {
        extend_coupling(
            &base,
            &[NAME],
            &[(
                NAME,
                "thermal",
                "prescribed_real_field_joule_power",
                "monolithic",
                "topology-dependent prescribed-field load assembled into shared temperature residual",
            )],
            &[],
            &limitations(),
        )
    }

    fn owns_material_forcing(&self, _settings: &Value) -> bool {
        false
    }
}

impl FieldSourceAuthoring for PrescribedJoule {
    fn editor_schema(&self, settings: &Value, context: &Value) -> Value {
        editor_schema(settings, context)
    }

    fn study_templates(&self, context: &Value) -> Vec<Value> {
        study_templates(context)
    }
}
