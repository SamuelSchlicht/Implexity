// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Map, Value, json};

use implexity_core::py_repr::{PyValue, repr_str};

use crate::error::{CfdError, CfdResult};

pub const FACES: [&str; 6] = ["x_min", "x_max", "y_min", "y_max", "z_min", "z_max"];

pub const PORT_QUANTITIES: [&str; 3] = ["volume_flow", "mass_flow", "mean_normal_velocity"];

pub const OPEN_KINDS: [&str; 5] = ["velocity", "volume_flow", "mass_flow", "pressure", "traction_outlet"];

pub const KINDS: [&str; 8] = [
    "mass_flow",
    "moving_wall",
    "no_slip",
    "pressure",
    "symmetry",
    "traction_outlet",
    "velocity",
    "volume_flow",
];

pub const MODELS: [&str; 3] =
    ["darcy_forchheimer_reduced", "steady_laminar_navier_stokes_brinkman", "stokes_brinkman"];

pub const PROBLEM_SCHEMA: &str = "implexity-cfd-problem/3";

pub const TOPOLOGY_PARAMETER: &str = "model:control";

pub const ALLOWED_RESPONSES: [&str; 11] = [
    "pressure_drop",
    "pumping_power",
    "dissipation",
    "volume_flow",
    "outlet_uniformity",
    "backflow",
    "brinkman_force",
    "mean_temperature",
    "max_temperature",
    "heat_transfer",
    "wall_shear",
];

#[must_use]
pub fn port_responses() -> Vec<(String, &'static str, &'static str)> {
    let mut out = Vec::with_capacity(18);
    for q in PORT_QUANTITIES {
        for f in FACES {
            out.push((format!("port_{q}_{f}"), f, q));
        }
    }
    out
}

#[must_use]
pub fn port_response(name: &str) -> Option<(&'static str, &'static str)> {
    let rest = name.strip_prefix("port_")?;
    for q in PORT_QUANTITIES {
        if let Some(face) = rest.strip_prefix(q).and_then(|r| r.strip_prefix('_')) {
            return FACES.iter().find(|f| **f == face).map(|f| (*f, q));
        }
    }
    None
}

#[must_use]
pub fn face_normal(face: &str) -> [f64; 3] {
    match face {
        "x_min" => [-1.0, 0.0, 0.0],
        "x_max" => [1.0, 0.0, 0.0],
        "y_min" => [0.0, -1.0, 0.0],
        "y_max" => [0.0, 1.0, 0.0],
        "z_min" => [0.0, 0.0, -1.0],
        _ => [0.0, 0.0, 1.0],
    }
}

#[must_use]
pub fn face_axis(face: &str) -> usize {
    match face.as_bytes().first() {
        Some(b'x') => 0,
        Some(b'y') => 1,
        _ => 2,
    }
}

#[must_use]
pub fn face_is_min(face: &str) -> bool {
    face.ends_with("min")
}

#[must_use]
pub fn is_open_kind(kind: &str) -> bool {
    OPEN_KINDS.contains(&kind)
}

fn py_repr(v: &Value) -> String {
    PyValue::from_json(v).repr()
}

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn py_tuple(v: &Value) -> CfdResult<Value> {
    match v {
        Value::Array(a) => Ok(Value::Array(a.clone())),
        Value::String(s) => Ok(Value::Array(s.chars().map(|c| Value::String(c.to_string())).collect())),
        Value::Object(o) => Ok(Value::Array(o.keys().map(|k| Value::String(k.clone())).collect())),
        other => Err(CfdError::Type(format!("'{}' object is not iterable", py_type(other)))),
    }
}

fn py_iter(v: &Value) -> CfdResult<Vec<Value>> {
    match py_tuple(v)? {
        Value::Array(a) => Ok(a),
        _ => Ok(Vec::new()),
    }
}

fn as_mapping(v: &Value) -> CfdResult<&Map<String, Value>> {
    v.as_object().ok_or_else(|| CfdError::Type(format!("'{}' object is not a mapping", py_type(v))))
}

fn key<'a>(d: &'a Map<String, Value>, k: &str) -> CfdResult<&'a Value> {
    d.get(k).ok_or_else(|| CfdError::Key(repr_str(k)))
}

fn construct(
    class: &str,
    fields: &[(&str, Option<Value>)],
    mapping: &Value,
) -> CfdResult<Map<String, Value>> {
    let Some(given) = mapping.as_object() else {
        return Err(CfdError::Type(format!(
            "implexity.cfd.workspace_contract.{class}() argument after ** must be a mapping, not {}",
            py_type(mapping)
        )));
    };
    if let Some(unknown) = given.keys().find(|k| !fields.iter().any(|(f, _)| f == k)) {
        return Err(CfdError::Type(format!(
            "{class}.__init__() got an unexpected keyword argument {}",
            repr_str(unknown)
        )));
    }
    let missing: Vec<String> = fields
        .iter()
        .filter(|(f, d)| d.is_none() && !given.contains_key(*f))
        .map(|(f, _)| repr_str(f))
        .collect();
    if !missing.is_empty() {
        let noun = if missing.len() == 1 { "argument" } else { "arguments" };
        let list = match missing.len() {
            1 => missing[0].clone(),
            2 => format!("{} and {}", missing[0], missing[1]),
            n => format!("{}, and {}", missing[..n - 1].join(", "), missing[n - 1]),
        };
        return Err(CfdError::Type(format!(
            "{class}.__init__() missing {} required positional {noun}: {list}",
            missing.len()
        )));
    }
    let mut out = Map::new();
    for (f, default) in fields {
        let v = given.get(*f).cloned().or_else(|| default.clone()).unwrap_or(Value::Null);
        out.insert((*f).to_string(), v);
    }
    Ok(out)
}

fn finite(x: &Value, name: &str) -> CfdResult<f64> {
    match x {
        Value::Number(n) => {
            let y = n.as_f64().unwrap_or(f64::NAN);
            if y.is_finite() { Ok(y) } else { Err(CfdError::Input(format!("{name} must be finite"))) }
        }
        _ => Err(CfdError::Input(format!("{name} must be a number"))),
    }
}

fn positive(x: &Value, name: &str) -> CfdResult<f64> {
    let y = finite(x, name)?;
    if y <= 0.0 {
        return Err(CfdError::Input(format!("{name} must be > 0")));
    }
    Ok(y)
}

fn vec3(x: &Value, name: &str) -> CfdResult<[f64; 3]> {
    let items = match x {
        Value::Array(a) if a.len() == 3 => a,
        _ => return Err(CfdError::Input(format!("{name} must have three components"))),
    };
    Ok([
        finite(&items[0], &format!("{name}[0]"))?,
        finite(&items[1], &format!("{name}[1]"))?,
        finite(&items[2], &format!("{name}[2]"))?,
    ])
}

fn py_int(x: &Value) -> CfdResult<i64> {
    match x {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i)
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_finite() {

                    #[allow(clippy::cast_possible_truncation)]
                    let t = f.trunc() as i64;
                    Ok(t)
                } else {
                    Err(CfdError::Type("cannot convert float to integer".into()))
                }
            }
        }
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| CfdError::Input(format!("invalid literal for int() with base 10: {}", repr_str(s)))),
        other => Err(CfdError::Type(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            py_type(other)
        ))),
    }
}

fn string_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

fn opt_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fluid {
    pub density_kg_m3: f64,
    pub dynamic_viscosity_pa_s: f64,
    pub heat_capacity_j_kg_k: Option<f64>,
    pub thermal_conductivity_w_m_k: Option<f64>,
    pub reference_temperature_k: f64,
    pub name: Value,
    raw: Map<String, Value>,
}

fn fluid_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("density_kg_m3", None),
        ("dynamic_viscosity_Pa_s", None),
        ("heat_capacity_J_kg_K", Some(Value::Null)),
        ("thermal_conductivity_W_m_K", Some(Value::Null)),
        ("reference_temperature_K", Some(json!(293.15))),
        ("name", Some(json!("Newtonian fluid"))),
    ]
}

impl Fluid {
    fn validate_raw(raw: &Map<String, Value>, require_thermal: bool) -> CfdResult<Self> {
        let density = positive(&raw["density_kg_m3"], "fluid density [kg/m^3]")?;
        let mu = positive(&raw["dynamic_viscosity_Pa_s"], "dynamic viscosity [Pa s]")?;
        let tref = positive(&raw["reference_temperature_K"], "reference temperature [K]")?;
        let (cp, k) = if require_thermal {
            (
                Some(positive(&raw["heat_capacity_J_kg_K"], "heat capacity [J/(kg K)]")?),
                Some(positive(&raw["thermal_conductivity_W_m_K"], "thermal conductivity [W/(m K)]")?),
            )
        } else {
            let cp = match &raw["heat_capacity_J_kg_K"] {
                Value::Null => None,
                v => Some(positive(v, "heat capacity [J/(kg K)]")?),
            };
            let k = match &raw["thermal_conductivity_W_m_K"] {
                Value::Null => None,
                v => Some(positive(v, "thermal conductivity [W/(m K)]")?),
            };
            (cp, k)
        };
        Ok(Self {
            density_kg_m3: density,
            dynamic_viscosity_pa_s: mu,
            heat_capacity_j_kg_k: cp,
            thermal_conductivity_w_m_k: k,
            reference_temperature_k: tref,
            name: raw["name"].clone(),
            raw: raw.clone(),
        })
    }


    pub fn new(
        density_kg_m3: f64,
        dynamic_viscosity_pa_s: f64,
        heat_capacity_j_kg_k: Option<f64>,
        thermal_conductivity_w_m_k: Option<f64>,
        reference_temperature_k: f64,
        name: Value,
        require_thermal: bool,
    ) -> CfdResult<Self> {
        let checks = [
            (Some(density_kg_m3), "fluid density [kg/m^3]"),
            (Some(dynamic_viscosity_pa_s), "dynamic viscosity [Pa s]"),
            (Some(reference_temperature_k), "reference temperature [K]"),
            (heat_capacity_j_kg_k, "heat capacity [J/(kg K)]"),
            (thermal_conductivity_w_m_k, "thermal conductivity [W/(m K)]"),
        ];
        for (value, name) in checks {
            if value.is_some_and(|v| !v.is_finite()) {
                return Err(CfdError::Input(format!("{name} must be finite")));
            }
        }
        let mut raw = Map::new();
        raw.insert("density_kg_m3".into(), json!(density_kg_m3));
        raw.insert("dynamic_viscosity_Pa_s".into(), json!(dynamic_viscosity_pa_s));
        raw.insert("heat_capacity_J_kg_K".into(), json!(heat_capacity_j_kg_k));
        raw.insert("thermal_conductivity_W_m_K".into(), json!(thermal_conductivity_w_m_k));
        raw.insert("reference_temperature_K".into(), json!(reference_temperature_k));
        raw.insert("name".into(), name);
        Self::validate_raw(&raw, require_thermal)
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    pub origin_m: [f64; 3],
    pub extent_m: [f64; 3],
    pub cells: [usize; 3],
    pub reference_length_m: f64,
    pub reference_velocity_m_s: f64,
    pub gravity_m_s2: [f64; 3],
    raw: Map<String, Value>,
}

fn domain_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("origin_m", None),
        ("extent_m", None),
        ("cells", None),
        ("reference_length_m", None),
        ("reference_velocity_m_s", None),
        ("gravity_m_s2", Some(json!([0.0, 0.0, 0.0]))),
    ]
}

impl Domain {
    fn validate_raw(raw: &Map<String, Value>) -> CfdResult<Self> {
        let origin = vec3(&raw["origin_m"], "domain origin [m]")?;
        let extent = vec3(&raw["extent_m"], "domain extent [m]")?;
        if extent.iter().any(|v| *v <= 0.0) {
            return Err(CfdError::Input("all domain extents must be > 0 m".into()));
        }
        let cells_v = match &raw["cells"] {
            Value::Array(a) if a.len() == 3 => a,
            _ => return Err(CfdError::Input("the 3D CFD grid needs three whole cell counts".into())),
        };
        let mut cells = [0usize; 3];
        for (slot, value) in cells.iter_mut().zip(cells_v) {
            let count = finite(value, "domain cell count")?;
            if count < 2.0 || count.fract() != 0.0 {
                return Err(CfdError::Input(
                    "the 3D CFD grid needs at least two whole cells per direction".into(),
                ));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let c = count as usize;
            *slot = c;
        }
        let lref = positive(&raw["reference_length_m"], "reference length [m]")?;
        let uref = positive(&raw["reference_velocity_m_s"], "reference velocity [m/s]")?;
        let gravity = vec3(&raw["gravity_m_s2"], "gravity [m/s^2]")?;
        Ok(Self {
            origin_m: origin,
            extent_m: extent,
            cells,
            reference_length_m: lref,
            reference_velocity_m_s: uref,
            gravity_m_s2: gravity,
            raw: raw.clone(),
        })
    }

    #[must_use]
    pub fn spacing_m(&self) -> [f64; 3] {
        [
            self.extent_m[0] / self.cells[0] as f64,
            self.extent_m[1] / self.cells[1] as f64,
            self.extent_m[2] / self.cells[2] as f64,
        ]
    }

    #[must_use]
    pub fn face_area_m2(&self, face: &str) -> f64 {
        let [lx, ly, lz] = self.extent_m;
        match face_axis(face) {
            0 => ly * lz,
            1 => lx * lz,
            _ => lx * ly,
        }
    }

    #[must_use]
    pub fn n_cells(&self) -> usize {
        self.cells[0] * self.cells[1] * self.cells[2]
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }

    #[must_use]
    pub fn cells_repr(&self) -> String {
        match &self.raw["cells"] {
            Value::Array(a) => PyValue::Tuple(a.iter().map(PyValue::from_json).collect()).repr(),
            other => py_repr(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryCondition {
    pub face: String,
    pub kind: String,
    pub velocity_m_s: Option<[f64; 3]>,
    pub static_pressure_pa: Option<f64>,
    pub volume_flow_m3_s: Option<f64>,
    pub mass_flow_kg_s: Option<f64>,
    pub port_buffer_cells: usize,
    pub label: Value,
    pub enabled: bool,
    raw: Map<String, Value>,
}

fn boundary_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("face", None),
        ("kind", None),
        ("velocity_m_s", Some(Value::Null)),
        ("static_pressure_Pa", Some(Value::Null)),
        ("volume_flow_m3_s", Some(Value::Null)),
        ("mass_flow_kg_s", Some(Value::Null)),
        ("port_buffer_cells", Some(json!(2))),
        ("label", Some(json!(""))),
        ("enabled", Some(json!(true))),
    ]
}

impl BoundaryCondition {
    fn lenient(raw: &Map<String, Value>) -> Self {
        let velocity = raw["velocity_m_s"].as_array().filter(|a| a.len() == 3).map(|a| {
            [
                a[0].as_f64().unwrap_or(f64::NAN),
                a[1].as_f64().unwrap_or(f64::NAN),
                a[2].as_f64().unwrap_or(f64::NAN),
            ]
        });
        let layers = raw["port_buffer_cells"].as_f64().filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(0.0);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let layers = layers as usize;
        Self {
            face: string_of(&raw["face"]),
            kind: string_of(&raw["kind"]),
            velocity_m_s: velocity,
            static_pressure_pa: opt_f64(&raw["static_pressure_Pa"]),
            volume_flow_m3_s: opt_f64(&raw["volume_flow_m3_s"]),
            mass_flow_kg_s: opt_f64(&raw["mass_flow_kg_s"]),
            port_buffer_cells: layers,
            label: raw["label"].clone(),
            enabled: truthy(&raw["enabled"]),
            raw: raw.clone(),
        }
    }

    fn validate_raw(raw: &Map<String, Value>) -> CfdResult<Self> {
        let face = match &raw["face"] {
            Value::String(s) if FACES.contains(&s.as_str()) => s.clone(),
            other => return Err(CfdError::Input(format!("unknown domain face {}", py_repr(other)))),
        };
        let kind = match &raw["kind"] {
            Value::String(s) if KINDS.contains(&s.as_str()) => s.clone(),
            other => return Err(CfdError::Input(format!("unsupported boundary kind {}", py_repr(other)))),
        };
        let layers = finite(&raw["port_buffer_cells"], "protected fluid layers")?;
        if layers < 0.0 || layers.fract() != 0.0 {
            return Err(CfdError::Input(
                "protected fluid layers must be a whole number, zero or greater".into(),
            ));
        }
        let present = |k: &str| !raw[k].is_null();
        let exactly = match kind.as_str() {
            "velocity" | "moving_wall" => Some(present("velocity_m_s")),
            "pressure" => Some(present("static_pressure_Pa")),
            "volume_flow" => Some(present("volume_flow_m3_s")),
            "mass_flow" => Some(present("mass_flow_kg_s")),
            _ => None,
        };
        if exactly == Some(false) {
            return Err(CfdError::Input(format!("{kind} boundary on {face} is missing its required value")));
        }
        let velocity = if present("velocity_m_s") {
            Some(vec3(&raw["velocity_m_s"], "boundary velocity [m/s]")?)
        } else {
            None
        };
        let static_pressure = if present("static_pressure_Pa") {
            Some(finite(&raw["static_pressure_Pa"], "static pressure [Pa]")?)
        } else {
            None
        };
        let volume_flow = if present("volume_flow_m3_s") {
            Some(finite(&raw["volume_flow_m3_s"], "volume flow [m^3/s]")?)
        } else {
            None
        };
        let mass_flow = if present("mass_flow_kg_s") {
            Some(finite(&raw["mass_flow_kg_s"], "mass flow [kg/s]")?)
        } else {
            None
        };
        let supplied = ["velocity_m_s", "static_pressure_Pa", "volume_flow_m3_s", "mass_flow_kg_s"]
            .iter()
            .filter(|k| present(k))
            .count();
        if matches!(kind.as_str(), "no_slip" | "symmetry" | "traction_outlet") && supplied > 0 {
            return Err(CfdError::Input(format!(
                "{kind} boundary on {face} must not carry a velocity, pressure, or flow value"
            )));
        }
        if exactly.is_some() && supplied != 1 {
            return Err(CfdError::Input(format!(
                "{kind} boundary on {face} must define exactly its one physical quantity"
            )));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let layers = layers as usize;
        Ok(Self {
            face,
            kind,
            velocity_m_s: velocity,
            static_pressure_pa: static_pressure,
            volume_flow_m3_s: volume_flow,
            mass_flow_kg_s: mass_flow,
            port_buffer_cells: layers,
            label: raw["label"].clone(),
            enabled: true,
            raw: raw.clone(),
        })
    }

    #[must_use]
    pub fn inward_volume_flow_m3_s(&self, domain: &Domain, fluid: &Fluid) -> Option<f64> {
        let area = domain.face_area_m2(&self.face);
        let n = face_normal(&self.face);
        match self.kind.as_str() {
            "velocity" | "moving_wall" => {
                let v = self.velocity_m_s.unwrap_or([0.0; 3]);
                let dot = v[0] * n[0] + v[1] * n[1] + v[2] * n[2];
                Some(-dot * area)
            }
            "volume_flow" => self.volume_flow_m3_s,
            "mass_flow" => self.mass_flow_kg_s.map(|m| m / fluid.density_kg_m3),
            _ => None,
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.enabled && is_open_kind(&self.kind)
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }

    #[must_use]
    pub fn raw(&self) -> &Map<String, Value> {
        &self.raw
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PressureGauge {
    pub mode: String,
    pub cell: Option<[usize; 3]>,
    pub value_pa: f64,
    raw: Map<String, Value>,
}

fn gauge_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![("mode", Some(json!("mean_zero"))), ("cell", Some(Value::Null)), ("value_Pa", Some(json!(0.0)))]
}

fn py_len(v: &Value) -> CfdResult<usize> {
    match v {
        Value::Array(a) => Ok(a.len()),
        Value::String(s) => Ok(s.chars().count()),
        Value::Object(o) => Ok(o.len()),
        other => Err(CfdError::Type(format!("object of type '{}' has no len()", py_type(other)))),
    }
}

impl PressureGauge {
    fn validate_raw(raw: &Map<String, Value>, domain: &Domain) -> CfdResult<Self> {
        let mode = match &raw["mode"] {
            Value::String(s) if s == "mean_zero" || s == "cell" => s.clone(),
            _ => return Err(CfdError::Input("pressure gauge mode must be mean_zero or cell".into())),
        };
        let value = finite(&raw["value_Pa"], "pressure gauge value [Pa]")?;
        let mut cell = None;
        if mode == "cell" {
            let c = &raw["cell"];
            if c.is_null() || py_len(c)? != 3 {
                return Err(CfdError::Input("cell pressure gauge requires a cell index".into()));
            }
            let items = py_iter(c)?;
            let mut idx = [0usize; 3];
            for ((slot, item), n) in idx.iter_mut().zip(&items).zip(domain.cells) {
                let i = py_int(item)?;
                let inside = usize::try_from(i).ok().filter(|u| *u < n);
                match inside {
                    Some(u) => *slot = u,
                    None => {
                        return Err(CfdError::Input("pressure gauge cell lies outside the CFD grid".into()));
                    }
                }
            }
            cell = Some(idx);
        }
        Ok(Self { mode, cell, value_pa: value, raw: raw.clone() })
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Brinkman {
    pub fluid_permeability_m2: f64,
    pub solid_permeability_m2: f64,
    pub ramp_q: f64,
    pub continuation: Vec<f64>,
    pub minimum_fluid_fraction: f64,
    raw: Map<String, Value>,
}

fn brinkman_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("fluid_permeability_m2", Some(json!(1e6))),
        ("solid_permeability_m2", Some(json!(1e-12))),
        ("ramp_q", Some(json!(8.0))),
        ("continuation", Some(json!([0.02, 0.1, 0.35, 1.0]))),
        ("minimum_fluid_fraction", Some(json!(1e-4))),
    ]
}

impl Brinkman {
    fn validate_raw(raw: &Map<String, Value>) -> CfdResult<Self> {
        let kf = positive(&raw["fluid_permeability_m2"], "fluid permeability [m^2]")?;
        let ks = positive(&raw["solid_permeability_m2"], "solid permeability [m^2]")?;
        if ks >= kf {
            return Err(CfdError::Input("solid permeability must be smaller than fluid permeability".into()));
        }
        let q = finite(&raw["ramp_q"], "Brinkman RAMP q")?;
        if q < 0.0 {
            return Err(CfdError::Input("Brinkman RAMP q must be >= 0".into()));
        }
        let items = py_iter(&raw["continuation"])?;
        if items.is_empty() {
            return Err(CfdError::Input("Brinkman continuation must contain at least one factor".into()));
        }
        let mut prev = 0.0;
        let mut continuation = Vec::with_capacity(items.len());
        for (i, c) in items.iter().enumerate() {
            let c = positive(c, &format!("continuation[{i}]"))?;
            if c < prev || c > 1.0 {
                return Err(CfdError::Input("Brinkman continuation must be nondecreasing and <= 1".into()));
            }
            prev = c;
            continuation.push(c);
        }
        if (prev - 1.0).abs() > 1e-12 {
            return Err(CfdError::Input("Brinkman continuation must end at 1".into()));
        }
        let mff = match &raw["minimum_fluid_fraction"] {
            Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
            Value::Bool(b) => f64::from(u8::from(*b)),
            other => {
                return Err(CfdError::Type(format!(
                    "'<=' not supported between instances of 'int' and '{}'",
                    py_type(other)
                )));
            }
        };
        if !(0.0..0.5).contains(&mff) {
            return Err(CfdError::Input("minimum fluid fraction must lie in [0,0.5)".into()));
        }
        Ok(Self {
            fluid_permeability_m2: kf,
            solid_permeability_m2: ks,
            ramp_q: q,
            continuation,
            minimum_fluid_fraction: mff,
            raw: raw.clone(),
        })
    }


    pub fn new(
        fluid_permeability_m2: f64,
        solid_permeability_m2: f64,
        ramp_q: f64,
        continuation: &[f64],
        minimum_fluid_fraction: f64,
    ) -> CfdResult<Self> {
        let finite_or = |x: f64, name: &str| {
            if x.is_finite() { Ok(json!(x)) } else { Err(CfdError::Input(format!("{name} must be finite"))) }
        };
        let num = |x: f64| json!(x);
        finite_or(fluid_permeability_m2, "fluid permeability [m^2]")?;
        finite_or(solid_permeability_m2, "solid permeability [m^2]")?;
        finite_or(ramp_q, "Brinkman RAMP q")?;
        for (i, c) in continuation.iter().enumerate() {
            finite_or(*c, &format!("continuation[{i}]"))?;
        }
        let mut raw = Map::new();
        raw.insert("fluid_permeability_m2".into(), num(fluid_permeability_m2));
        raw.insert("solid_permeability_m2".into(), num(solid_permeability_m2));
        raw.insert("ramp_q".into(), num(ramp_q));
        raw.insert("continuation".into(), Value::Array(continuation.iter().map(|c| num(*c)).collect()));
        raw.insert("minimum_fluid_fraction".into(), num(minimum_fluid_fraction));
        Self::validate_raw(&raw)
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SolverSettings {
    pub model: String,
    pub convection_scheme: String,
    pub nonlinear_tolerance: f64,
    pub linear_tolerance: f64,
    pub adjoint_tolerance: f64,
    pub maximum_nonlinear_iterations: usize,
    pub maximum_linear_iterations: usize,
    pub pressure_stabilization: f64,
    pub use_matrix_free: Value,
    raw: Map<String, Value>,
}

fn solver_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("model", Some(json!("stokes_brinkman"))),
        ("convection_scheme", Some(json!("smooth_upwind"))),
        ("nonlinear_tolerance", Some(json!(1e-8))),
        ("linear_tolerance", Some(json!(1e-10))),
        ("adjoint_tolerance", Some(json!(1e-9))),
        ("maximum_nonlinear_iterations", Some(json!(50))),
        ("maximum_linear_iterations", Some(json!(800))),
        ("pressure_stabilization", Some(json!(0.0))),
        ("use_matrix_free", Some(json!(true))),
    ]
}

impl SolverSettings {
    fn validate_raw(raw: &Map<String, Value>) -> CfdResult<Self> {
        let model = match &raw["model"] {
            Value::String(s) if MODELS.contains(&s.as_str()) => s.clone(),
            other => return Err(CfdError::Input(format!("unsupported flow model {}", py_repr(other)))),
        };
        let scheme = match &raw["convection_scheme"] {
            Value::String(s) if s == "central" || s == "smooth_upwind" => s.clone(),
            _ => return Err(CfdError::Input("convection scheme must be central or smooth_upwind".into())),
        };
        let nl = positive(&raw["nonlinear_tolerance"], "nonlinear tolerance")?;
        let lin = positive(&raw["linear_tolerance"], "linear tolerance")?;
        let adj = positive(&raw["adjoint_tolerance"], "adjoint tolerance")?;
        let mut counts = [0usize; 2];
        for (slot, k) in counts.iter_mut().zip(["maximum_nonlinear_iterations", "maximum_linear_iterations"])
        {
            let count = finite(&raw[k], "solver iteration limit")?;
            if count < 1.0 || count.fract() != 0.0 {
                return Err(CfdError::Input("solver iteration limits must be positive whole numbers".into()));
            }
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let c = count as usize;
            *slot = c;
        }
        let stab = finite(&raw["pressure_stabilization"], "pressure stabilization")?;
        if stab < 0.0 {
            return Err(CfdError::Input("pressure stabilization must be >= 0".into()));
        }
        Ok(Self {
            model,
            convection_scheme: scheme,
            nonlinear_tolerance: nl,
            linear_tolerance: lin,
            adjoint_tolerance: adj,
            maximum_nonlinear_iterations: counts[0],
            maximum_linear_iterations: counts[1],
            pressure_stabilization: stab,
            use_matrix_free: raw["use_matrix_free"].clone(),
            raw: raw.clone(),
        })
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    pub response: String,
    pub sense: String,
    pub weight: f64,
    pub scale: f64,
    pub target: Option<f64>,
    pub inlet_face: Option<String>,
    pub outlet_face: Option<String>,
    pub region_id: Option<Value>,
    raw: Map<String, Value>,
}

fn objective_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("response", None),
        ("sense", Some(json!("minimize"))),
        ("weight", Some(json!(1.0))),
        ("scale", Some(json!(1.0))),
        ("target", Some(Value::Null)),
        ("inlet_face", Some(Value::Null)),
        ("outlet_face", Some(Value::Null)),
        ("region_id", Some(Value::Null)),
    ]
}

impl Objective {
    fn validate_raw(raw: &Map<String, Value>) -> CfdResult<Self> {
        let response = match &raw["response"] {
            Value::String(s) if ALLOWED_RESPONSES.contains(&s.as_str()) || port_response(s).is_some() => {
                s.clone()
            }
            other => return Err(CfdError::Input(format!("unknown CFD response {}", py_repr(other)))),
        };
        if port_response(&response).is_some()
            && ["inlet_face", "outlet_face", "region_id"].iter().any(|k| !raw[*k].is_null())
        {
            return Err(CfdError::Input(
                "named port response already identifies its whole domain face; omit alternate face/region selectors"
                    .into(),
            ));
        }
        let sense = match &raw["sense"] {
            Value::String(s)
                if ["minimize", "maximize", "target", "upper", "lower", "equal"].contains(&s.as_str()) =>
            {
                s.clone()
            }
            _ => return Err(CfdError::Input("invalid objective sense".into())),
        };
        let weight = finite(&raw["weight"], "objective weight")?;
        let scale = positive(&raw["scale"], "objective scale")?;
        if ["target", "upper", "lower", "equal"].contains(&sense.as_str()) && raw["target"].is_null() {
            return Err(CfdError::Input(format!("{sense} response requires a target")));
        }
        let target =
            if raw["target"].is_null() { None } else { Some(finite(&raw["target"], "objective target")?) };
        let mut faces = [None, None];
        for (slot, k) in faces.iter_mut().zip(["inlet_face", "outlet_face"]) {
            match &raw[k] {
                Value::Null => {}
                Value::String(s) if FACES.contains(&s.as_str()) => *slot = Some(s.clone()),
                other => return Err(CfdError::Input(format!("unknown objective face {}", py_repr(other)))),
            }
        }
        let [inlet, outlet] = faces;
        Ok(Self {
            response,
            sense,
            weight,
            scale,
            target,
            inlet_face: inlet,
            outlet_face: outlet,
            region_id: if raw["region_id"].is_null() { None } else { Some(raw["region_id"].clone()) },
            raw: raw.clone(),
        })
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalSettings {
    pub enabled: bool,
    pub solid_conductivity_w_m_k: Option<f64>,
    pub solid_heat_capacity_j_kg_k: Option<f64>,
    pub solid_density_kg_m3: Option<f64>,
    pub volumetric_heat_w_m3: f64,
    pub inlet_temperature_k: Option<f64>,
    pub wall_temperature_k: Option<f64>,
    pub wall_heat_flux_w_m2: Option<f64>,
    raw: Map<String, Value>,
}

fn thermal_fields() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("enabled", Some(json!(false))),
        ("solid_conductivity_W_m_K", Some(Value::Null)),
        ("solid_heat_capacity_J_kg_K", Some(Value::Null)),
        ("solid_density_kg_m3", Some(Value::Null)),
        ("volumetric_heat_W_m3", Some(json!(0.0))),
        ("inlet_temperature_K", Some(Value::Null)),
        ("wall_temperature_K", Some(Value::Null)),
        ("wall_heat_flux_W_m2", Some(Value::Null)),
    ]
}

impl ThermalSettings {
    fn lenient(raw: &Map<String, Value>) -> Self {
        Self {
            enabled: truthy(&raw["enabled"]),
            solid_conductivity_w_m_k: opt_f64(&raw["solid_conductivity_W_m_K"]),
            solid_heat_capacity_j_kg_k: opt_f64(&raw["solid_heat_capacity_J_kg_K"]),
            solid_density_kg_m3: opt_f64(&raw["solid_density_kg_m3"]),
            volumetric_heat_w_m3: raw["volumetric_heat_W_m3"].as_f64().unwrap_or(0.0),
            inlet_temperature_k: opt_f64(&raw["inlet_temperature_K"]),
            wall_temperature_k: opt_f64(&raw["wall_temperature_K"]),
            wall_heat_flux_w_m2: opt_f64(&raw["wall_heat_flux_W_m2"]),
            raw: raw.clone(),
        }
    }

    fn validate_raw(raw: &Map<String, Value>, fluid_raw: &Map<String, Value>) -> CfdResult<Self> {
        if !truthy(&raw["enabled"]) {
            return Ok(Self::lenient(raw));
        }
        Fluid::validate_raw(fluid_raw, true)?;
        let k = positive(&raw["solid_conductivity_W_m_K"], "solid conductivity [W/(m K)]")?;
        let cp = positive(&raw["solid_heat_capacity_J_kg_K"], "solid heat capacity [J/(kg K)]")?;
        let rho = positive(&raw["solid_density_kg_m3"], "solid density [kg/m^3]")?;
        let q = finite(&raw["volumetric_heat_W_m3"], "volumetric heat [W/m^3]")?;
        if raw["inlet_temperature_K"].is_null() {
            return Err(CfdError::Input("thermal flow requires an inlet temperature".into()));
        }
        let tin = positive(&raw["inlet_temperature_K"], "inlet temperature [K]")?;
        let tw = if raw["wall_temperature_K"].is_null() {
            None
        } else {
            Some(positive(&raw["wall_temperature_K"], "wall temperature [K]")?)
        };
        let qw = if raw["wall_heat_flux_W_m2"].is_null() {
            None
        } else {
            Some(finite(&raw["wall_heat_flux_W_m2"], "wall heat flux [W/m^2]")?)
        };
        if tw.is_some() && qw.is_some() {
            return Err(CfdError::Input("choose wall temperature or wall heat flux, not both".into()));
        }
        Ok(Self {
            enabled: true,
            solid_conductivity_w_m_k: Some(k),
            solid_heat_capacity_j_kg_k: Some(cp),
            solid_density_kg_m3: Some(rho),
            volumetric_heat_w_m3: q,
            inlet_temperature_k: Some(tin),
            wall_temperature_k: tw,
            wall_heat_flux_w_m2: qw,
            raw: raw.clone(),
        })
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        Value::Object(self.raw.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CfdProblem {
    pub fluid: Fluid,
    pub domain: Domain,
    pub boundaries: Vec<BoundaryCondition>,
    pub gauge: PressureGauge,
    pub brinkman: Brinkman,
    pub solver: SolverSettings,
    pub objectives: Vec<Objective>,
    pub thermal: ThermalSettings,
    pub topology_parameter: String,
    pub schema: String,
}

impl CfdProblem {
    pub fn enabled_boundaries(&self) -> impl Iterator<Item = &BoundaryCondition> {
        self.boundaries.iter().filter(|b| b.enabled)
    }

    #[must_use]
    pub fn boundary_on(&self, face: &str) -> Option<&BoundaryCondition> {
        self.enabled_boundaries().find(|b| b.face == face)
    }


    pub fn validate(&self) -> CfdResult<&Self> {
        Ok(self)
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        let mut m = Map::new();
        m.insert("fluid".into(), self.fluid.as_dict());
        m.insert("domain".into(), self.domain.as_dict());
        m.insert(
            "boundaries".into(),
            Value::Array(self.boundaries.iter().map(BoundaryCondition::as_dict).collect()),
        );
        m.insert("gauge".into(), self.gauge.as_dict());
        m.insert("brinkman".into(), self.brinkman.as_dict());
        m.insert("solver".into(), self.solver.as_dict());
        m.insert("objectives".into(), Value::Array(self.objectives.iter().map(Objective::as_dict).collect()));
        m.insert("thermal".into(), self.thermal.as_dict());
        m.insert("topology_parameter".into(), Value::String(self.topology_parameter.clone()));
        m.insert("schema".into(), Value::String(self.schema.clone()));
        Value::Object(m)
    }
}


pub fn from_mapping(data: &Value) -> CfdResult<CfdProblem> {
    let d = match data {
        Value::Object(o) => o,
        other => return Err(CfdError::Type(format!("'{}' object is not iterable", py_type(other)))),
    };
    let fluid_raw = construct("Fluid", &fluid_fields(), key(d, "fluid")?)?;

    let dom = key(d, "domain")?;
    let dom_map = as_mapping(dom)?;
    let mut dom_args = dom_map.clone();
    dom_args.insert("origin_m".into(), py_tuple(key(dom_map, "origin_m")?)?);
    dom_args.insert("extent_m".into(), py_tuple(key(dom_map, "extent_m")?)?);
    dom_args.insert("cells".into(), py_tuple(key(dom_map, "cells")?)?);
    let gravity = dom_map.get("gravity_m_s2").cloned().unwrap_or_else(|| json!([0, 0, 0]));
    dom_args.insert("gravity_m_s2".into(), py_tuple(&gravity)?);
    let domain_raw = construct("Domain", &domain_fields(), &Value::Object(dom_args))?;

    let mut boundary_raws = Vec::new();
    for b in py_iter(key(d, "boundaries")?)? {
        let bm = as_mapping(&b)?;
        let mut args = bm.clone();
        let velocity = match bm.get("velocity_m_s") {
            Some(v) if !v.is_null() => py_tuple(v)?,
            _ => Value::Null,
        };
        args.insert("velocity_m_s".into(), velocity);
        boundary_raws.push(construct("BoundaryCondition", &boundary_fields(), &Value::Object(args))?);
    }

    let gauge_raw = construct("PressureGauge", &gauge_fields(), d.get("gauge").unwrap_or(&json!({})))?;

    let brink = d.get("brinkman").cloned().unwrap_or_else(|| json!({}));
    let brink_map = as_mapping(&brink)?;
    let mut brink_args = brink_map.clone();
    let continuation =
        brink_map.get("continuation").cloned().unwrap_or_else(|| json!([0.02, 0.1, 0.35, 1.0]));
    brink_args.insert("continuation".into(), py_tuple(&continuation)?);
    let brinkman_raw = construct("Brinkman", &brinkman_fields(), &Value::Object(brink_args))?;

    let solver_raw = construct("SolverSettings", &solver_fields(), d.get("solver").unwrap_or(&json!({})))?;

    let objectives_v = d.get("objectives").cloned().unwrap_or_else(|| json!([{"response": "pumping_power"}]));
    let mut objective_raws = Vec::new();
    for o in py_iter(&objectives_v)? {
        objective_raws.push(construct("Objective", &objective_fields(), &o)?);
    }

    let thermal_raw =
        construct("ThermalSettings", &thermal_fields(), d.get("thermal").unwrap_or(&json!({})))?;
    let topology_parameter =
        d.get("topology_parameter").cloned().unwrap_or_else(|| json!(TOPOLOGY_PARAMETER));
    let schema = d.get("schema").cloned().unwrap_or_else(|| json!(PROBLEM_SCHEMA));

    if schema.as_str() != Some(PROBLEM_SCHEMA) {
        return Err(CfdError::Input("unsupported CFD problem schema".into()));
    }
    let thermal_enabled = truthy(&thermal_raw["enabled"]);
    let fluid = Fluid::validate_raw(&fluid_raw, thermal_enabled)?;
    let domain = Domain::validate_raw(&domain_raw)?;
    let gauge = PressureGauge::validate_raw(&gauge_raw, &domain)?;
    let brinkman = Brinkman::validate_raw(&brinkman_raw)?;
    let solver = SolverSettings::validate_raw(&solver_raw)?;
    let thermal = ThermalSettings::validate_raw(&thermal_raw, &fluid_raw)?;
    if topology_parameter.as_str() != Some(TOPOLOGY_PARAMETER) {
        return Err(CfdError::Input(
            "resolved CFD must use the universal topology coordinate model:control".into(),
        ));
    }
    let mut boundaries = Vec::with_capacity(boundary_raws.len());
    let mut seen: Vec<String> = Vec::new();
    for raw in &boundary_raws {
        if !truthy(&raw["enabled"]) {
            boundaries.push(BoundaryCondition::lenient(raw));
            continue;
        }
        let b = BoundaryCondition::validate_raw(raw)?;
        if seen.contains(&b.face) {
            return Err(CfdError::Input(format!(
                "domain face {} has more than one enabled boundary condition",
                b.face
            )));
        }
        seen.push(b.face.clone());
        boundaries.push(b);
    }
    if seen.len() != FACES.len() {
        return Err(CfdError::Input(
            "all six domain faces must be assigned explicitly; use no_slip for closed faces".into(),
        ));
    }
    let mut objectives = Vec::with_capacity(objective_raws.len());
    for raw in &objective_raws {
        objectives.push(Objective::validate_raw(raw)?);
    }
    Ok(CfdProblem {
        fluid,
        domain,
        boundaries,
        gauge,
        brinkman,
        solver,
        objectives,
        thermal,
        topology_parameter: TOPOLOGY_PARAMETER.into(),
        schema: PROBLEM_SCHEMA.into(),
    })
}

