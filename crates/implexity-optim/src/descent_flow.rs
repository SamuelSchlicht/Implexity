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



use std::collections::BTreeMap;

use implexity_core::{CaeError, CaeResult};
use ndarray::{ArrayD, Zip};
use serde_json::{Map, Value, json};

use crate::bounds::{BoundMultiplierState, is_bound, scaled_constraint};
use crate::design::NamedArrays;
use crate::numeric::float_value;
use crate::search::ExactPoint;
use crate::stage_search::{StageCoordinate, StageResponseProgram};

pub const DESCENT_FLOW_SCHEMA: &str = "implexity-descent-flow/1";
pub const AUGMENTED_LAGRANGIAN: &str = "augmented_lagrangian";
pub const NULL_SPACE: &str = "null_space";

const KEYS: [&str; 8] =
    ["schema", "mode", "range_fraction", "merit_penalty", "active_tolerance", "deflation", "directions", "memory"];
pub const STEEPEST: &str = "steepest";
pub const CONJUGATE: &str = "conjugate";
pub const QUASI_NEWTON: &str = "quasi_newton";
pub const DEFAULT_MEMORY: usize = 5;
const MAX_MEMORY: usize = 50;
const DEFLATION_KEYS: [&str; 3] = ["references", "radius", "weight"];

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn number(raw: Option<&Value>, default: f64, key: &str, admissible: impl Fn(f64) -> bool) -> CaeResult<f64> {
    match raw {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && admissible(x) => Ok(x),
            _ => contract(format!("descent_flow {key} is outside its admissible range")),
        },
    }
}

fn normalise_deflation(raw: &Value) -> CaeResult<Value> {
    let Some(map) = raw.as_object() else { return contract("descent_flow deflation must be a mapping") };
    if let Some(k) = map.keys().find(|k| !DEFLATION_KEYS.contains(&k.as_str())) {
        return contract(format!("descent_flow deflation has the unknown field {k}"));
    }
    let Some(references) = map.get("references").and_then(Value::as_array).filter(|r| !r.is_empty()) else {
        return contract("descent_flow deflation requires a nonempty list of reference designs");
    };
    let mut canonical = Vec::with_capacity(references.len());
    for (i, r) in references.iter().enumerate() {
        let design = NamedArrays::from_wire(r, &format!("deflation reference {i}"))?;
        if design.is_empty() {
            return contract("a deflation reference must name its design coordinates");
        }
        canonical.push(design.to_wire());
    }
    let radius = number(map.get("radius"), 0.1, "deflation radius", |x| x > 0.0)?;
    let weight = number(map.get("weight"), 1.0, "deflation weight", |x| x > 0.0)?;
    Ok(json!({"references": canonical, "radius": radius, "weight": weight}))
}

pub fn normalise_descent_flow(raw: Option<&Value>) -> CaeResult<Map<String, Value>> {
    let empty = Map::new();
    let map = match raw {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(m)) => m,
        Some(Value::String(s)) => return normalise_descent_flow(Some(&json!({"mode": s}))),
        Some(_) => return contract("descent_flow must be a mapping or a mode name"),
    };
    if let Some(k) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return contract(format!("descent_flow has the unknown field {k}"));
    }
    if map.get("schema").is_some_and(|s| s.as_str() != Some(DESCENT_FLOW_SCHEMA)) {
        return contract(format!("descent_flow schema must be {DESCENT_FLOW_SCHEMA}"));
    }
    let mode = match map.get("mode") {
        None | Some(Value::Null) => AUGMENTED_LAGRANGIAN,
        Some(v) => match v.as_str().map(|s| s.trim().to_lowercase()).as_deref() {
            Some(AUGMENTED_LAGRANGIAN) => AUGMENTED_LAGRANGIAN,
            Some(NULL_SPACE) => NULL_SPACE,
            _ => return contract("descent_flow mode must be augmented_lagrangian or null_space"),
        },
    };
    let mut out = Map::new();
    out.insert("schema".into(), json!(DESCENT_FLOW_SCHEMA));
    out.insert("mode".into(), json!(mode));
    out.insert(
        "range_fraction".into(),
        json!(number(map.get("range_fraction"), 1.0, "range_fraction", |x| x > 0.0 && x <= 1.0)?),
    );
    out.insert("merit_penalty".into(), json!(number(map.get("merit_penalty"), 1.0, "merit_penalty", |x| x > 0.0)?));
    out.insert(
        "active_tolerance".into(),
        json!(number(map.get("active_tolerance"), 0.0, "active_tolerance", |x| x >= 0.0)?),
    );
    if let Some(d) = map.get("deflation").filter(|v| !v.is_null()) {
        if mode != NULL_SPACE {
            return contract("descent_flow deflation requires the null_space mode");
        }
        out.insert("deflation".into(), normalise_deflation(d)?);
    }
    let directions = match map.get("directions") {
        None | Some(Value::Null) => STEEPEST,
        Some(v) => match v.as_str().map(|s| s.trim().to_lowercase()).as_deref() {
            Some(STEEPEST) => STEEPEST,
            Some(CONJUGATE) => CONJUGATE,
            Some(QUASI_NEWTON) => QUASI_NEWTON,
            _ => return contract("descent_flow directions must be steepest, conjugate or quasi_newton"),
        },
    };
    if directions != STEEPEST {
        if mode != NULL_SPACE {
            return contract(format!("descent_flow {directions} directions require the null_space mode"));
        }
        out.insert("directions".into(), json!(directions));
    }
    match map.get("memory") {
        None | Some(Value::Null) => {
            if directions == QUASI_NEWTON {
                out.insert("memory".into(), json!(DEFAULT_MEMORY));
            }
        }
        Some(v) => {
            if directions != QUASI_NEWTON {
                return contract("descent_flow memory requires the quasi_newton directions");
            }
            let memory = v
                .as_u64()
                .or_else(|| v.as_f64().filter(|x| x.fract() == 0.0 && *x >= 0.0).map(|x| x as u64))
                .filter(|m| (1..=MAX_MEMORY as u64).contains(m));
            let Some(memory) = memory else {
                return contract(format!("descent_flow memory must be an integer from 1 to {MAX_MEMORY}"));
            };
            out.insert("memory".into(), json!(memory));
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directions {
    Steepest,
    Conjugate,
    QuasiNewton {
        memory: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Deflation {
    pub references: Vec<NamedArrays>,
    pub radius: f64,
    pub weight: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DescentFlow {
    pub mode: String,
    pub range_fraction: f64,
    pub merit_penalty: f64,
    pub active_tolerance: f64,
    pub deflation: Option<Deflation>,
    pub directions: Directions,
}

impl DescentFlow {
    pub fn from_wire(raw: &Map<String, Value>) -> CaeResult<Self> {
        let m = normalise_descent_flow(Some(&Value::Object(raw.clone())))?;
        let num = |k: &str| m.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let deflation = match m.get("deflation") {
            None => None,
            Some(d) => {
                let references = d["references"]
                    .as_array()
                    .map(|rows| {
                        rows.iter()
                            .enumerate()
                            .map(|(i, r)| NamedArrays::from_wire(r, &format!("deflation reference {i}")))
                            .collect::<CaeResult<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default();
                Some(Deflation {
                    references,
                    radius: d["radius"].as_f64().unwrap_or(0.1),
                    weight: d["weight"].as_f64().unwrap_or(1.0),
                })
            }
        };
        let directions = match m.get("directions").and_then(Value::as_str) {
            Some(CONJUGATE) => Directions::Conjugate,
            Some(QUASI_NEWTON) => Directions::QuasiNewton {
                memory: m
                    .get("memory")
                    .and_then(Value::as_u64)
                    .and_then(|v| usize::try_from(v).ok())
                    .unwrap_or(DEFAULT_MEMORY),
            },
            _ => Directions::Steepest,
        };
        Ok(Self {
            mode: m.get("mode").and_then(Value::as_str).unwrap_or(AUGMENTED_LAGRANGIAN).to_string(),
            range_fraction: num("range_fraction"),
            merit_penalty: num("merit_penalty"),
            active_tolerance: num("active_tolerance"),
            deflation,
            directions,
        })
    }

    #[must_use]
    pub fn augmented_lagrangian() -> Self {
        Self {
            mode: AUGMENTED_LAGRANGIAN.into(),
            range_fraction: 1.0,
            merit_penalty: 1.0,
            active_tolerance: 0.0,
            deflation: None,
            directions: Directions::Steepest,
        }
    }

    #[must_use]
    pub fn null_space(&self) -> bool {
        self.mode == NULL_SPACE
    }
}

fn deflation_term(deflation: &Deflation, coordinates: &[StageCoordinate], design: &NamedArrays) -> (f64, NamedArrays) {
    let free: usize = coordinates.iter().map(|c| c.designable.iter().filter(|d| **d).count()).sum();
    let mut gradient = NamedArrays::new();
    for c in coordinates {
        gradient.insert(c.name.clone(), ArrayD::zeros(c.lower.raw_dim()));
    }
    if free == 0 {
        return (0.0, gradient);
    }
    #[allow(clippy::cast_precision_loss)]
    let n = free as f64;
    let s2 = deflation.radius * deflation.radius;
    let mut value = 0.0;
    for reference in &deflation.references {
        let mut d2 = 0.0;
        for c in coordinates {
            let (Some(x), Some(r)) = (design.get(&c.name), reference.get(&c.name)) else { continue };
            if x.shape() != r.shape() {
                continue;
            }
            Zip::from(x).and(r).and(&c.span()).and(&c.designable).for_each(|x, r, s, d| {
                if *d && *s > 0.0 {
                    d2 += ((x - r) / s).powi(2);
                }
            });
        }
        let bump = deflation.weight * (-d2 / n / (2.0 * s2)).exp();
        value += bump;
        for c in coordinates {
            let (Some(x), Some(r), Some(g)) = (design.get(&c.name), reference.get(&c.name), gradient.get_mut(&c.name))
            else {
                continue;
            };
            if x.shape() != r.shape() {
                continue;
            }
            Zip::from(g).and(x).and(r).and(&c.span()).and(&c.designable).for_each(|g, x, r, s, d| {
                if *d && *s > 0.0 {
                    *g -= bump * (x - r) / (s2 * s * s * n);
                }
            });
        }
    }
    (value, gradient)
}

#[must_use]
pub fn deflation_value(flow: &DescentFlow, coordinates: &[StageCoordinate], design: &NamedArrays) -> f64 {
    flow.deflation.as_ref().map_or(0.0, |d| deflation_term(d, coordinates, design).0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Manifold {
    pub active: Vec<usize>,
    pub names: Vec<String>,
    pub residuals: Vec<f64>,
    pub multipliers: Vec<f64>,
    pub objective_per_target: Vec<f64>,
    pub penalty: f64,
    pub tangent: NamedArrays,
    pub range: NamedArrays,
    pub objective: f64,
    pub deflation: f64,
    pub merit: f64,
    pub merit_gradient: NamedArrays,
    pub metric: &'static str,
    pub released: Vec<String>,
    pub lagrangian_gradient: NamedArrays,
    layout: Flat,
    normals: Vec<(Vec<f64>, Vec<f64>)>,
    objective_fields: (Vec<f64>, Vec<f64>),
}

impl Manifold {
    pub fn pairing(&self, covector: &NamedArrays, vector: &NamedArrays) -> CaeResult<f64> {
        Ok(self.layout.dot(&self.layout.flatten(covector)?, &self.layout.flatten(vector)?))
    }

    pub fn project(&self, vector: &NamedArrays) -> CaeResult<NamedArrays> {
        let mut u = self.layout.flatten(vector)?;
        self.project_flat(&mut u)?;
        self.layout.unflatten(&u)
    }

    fn project_flat(&self, u: &mut [f64]) -> CaeResult<()> {
        for (v, f) in u.iter_mut().zip(&self.layout.free) {
            if !*f {
                *v = 0.0;
            }
        }
        if !self.normals.is_empty() {
            let gram: Vec<Vec<f64>> = self
                .normals
                .iter()
                .map(|(gk, _)| self.normals.iter().map(|(_, dl)| self.layout.dot(gk, dl)).collect())
                .collect();
            let b: Vec<f64> = self.normals.iter().map(|(g, _)| self.layout.dot(g, u)).collect();
            let nu = solve(&gram, &b)?;
            for ((_, d), n) in self.normals.iter().zip(&nu) {
                for (k, v) in u.iter_mut().enumerate() {
                    if self.layout.free[k] {
                        *v -= n * d[k];
                    }
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn to_value(&self, coordinates: &[StageCoordinate]) -> Value {
        let rows: Vec<Value> = self
            .names
            .iter()
            .enumerate()
            .map(|(k, name)| {
                json!({"response": name, "constraint": float_value(self.residuals[k]),
                       "multiplier": float_value(self.multipliers[k]),
                       "objective_per_target": float_value(self.objective_per_target[k])})
            })
            .collect();
        let normalised = |a: &NamedArrays| -> f64 {
            coordinates
                .iter()
                .filter_map(|c| a.get(&c.name).map(|v| (v, c.span())))
                .map(|(v, s)| {
                    v.iter().zip(s.iter()).fold(0.0_f64, |m, (x, s)| if *s > 0.0 { m.max((x / s).abs()) } else { m })
                })
                .fold(0.0_f64, f64::max)
        };
        json!({
            "mode": NULL_SPACE,
            "metric": self.metric,
            "constraints": rows,
            "released": self.released,
            "objective": float_value(self.objective),
            "deflation": float_value(self.deflation),
            "merit": float_value(self.merit),
            "merit_penalty": float_value(self.penalty),
            "tangent_max_normalised": float_value(normalised(&self.tangent)),
            "range_max_normalised": float_value(normalised(&self.range)),
        })
    }

    pub fn merit_of(
        &self,
        flow: &DescentFlow,
        program: &StageResponseProgram,
        bound_state: &BoundMultiplierState,
        coordinates: &[StageCoordinate],
        values: &BTreeMap<String, f64>,
        design: Option<&NamedArrays>,
    ) -> CaeResult<f64> {
        let mut phi = program.aggregate(values, bound_state)?.base;
        if let Some(x) = design {
            phi += deflation_value(flow, coordinates, x);
        }
        for (k, &i) in self.active.iter().enumerate() {
            let spec = &program.expanded[i];
            let Some(v) = values.get(&spec.name) else {
                return contract(format!("trial values omit the constrained response {}", spec.name));
            };
            let (c, _) = scaled_constraint(spec, *v);
            let p = if spec.sense == "equal" { c } else { c.max(0.0) };
            phi += -self.multipliers[k] * c + 0.5 * self.penalty * p * p;
        }
        if !phi.is_finite() {
            return contract("nonfinite merit of the null-space descent");
        }
        Ok(phi)
    }
}

#[derive(Debug, Clone)]
struct SecantPoint {
    design: Vec<f64>,
    objective: (Vec<f64>, Vec<f64>),
    normals: Vec<(Vec<f64>, Vec<f64>)>,
    active: Vec<usize>,
    violation: f64,
}

#[derive(Debug, Clone)]
struct SecantPair {
    s: Vec<f64>,
    y: Vec<f64>,
    z: Vec<f64>,
    sy: f64,
    yz: f64,
}

#[derive(Debug, Clone)]
pub struct SecantMemory {
    memory: usize,
    pairs: std::collections::VecDeque<SecantPair>,
    previous: Option<SecantPoint>,
    pub pairs_used: usize,
    pub scaling: f64,
    pub skipped: Option<&'static str>,
}

impl SecantMemory {
    #[must_use]
    pub fn new(memory: usize) -> Self {
        Self {
            memory: memory.max(1),
            pairs: std::collections::VecDeque::new(),
            previous: None,
            pairs_used: 0,
            scaling: 1.0,
            skipped: None,
        }
    }

    pub fn clear(&mut self, point: bool) {
        self.pairs.clear();
        if point {
            self.previous = None;
        }
    }

    pub fn search(&mut self, m: &Manifold, design: &NamedArrays) -> CaeResult<NamedArrays> {
        let layout = &m.layout;
        let x = layout.flatten(design)?;
        let current = SecantPoint {
            design: x.clone(),
            objective: m.objective_fields.clone(),
            normals: m.normals.clone(),
            active: m.active.clone(),
            violation: m.residuals.iter().fold(0.0_f64, |a, c| a.max(c.abs())),
        };
        self.skipped = None;
        if let Some(previous) = self.previous.take() {
            if previous.active == current.active && previous.normals.len() == current.normals.len() {
                self.record(m, &previous, &current)?;
            } else {
                self.pairs.clear();
                self.skipped = Some("active_set");
            }
        }
        self.previous = Some(current);
        let lagrangian = layout.flatten(&m.lagrangian_gradient)?;
        let tangent = layout.flatten(&m.tangent)?;
        self.pairs_used = 0;
        self.scaling = 1.0;
        if self.pairs.is_empty() {
            return Ok(m.tangent.clone());
        }
        let mut q = lagrangian.clone();
        let mut qd = tangent;
        let mut alpha = vec![0.0; self.pairs.len()];
        for (i, p) in self.pairs.iter().enumerate().rev() {
            let a = layout.dot(&q, &p.s) / p.sy;
            alpha[i] = a;
            for k in 0..q.len() {
                q[k] -= a * p.y[k];
                qd[k] -= a * p.z[k];
            }
        }
        let newest = self.pairs.back().map_or(1.0, |p| p.sy / p.yz);
        let gamma = if newest.is_finite() && newest > 0.0 { newest } else { 1.0 };
        let mut r: Vec<f64> = qd.iter().map(|v| gamma * v).collect();
        for (i, p) in self.pairs.iter().enumerate() {
            let b = layout.dot(&p.y, &r) / p.sy;
            for k in 0..r.len() {
                r[k] += (alpha[i] - b) * p.s[k];
            }
        }
        m.project_flat(&mut r)?;
        let descent = layout.dot(&lagrangian, &r);
        if !(descent.is_finite() && descent > 0.0) || r.iter().any(|v| !v.is_finite()) {
            self.pairs.clear();
            self.skipped = Some("no_descent");
            return Ok(m.tangent.clone());
        }
        self.pairs_used = self.pairs.len();
        self.scaling = gamma;
        layout.unflatten(&r)
    }

    fn record(&mut self, m: &Manifold, previous: &SecantPoint, current: &SecantPoint) -> CaeResult<()> {
        let layout = &m.layout;
        let size = current.design.len();
        let mut s: Vec<f64> = current.design.iter().zip(&previous.design).map(|(a, b)| a - b).collect();
        let step = layout.dot(&s, &s).sqrt();
        if step.is_nan() || step <= 0.0 {
            self.skipped = Some("no_step");
            return Ok(());
        }
        if previous.violation > 1.0 || current.violation > 1.0 {
            self.skipped = Some("off_manifold");
            return Ok(());
        }
        m.project_flat(&mut s)?;
        if layout.dot(&s, &s).sqrt() < 0.5 * step {
            self.skipped = Some("range_step");
            return Ok(());
        }
        let mut y: Vec<f64> = (0..size).map(|k| current.objective.0[k] - previous.objective.0[k]).collect();
        let mut z: Vec<f64> = (0..size).map(|k| current.objective.1[k] - previous.objective.1[k]).collect();
        for ((now, before), l) in current.normals.iter().zip(&previous.normals).zip(&m.multipliers) {
            for k in 0..size {
                y[k] -= l * (now.0[k] - before.0[k]);
                z[k] -= l * (now.1[k] - before.1[k]);
            }
        }
        m.project_flat(&mut z)?;
        let sy = layout.dot(&y, &s);
        let yz = layout.dot(&y, &z);
        if !(sy.is_finite() && yz.is_finite() && sy > 0.0 && yz > 0.0) {
            self.skipped = Some("curvature");
            return Ok(());
        }
        self.pairs.push_back(SecantPair { s, y, z, sy, yz });
        while self.pairs.len() > self.memory {
            self.pairs.pop_front();
        }
        Ok(())
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "pairs": self.pairs_used,
            "scaling": float_value(self.scaling),
            "skipped": self.skipped,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Flat {
    shapes: Vec<(String, Vec<usize>)>,
    free: Vec<bool>,
}

impl Flat {
    fn new(coordinates: &[StageCoordinate]) -> Self {
        let mut shapes = Vec::new();
        let mut free = Vec::new();
        for c in coordinates {
            shapes.push((c.name.clone(), c.lower.shape().to_vec()));
            free.extend(c.designable.iter().zip(c.lower.iter().zip(&c.upper))
                .map(|(designable, (lower, upper))| *designable && upper > lower));
        }
        Self { shapes, free }
    }

    fn flatten(&self, a: &NamedArrays) -> CaeResult<Vec<f64>> {
        let mut out = Vec::with_capacity(self.free.len());
        for (name, shape) in &self.shapes {
            let v = a.get(name).ok_or_else(|| CaeError::contract(format!("descent omits coordinate {name}")))?;
            if v.shape() != shape.as_slice() {
                return contract(format!("descent field of {name} has another shape than the coordinate"));
            }
            out.extend(v.iter().copied());
        }
        Ok(out)
    }

    fn unflatten(&self, flat: &[f64]) -> CaeResult<NamedArrays> {
        let mut out = NamedArrays::new();
        let mut offset = 0;
        for (name, shape) in &self.shapes {
            let size: usize = shape.iter().product();
            let a = ArrayD::from_shape_vec(ndarray::IxDyn(shape), flat[offset..offset + size].to_vec())
                .map_err(|e| CaeError::contract(e.to_string()))?;
            out.insert(name.clone(), a);
            offset += size;
        }
        Ok(out)
    }

    fn dot(&self, a: &[f64], b: &[f64]) -> f64 {
        a.iter().zip(b).zip(&self.free).filter(|(_, f)| **f).map(|((x, y), _)| x * y).sum()
    }
}

fn solve(g: &[Vec<f64>], b: &[f64]) -> CaeResult<Vec<f64>> {
    let n = b.len();
    // A fully bound-limited tangent can have no remaining constraint support.
    // Its homogeneous projection is still well defined; restoration is solved
    // separately using the designable coordinates.
    if b.iter().all(|v| *v == 0.0) {
        return Ok(vec![0.0; n]);
    }
    let scale = (0..n).map(|i| g[i][i].abs()).fold(0.0_f64, f64::max).max(f64::MIN_POSITIVE);
    let mut a: Vec<Vec<f64>> = (0..n)
        .map(|i| {
            let mut row = g[i].clone();
            row[i] += 1e-12 * scale;
            row.push(b[i]);
            row
        })
        .collect();
    for col in 0..n {
        let pivot = (col..n)
            .max_by(|x, y| a[*x][col].abs().total_cmp(&a[*y][col].abs()))
            .unwrap_or(col);
        a.swap(col, pivot);
        let p = a[col][col];
        if p.abs() <= 1e-300 {
            return contract("the active constraints of the null-space descent are linearly dependent");
        }
        for row in col + 1..n {
            let factor = a[row][col] / p;
            for k in col..=n {
                a[row][k] -= factor * a[col][k];
            }
        }
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let s: f64 = (i + 1..n).map(|k| a[i][k] * x[k]).sum();
        x[i] = (a[i][n] - s) / a[i][i];
    }
    Ok(x)
}

#[allow(clippy::too_many_lines)]
pub fn manifold(
    flow: &DescentFlow,
    program: &StageResponseProgram,
    bound_state: &BoundMultiplierState,
    coordinates: &[StageCoordinate],
    point: &ExactPoint,
) -> CaeResult<Manifold> {
    let aggregate = program.aggregate(&point.values, bound_state)?;
    let flat = Flat::new(coordinates);
    let spans: NamedArrays = coordinates.iter().map(|c| (c.name.clone(), c.span())).collect();
    let span2 = flat.flatten(&spans)?.iter().map(|s| s * s).collect::<Vec<_>>();
    let objective_terms: Vec<(usize, f64)> = program
        .expanded
        .iter()
        .enumerate()
        .filter(|(i, s)| !is_bound(s) && aggregate.coefficients[*i].abs() > 0.0)
        .map(|(i, _)| (i, aggregate.coefficients[i]))
        .collect();
    let constraint_terms: Vec<usize> =
        program.expanded.iter().enumerate().filter(|(_, s)| is_bound(s)).map(|(i, _)| i).collect();
    let geometry = !point.descent.is_empty()
        && objective_terms
            .iter()
            .map(|(i, _)| *i)
            .chain(constraint_terms.iter().copied())
            .all(|i| point.descent.contains_key(&program.expanded[i].name));
    let gradient_of = |i: usize| -> CaeResult<Vec<f64>> {
        let name = &program.expanded[i].name;
        let g = point.gradients.get(name).ok_or_else(|| CaeError::contract(format!("exact point omits gradients of {name}")))?;
        flat.flatten(g)
    };
    let descent_of = |i: usize, g: &[f64]| -> CaeResult<Vec<f64>> {
        if geometry {
            flat.flatten(&point.descent[&program.expanded[i].name])
        } else {
            Ok(g.iter().zip(&span2).map(|(g, s)| g * s).collect())
        }
    };
    let size = flat.free.len();
    let mut g_j = vec![0.0; size];
    let mut d_j = vec![0.0; size];
    for (i, coefficient) in &objective_terms {
        let g = gradient_of(*i)?;
        let d = descent_of(*i, &g)?;
        for k in 0..size {
            g_j[k] += coefficient * g[k];
            d_j[k] += coefficient * d[k];
        }
    }
    let mut deflation = 0.0;
    if let Some(defl) = &flow.deflation {
        let (value, gradient) = deflation_term(defl, coordinates, &point.design);
        deflation = value;
        let g = flat.flatten(&gradient)?;
        for k in 0..size {
            g_j[k] += g[k];
            d_j[k] += g[k] * span2[k];
        }
    }
    struct Row {
        index: usize,
        c: f64,
        equality: bool,
        g: Vec<f64>,
        d: Vec<f64>,
    }
    let mut rows: Vec<Row> = Vec::new();
    for &i in &constraint_terms {
        let spec = &program.expanded[i];
        let value = *point
            .values
            .get(&spec.name)
            .ok_or_else(|| CaeError::contract(format!("exact point omits response {}", spec.name)))?;
        let (c, dc) = scaled_constraint(spec, value);
        let equality = spec.sense == "equal";
        if !equality && c <= -flow.active_tolerance {
            continue;
        }
        let raw = gradient_of(i)?;
        let d: Vec<f64> = descent_of(i, &raw)?.iter().map(|v| dc * v).collect();
        let g: Vec<f64> = raw.iter().map(|v| dc * v).collect();
        rows.push(Row { index: i, c, equality, g, d });
    }
    let lower = flat.flatten(&coordinates.iter().map(|c| (c.name.clone(), c.lower.clone())).collect())?;
    let upper = flat.flatten(&coordinates.iter().map(|c| (c.name.clone(), c.upper.clone())).collect())?;
    let x = flat.flatten(&point.design)?;
    let mut free = flat.free.clone();
    let mut released = Vec::new();
    let mut lambda;
    let mut tangent;
    loop {
        let view = Flat { shapes: flat.shapes.clone(), free: free.clone() };
        let gram: Vec<Vec<f64>> =
            rows.iter().map(|rk| rows.iter().map(|rl| view.dot(&rk.g, &rl.d)).collect()).collect();
        let b: Vec<f64> = rows.iter().map(|r| view.dot(&r.g, &d_j)).collect();
        lambda = if rows.is_empty() { Vec::new() } else { solve(&gram, &b)? };
        let release = rows
            .iter()
            .enumerate()
            .filter(|(k, r)| !r.equality && r.c <= 0.0 && lambda[*k] > 0.0)
            .max_by(|(a, _), (b, _)| lambda[*a].total_cmp(&lambda[*b]))
            .map(|(k, _)| k);
        if let Some(k) = release {
            released.push(program.expanded[rows[k].index].name.clone());
            rows.remove(k);
            continue;
        }
        tangent = d_j.clone();
        let mut cancellation_scale: Vec<f64> = d_j.iter().map(|v| v.abs()).collect();
        for (r, l) in rows.iter().zip(&lambda) {
            for k in 0..size {
                tangent[k] -= l * r.d[k];
                cancellation_scale[k] += (l * r.d[k]).abs();
            }
        }
        for k in 0..size {
            // Do not turn the regularized solve's cancellation error into an
            // active bound (or amplify it into a finite normalized step).
            if !free[k] || tangent[k].abs() <= 8e-12 * cancellation_scale[k] {
                tangent[k] = 0.0;
            }
        }
        let mut changed = false;
        for k in 0..size {
            if !free[k] {
                continue;
            }
            let tol = 1e-12 * (upper[k] - lower[k]).abs();
            let outward = (x[k] <= lower[k] + tol && tangent[k] > 0.0) || (x[k] >= upper[k] - tol && tangent[k] < 0.0);
            if outward {
                free[k] = false;
                changed = true;
            }
        }
        // Each changed pass removes at least one free coordinate. Recompute
        // until stable so the final vector satisfies the final active set.
        if !changed {
            break;
        }
    }
    let view = Flat { shapes: flat.shapes.clone(), free: free.clone() };
    // Bounds that block objective descent must not block an inward constraint
    // correction. Use the original designable mask for the restoration solve.
    let gram: Vec<Vec<f64>> = rows.iter().map(|rk| rows.iter().map(|rl| flat.dot(&rk.g, &rl.d)).collect()).collect();
    let phi: Vec<f64> = rows.iter().map(|r| if r.equality { r.c } else { r.c.max(0.0) }).collect();
    let mut range = vec![0.0; size];
    if phi.iter().any(|p| *p != 0.0) {
        let mu = solve(&gram, &phi)?;
        for (r, m) in rows.iter().zip(&mu) {
            for k in 0..size {
                if flat.free[k] {
                    range[k] -= m * r.d[k];
                }
            }
        }
    }
    let mut lagrangian_gradient = g_j.clone();
    for (r, l) in rows.iter().zip(&lambda) {
        for k in 0..size {
            lagrangian_gradient[k] -= l * r.g[k];
        }
    }
    let squared: f64 = phi.iter().map(|p| p * p).sum();
    let mut penalty = flow.merit_penalty;
    if squared > 0.0 {
        let along = flat.dot(&lagrangian_gradient, &range);
        penalty = penalty.max(2.0 * along.abs() / squared);
    }
    let lagrangian = flat.unflatten(&lagrangian_gradient)?;
    let mut merit_gradient = lagrangian_gradient;
    for (r, p) in rows.iter().zip(&phi) {
        for k in 0..size {
            merit_gradient[k] += penalty * p * r.g[k];
        }
    }
    let objective = aggregate.base + deflation;
    let merit = objective - rows.iter().zip(&lambda).map(|(r, l)| l * r.c).sum::<f64>() + 0.5 * penalty * squared;
    let objective_per_target: Vec<f64> = rows
        .iter()
        .zip(&lambda)
        .map(|(r, l)| {
            let spec = &program.expanded[r.index];
            let scale = spec.scale.as_f64();
            if spec.sense == "lower" { -l / scale } else { l / scale }
        })
        .collect();
    if !merit.is_finite() || merit_gradient.iter().chain(&tangent).chain(&range).any(|v| !v.is_finite()) {
        return contract("nonfinite null-space descent quantities");
    }
    Ok(Manifold {
        active: rows.iter().map(|r| r.index).collect(),
        names: rows.iter().map(|r| program.expanded[r.index].name.clone()).collect(),
        residuals: rows.iter().map(|r| r.c).collect(),
        multipliers: lambda,
        objective_per_target,
        penalty,
        tangent: flat.unflatten(&tangent)?,
        range: flat.unflatten(&range)?,
        objective,
        deflation,
        merit,
        merit_gradient: flat.unflatten(&merit_gradient)?,
        metric: if geometry { "geometry" } else { "box" },
        released,
        lagrangian_gradient: lagrangian,
        normals: rows.iter().map(|r| (r.g.clone(), r.d.clone())).collect(),
        layout: view,
        objective_fields: (g_j, d_j),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quadratic_point(x: &[f64], a: &[f64]) -> (Manifold, NamedArrays) {
        let n = x.len();
        let g: Vec<f64> = x.iter().zip(a).map(|(x, a)| a * x).collect();
        let layout = Flat { shapes: vec![("x".into(), vec![n])], free: vec![true; n] };
        let field = |v: &[f64]| layout.unflatten(v).expect("layout");
        let objective = 0.5 * x.iter().zip(a).map(|(x, a)| a * x * x).sum::<f64>();
        let m = Manifold {
            active: Vec::new(),
            names: Vec::new(),
            residuals: Vec::new(),
            multipliers: Vec::new(),
            objective_per_target: Vec::new(),
            penalty: 1.0,
            tangent: field(&g),
            range: field(&vec![0.0; n]),
            objective,
            deflation: 0.0,
            merit: objective,
            merit_gradient: field(&g),
            metric: "box",
            released: Vec::new(),
            lagrangian_gradient: field(&g),
            normals: Vec::new(),
            objective_fields: (g.clone(), g.clone()),
            layout: layout.clone(),
        };
        (m, field(x))
    }

    fn values(a: &NamedArrays) -> Vec<f64> {
        a.get("x").expect("x").iter().copied().collect()
    }

    #[test]
    fn quasi_newton_declaration_is_canonical() {
        let a = normalise_descent_flow(Some(&json!({"mode": "null_space", "directions": "quasi_newton"}))).unwrap();
        let b = normalise_descent_flow(Some(&json!({"mode": "null_space", "directions": "quasi_newton", "memory": 5})))
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a["memory"], json!(DEFAULT_MEMORY));
        assert_eq!(DescentFlow::from_wire(&a).unwrap().directions, Directions::QuasiNewton { memory: 5 });
        let c = normalise_descent_flow(Some(&json!({"mode": "null_space", "directions": "quasi_newton", "memory": 8})))
            .unwrap();
        assert_eq!(DescentFlow::from_wire(&c).unwrap().directions, Directions::QuasiNewton { memory: 8 });
        let s = normalise_descent_flow(Some(&json!({"mode": "null_space", "directions": "steepest"}))).unwrap();
        assert!(!s.contains_key("directions") && !s.contains_key("memory"));
        assert_eq!(DescentFlow::from_wire(&s).unwrap().directions, Directions::Steepest);
        let k = normalise_descent_flow(Some(&json!({"mode": "null_space", "directions": "conjugate"}))).unwrap();
        assert_eq!(DescentFlow::from_wire(&k).unwrap().directions, Directions::Conjugate);
    }

    #[test]
    fn quasi_newton_declaration_rejects_misuse() {
        for raw in [
            json!({"mode": "augmented_lagrangian", "directions": "quasi_newton"}),
            json!({"mode": "null_space", "memory": 3}),
            json!({"mode": "null_space", "directions": "conjugate", "memory": 3}),
            json!({"mode": "null_space", "directions": "quasi_newton", "memory": 0}),
            json!({"mode": "null_space", "directions": "quasi_newton", "memory": 51}),
            json!({"mode": "null_space", "directions": "quasi_newton", "memory": 2.5}),
            json!({"mode": "null_space", "directions": "newton"}),
        ] {
            assert!(normalise_descent_flow(Some(&raw)).is_err(), "{raw}");
        }
    }

    #[test]
    fn first_search_is_the_tangent_field_and_one_pair_gives_the_newton_step() {
        let a = [4.0, 4.0, 4.0];
        let mut memory = SecantMemory::new(3);
        let (m0, x0) = quadratic_point(&[1.0, -2.0, 0.5], &a);
        let u0 = memory.search(&m0, &x0).unwrap();
        assert_eq!(values(&u0), values(&m0.tangent));
        assert_eq!(memory.pairs_used, 0);
        let x1v = [0.8, -1.5, 0.45];
        let (m1, x1) = quadratic_point(&x1v, &a);
        let u1 = memory.search(&m1, &x1).unwrap();
        assert_eq!(memory.pairs_used, 1);
        assert!((memory.scaling - 0.25).abs() < 1e-12);
        for (u, x) in values(&u1).iter().zip(&x1v) {
            assert!((u - x).abs() < 1e-12, "{u} {x}");
        }
    }

    #[test]
    fn secant_pairs_improve_on_the_tangent_field_and_negative_curvature_is_skipped() {
        let a = [1.0, 10.0, 100.0];
        let mut memory = SecantMemory::new(5);
        let points = [[1.0, 1.0, 1.0], [0.9, 0.7, 0.2], [0.85, 0.5, 0.05]];
        let mut last = None;
        for p in &points {
            let (m, x) = quadratic_point(p, &a);
            let u = memory.search(&m, &x).unwrap();
            assert!(m.layout.dot(&values(&m.lagrangian_gradient), &values(&u)) > 0.0);
            last = Some((m, u));
        }
        assert_eq!(memory.pairs_used, 2);
        let (m, u) = last.unwrap();
        let x = points[2];
        let newton_error = |step: &[f64]| -> f64 {
            let g: Vec<f64> = x.iter().zip(&a).map(|(x, a)| a * x).collect();
            let gd: f64 = g.iter().zip(step).map(|(g, d)| g * d).sum();
            let dad: f64 = step.iter().zip(&a).map(|(d, a)| a * d * d).sum();
            let t = gd / dad;
            x.iter().zip(step).map(|(x, d)| (x - t * d).powi(2)).sum::<f64>().sqrt()
        };
        assert!(newton_error(&values(&u)) < newton_error(&values(&m.tangent)));
        let mut memory = SecantMemory::new(5);
        let (m0, x0) = quadratic_point(&[1.0, 1.0, 1.0], &[-1.0, -1.0, -1.0]);
        memory.search(&m0, &x0).unwrap();
        let (m1, x1) = quadratic_point(&[0.5, 0.5, 0.5], &[-1.0, -1.0, -1.0]);
        let u = memory.search(&m1, &x1).unwrap();
        assert_eq!(memory.skipped, Some("curvature"));
        assert_eq!(memory.pairs_used, 0);
        assert_eq!(values(&u), values(&m1.tangent));
    }

    #[test]
    fn no_secant_pair_away_from_the_manifold() {
        let a = [1.0, 10.0, 100.0];
        let mut memory = SecantMemory::new(5);
        let (mut m0, x0) = quadratic_point(&[1.0, 1.0, 1.0], &a);
        m0.residuals = vec![2.5];
        memory.search(&m0, &x0).unwrap();
        let (mut m1, x1) = quadratic_point(&[0.9, 0.7, 0.2], &a);
        m1.residuals = vec![0.4];
        let u = memory.search(&m1, &x1).unwrap();
        assert_eq!(memory.skipped, Some("off_manifold"));
        assert_eq!(memory.pairs_used, 0);
        assert_eq!(values(&u), values(&m1.tangent));
        let (mut m2, x2) = quadratic_point(&[0.85, 0.5, 0.05], &a);
        m2.residuals = vec![-0.3];
        memory.search(&m2, &x2).unwrap();
        assert_eq!(memory.skipped, None);
        assert_eq!(memory.pairs_used, 1);
    }
}
