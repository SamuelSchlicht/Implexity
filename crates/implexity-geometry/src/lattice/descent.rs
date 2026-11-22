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



use serde_json::{Map, Value, json};

use crate::error::{GResult, GeometryError};
use crate::lattice::controls::control_components;
use crate::lattice::numerics::{Grids, Interp};

pub const DESCENT_METRIC_SCHEMA: &str = "implexity-lattice-descent-metric/1";

const KEYS: [&str; 12] = [
    "schema",
    "mode",
    "phase_length_mm",
    "occupancy",
    "thickness",
    "occupancy_mobility",
    "fixed",
    "damping",
    "regularization",
    "material_metric",
    "transport_length_mm",
    "prior",
];
const CG_TOLERANCE: f64 = 1e-12;
const PULLBACK_TOLERANCE: f64 = 1e-10;
const PULLBACK_FLOOR: f64 = 1e-12;
const FISHER_RAO_EPS: f64 = 1e-6;
const TRANSPORT_TOLERANCE: f64 = 1e-12;
pub const COMPONENTS: usize = 20;

fn invalid<T>(message: impl Into<String>) -> GResult<T> {
    Err(GeometryError::Value(message.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogitMetric {
    Fisher,
    Euclidean,
    Pullback,
}

impl LogitMetric {
    fn parse(raw: Option<&Value>, key: &str, mode: MetricMode, default: Self) -> GResult<Self> {
        let parsed = match raw.map(|v| v.as_str()) {
            None => default,
            Some(Some("fisher")) => Self::Fisher,
            Some(Some("euclidean")) => Self::Euclidean,
            Some(Some("pullback")) => Self::Pullback,
            _ => return invalid(format!("descent metric {key} must be 'fisher', 'euclidean' or 'pullback'")),
        };
        if parsed == Self::Pullback && mode != MetricMode::Pullback {
            return invalid(format!("descent metric {key} 'pullback' requires the pullback mode"));
        }
        Ok(parsed)
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Fisher => "fisher",
            Self::Euclidean => "euclidean",
            Self::Pullback => "pullback",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricMode {
    Channels,
    Pullback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialMetric {
    FisherRao,
    L2,
    Transport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChannelPrior {
    pub covariance: Vec<f64>,
    pub weight: f64,
}

impl ChannelPrior {
    fn parse(raw: &Value) -> GResult<Self> {
        let Some(map) = raw.as_object() else { return invalid("descent metric prior must be a mapping") };
        if let Some(unknown) = map.keys().find(|k| *k != "covariance" && *k != "weight") {
            return invalid(format!("descent metric prior has the unknown field {unknown}"));
        }
        let rows = map.get("covariance").and_then(Value::as_array).filter(|r| r.len() == COMPONENTS);
        let Some(rows) = rows else {
            return invalid("descent metric prior covariance must be a 20 x 20 matrix");
        };
        let mut covariance = Vec::with_capacity(COMPONENTS * COMPONENTS);
        for row in rows {
            let Some(row) = row.as_array().filter(|r| r.len() == COMPONENTS) else {
                return invalid("descent metric prior covariance must be a 20 x 20 matrix");
            };
            for v in row {
                match v.as_f64() {
                    Some(x) if x.is_finite() => covariance.push(x),
                    _ => return invalid("descent metric prior covariance must be finite"),
                }
            }
        }
        let scale = covariance.iter().fold(0.0_f64, |m, v| m.max(v.abs()));
        for i in 0..COMPONENTS {
            for j in 0..COMPONENTS {
                if (covariance[i * COMPONENTS + j] - covariance[j * COMPONENTS + i]).abs() > 1e-9 * scale {
                    return invalid("descent metric prior covariance must be symmetric");
                }
            }
        }
        let weight = match map.get("weight") {
            None => 1.0,
            Some(v) => match v.as_f64() {
                Some(x) if x.is_finite() && x > 0.0 => x,
                _ => return invalid("descent metric prior weight must be a finite positive number"),
            },
        };
        Ok(Self { covariance, weight })
    }

    fn to_value(&self) -> Value {
        let rows: Vec<Value> = self.covariance.chunks(COMPONENTS).map(|r| json!(r)).collect();
        json!({"covariance": rows, "weight": self.weight})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DescentMetric {
    pub mode: MetricMode,
    pub phase_length_mm: f64,
    pub occupancy: LogitMetric,
    pub thickness: LogitMetric,
    pub occupancy_mobility: f64,
    pub fixed: Vec<usize>,
    pub damping: f64,
    pub regularization: f64,
    pub material: MaterialMetric,
    pub transport_length_mm: f64,
    pub prior: Option<ChannelPrior>,
}

fn number(map: &Map<String, Value>, key: &str, default: f64, ok: impl Fn(f64) -> bool, what: &str) -> GResult<f64> {
    match map.get(key) {
        None => Ok(default),
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && ok(x) => Ok(x),
            _ => invalid(format!("descent metric {key} must be {what}")),
        },
    }
}

impl DescentMetric {
    pub fn from_value(raw: &Value) -> GResult<Self> {
        let Some(map) = raw.as_object() else { return invalid("descent metric must be a mapping") };
        if let Some(unknown) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
            return invalid(format!("descent metric has the unknown field {unknown}"));
        }
        if map.get("schema").is_some_and(|s| s.as_str() != Some(DESCENT_METRIC_SCHEMA)) {
            return invalid(format!("descent metric schema must be {DESCENT_METRIC_SCHEMA}"));
        }
        let mode = match map.get("mode").map(|v| v.as_str()) {
            None | Some(Some("channels")) => MetricMode::Channels,
            Some(Some("pullback")) => MetricMode::Pullback,
            _ => return invalid("descent metric mode must be 'channels' or 'pullback'"),
        };
        if mode == MetricMode::Channels {
            if let Some(key) = ["damping", "regularization", "material_metric", "transport_length_mm", "prior"]
                .iter()
                .find(|k| map.contains_key(**k))
            {
                return invalid(format!("descent metric {key} requires the pullback mode"));
            }
        }
        let names: Vec<String> = control_components().into_iter().map(|c| c.name).collect();
        let mut fixed = Vec::new();
        if let Some(raw_fixed) = map.get("fixed") {
            let Some(list) = raw_fixed.as_array() else {
                return invalid("descent metric fixed must be a list of control component names");
            };
            for v in list {
                let Some(i) = v.as_str().and_then(|n| names.iter().position(|m| m == n)) else {
                    return invalid(format!("descent metric fixed names no control component: {v}"));
                };
                if !fixed.contains(&i) {
                    fixed.push(i);
                }
            }
            fixed.sort_unstable();
        }
        let pullback = mode == MetricMode::Pullback;
        let material = match map.get("material_metric").map(|v| v.as_str()) {
            None | Some(Some("fisher_rao")) => MaterialMetric::FisherRao,
            Some(Some("l2")) => MaterialMetric::L2,
            Some(Some("transport")) => MaterialMetric::Transport,
            _ => return invalid("descent metric material_metric must be 'fisher_rao', 'l2' or 'transport'"),
        };
        let transport_length_mm = if material == MaterialMetric::Transport {
            let length = number(map, "transport_length_mm", 0.0, |x| x > 0.0, "a finite positive length")?;
            if length <= 0.0 {
                return invalid("descent metric transport material metric requires transport_length_mm");
            }
            length
        } else {
            if map.contains_key("transport_length_mm") {
                return invalid("descent metric transport_length_mm requires the transport material metric");
            }
            0.0
        };
        let prior = match map.get("prior") {
            None | Some(Value::Null) => None,
            Some(v) => Some(ChannelPrior::parse(v)?),
        };
        Ok(Self {
            mode,
            phase_length_mm: number(map, "phase_length_mm", 0.0, |x| x >= 0.0, "a finite nonnegative length")?,
            occupancy: LogitMetric::parse(map.get("occupancy"), "occupancy", mode, LogitMetric::Fisher)?,
            thickness: LogitMetric::parse(
                map.get("thickness"),
                "thickness",
                mode,
                if pullback { LogitMetric::Pullback } else { LogitMetric::Fisher },
            )?,
            occupancy_mobility: number(map, "occupancy_mobility", 1.0, |x| x > 0.0, "a finite positive factor")?,
            fixed,
            damping: number(map, "damping", 0.1, |x| x >= 0.0, "a finite nonnegative fraction")?,
            regularization: number(map, "regularization", 1.0, |x| x >= 0.0, "a finite nonnegative fraction")?,
            material,
            transport_length_mm,
            prior,
        })
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("schema".into(), json!(DESCENT_METRIC_SCHEMA));
        if self.mode == MetricMode::Pullback {
            m.insert("mode".into(), json!("pullback"));
        }
        m.insert("phase_length_mm".into(), json!(self.phase_length_mm));
        m.insert("occupancy".into(), json!(self.occupancy.name()));
        m.insert("thickness".into(), json!(self.thickness.name()));
        m.insert("occupancy_mobility".into(), json!(self.occupancy_mobility));
        if !self.fixed.is_empty() {
            let names: Vec<String> = control_components().into_iter().map(|c| c.name).collect();
            m.insert("fixed".into(), json!(self.fixed.iter().map(|i| names[*i].clone()).collect::<Vec<_>>()));
        }
        if self.mode == MetricMode::Pullback {
            m.insert("damping".into(), json!(self.damping));
            m.insert("regularization".into(), json!(self.regularization));
            m.insert(
                "material_metric".into(),
                json!(match self.material {
                    MaterialMetric::FisherRao => "fisher_rao",
                    MaterialMetric::L2 => "l2",
                    MaterialMetric::Transport => "transport",
                }),
            );
            if self.material == MaterialMetric::Transport {
                m.insert("transport_length_mm".into(), json!(self.transport_length_mm));
            }
            if let Some(p) = &self.prior {
                m.insert("prior".into(), p.to_value());
            }
        }
        Value::Object(m)
    }

    #[must_use]
    pub fn pullback_components(&self) -> Vec<usize> {
        if self.mode != MetricMode::Pullback {
            return Vec::new();
        }
        (0..COMPONENTS)
            .filter(|c| !self.fixed.contains(c))
            .filter(|c| match c {
                0 => self.thickness == LogitMetric::Pullback,
                1 => self.occupancy == LogitMetric::Pullback,
                _ => true,
            })
            .collect()
    }
}

#[must_use]
pub fn control_spacing(grids: &Grids) -> [f64; 3] {
    std::array::from_fn(|a| {
        #[allow(clippy::cast_precision_loss)]
        let length = grids.n[a] as f64 * grids.spacing[a];
        #[allow(clippy::cast_precision_loss)]
        let intervals = (grids.nc[a] - 1) as f64;
        length / intervals
    })
}

#[must_use]
pub fn node_weights(interp: &Interp, cells: usize) -> Vec<f64> {
    interp.transpose(&vec![1.0; cells])
}

#[must_use]
pub fn weight_floor(weights: &[f64]) -> f64 {
    1e-9 * weights.iter().fold(0.0_f64, |m, v| m.max(*v)).max(f64::MIN_POSITIVE)
}

#[must_use]
pub fn lumped(interp: &Interp, weights: &[f64], cell_field: &[f64]) -> Vec<f64> {
    let floor = weight_floor(weights);
    interp.transpose(cell_field).iter().zip(weights).map(|(v, w)| v / w.max(floor)).collect()
}

fn node(nc: [usize; 3], x: usize, y: usize, z: usize) -> usize {
    (x * nc[1] + y) * nc[2] + z
}

fn edges(nc: [usize; 3]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for x in 0..nc[0] {
        for y in 0..nc[1] {
            for z in 0..nc[2] {
                let i = node(nc, x, y, z);
                if x + 1 < nc[0] {
                    out.push((i, node(nc, x + 1, y, z), 0));
                }
                if y + 1 < nc[1] {
                    out.push((i, node(nc, x, y + 1, z), 1));
                }
                if z + 1 < nc[2] {
                    out.push((i, node(nc, x, y, z + 1), 2));
                }
            }
        }
    }
    out
}

struct Dirichlet {
    weights: Vec<f64>,
    edges: Vec<(usize, usize, f64)>,
    diagonal: Vec<f64>,
}

impl Dirichlet {
    fn new(nc: [usize; 3], spacing: [f64; 3], weights: &[f64], length: f64) -> Self {
        let floor = weight_floor(weights);
        let weights: Vec<f64> = weights.iter().map(|w| w.max(floor)).collect();
        let l2 = length * length;
        let edges: Vec<(usize, usize, f64)> = edges(nc)
            .into_iter()
            .map(|(i, j, a)| (i, j, l2 * 0.5 * (weights[i] + weights[j]) / (spacing[a] * spacing[a])))
            .collect();
        let mut diagonal = weights.clone();
        for (i, j, k) in &edges {
            diagonal[*i] += k;
            diagonal[*j] += k;
        }
        Self { weights, edges, diagonal }
    }

    fn apply(&self, x: &[f64]) -> Vec<f64> {
        let mut y: Vec<f64> = x.iter().zip(&self.weights).map(|(v, w)| v * w).collect();
        for (i, j, k) in &self.edges {
            let d = k * (x[*i] - x[*j]);
            y[*i] += d;
            y[*j] -= d;
        }
        y
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub fn dirichlet_riesz(
    nc: [usize; 3],
    spacing: [f64; 3],
    weights: &[f64],
    length: f64,
    g: &[f64],
) -> GResult<Vec<f64>> {
    let n = nc[0] * nc[1] * nc[2];
    if weights.len() != n || g.len() != n {
        return invalid("descent metric received a covector of another control grid");
    }
    if !spacing.iter().all(|h| h.is_finite() && *h > 0.0) || !length.is_finite() || length < 0.0 {
        return invalid("descent metric requires positive control spacings and a nonnegative length");
    }
    let metric = Dirichlet::new(nc, spacing, weights, length);
    if length == 0.0 {
        return Ok(g.iter().zip(&metric.weights).map(|(v, w)| v / w).collect());
    }
    let norm_g = dot(g, g).sqrt();
    let mut x = vec![0.0; n];
    if norm_g == 0.0 {
        return Ok(x);
    }
    let mut r = g.to_vec();
    let mut z: Vec<f64> = r.iter().zip(&metric.diagonal).map(|(v, d)| v / d).collect();
    let mut p = z.clone();
    let mut rz = dot(&r, &z);
    for _ in 0..(10 * n).max(50) {
        let ap = metric.apply(&p);
        let pap = dot(&p, &ap);
        if pap <= 0.0 || !pap.is_finite() {
            return invalid("descent metric lost positive definiteness");
        }
        let alpha = rz / pap;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        if dot(&r, &r).sqrt() <= CG_TOLERANCE * norm_g {
            return Ok(x);
        }
        z = r.iter().zip(&metric.diagonal).map(|(v, d)| v / d).collect();
        let rz_next = dot(&r, &z);
        let beta = rz_next / rz;
        rz = rz_next;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
    }
    invalid("descent metric solve did not converge")
}

#[must_use]
pub fn logistic_slope(x: f64) -> f64 {
    let s = crate::scalar::logistic(x);
    (s * (1.0 - s)).max(f64::MIN_POSITIVE)
}

pub type SparseRow = Vec<(usize, f64)>;

struct PriorBlocks {
    inverse: Vec<f64>,
    k: usize,
    nodes: Vec<(Vec<usize>, f64)>,
}

pub struct PullbackMetric {
    rows: Vec<(f64, SparseRow)>,
    transport: Option<TransportOperator>,
    damping_diagonal: Vec<f64>,
    diagonal: Vec<f64>,
    prior: Option<PriorBlocks>,
}

pub struct TransportOperator {
    rows: Vec<(usize, SparseRow)>,
    shape: [usize; 3],
    screening: Vec<f64>,
    faces: [Vec<f64>; 3],
    diagonal: Vec<f64>,
}

impl TransportOperator {
    pub fn new(
        rows: Vec<(usize, SparseRow)>,
        shape: [usize; 3],
        spacing_mm: [f64; 3],
        occupancy: &[f64],
        length_mm: f64,
    ) -> GResult<Self> {
        let cells: usize = shape.iter().product();
        if occupancy.len() != cells {
            return invalid("transport metric: occupancy does not match the analysis grid");
        }
        if !(length_mm.is_finite() && length_mm > 0.0) || spacing_mm.iter().any(|h| !(h.is_finite() && *h > 0.0)) {
            return invalid("transport metric needs a positive transport length and cell spacing");
        }
        if rows.iter().any(|(c, _)| *c >= cells) {
            return invalid("transport metric: a Jacobian row names a cell outside the analysis grid");
        }
        let mobility: Vec<f64> = occupancy
            .iter()
            .map(|r| {
                let r = r.clamp(0.0, 1.0);
                r * (1.0 - r) + FISHER_RAO_EPS
            })
            .collect();
        let strides = [shape[1] * shape[2], shape[2], 1];
        let mut faces: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; cells]);
        for c in 0..cells {
            let index = [c / strides[0], (c / strides[1]) % shape[1], c % shape[2]];
            for a in 0..3 {
                if index[a] + 1 < shape[a] {
                    let n = c + strides[a];
                    let harmonic = 2.0 * mobility[c] * mobility[n] / (mobility[c] + mobility[n]);
                    faces[a][c] = harmonic / (spacing_mm[a] * spacing_mm[a]);
                }
            }
        }
        let screening: Vec<f64> = mobility.iter().map(|m| m / (length_mm * length_mm)).collect();
        let mut diagonal = screening.clone();
        for (a, stride) in strides.iter().enumerate() {
            for (c, f) in faces[a].iter().enumerate() {
                if *f > 0.0 {
                    diagonal[c] += f;
                    diagonal[c + stride] += f;
                }
            }
        }
        Ok(Self { rows, shape, screening, faces, diagonal })
    }

    fn cells(&self) -> usize {
        self.shape.iter().product()
    }

    fn apply(&self, phi: &[f64]) -> Vec<f64> {
        let strides = [self.shape[1] * self.shape[2], self.shape[2], 1];
        let mut y: Vec<f64> = phi.iter().zip(&self.screening).map(|(p, s)| p * s).collect();
        for (a, stride) in strides.iter().enumerate() {
            for (c, f) in self.faces[a].iter().enumerate() {
                if *f > 0.0 {
                    let flux = f * (phi[c] - phi[c + stride]);
                    y[c] += flux;
                    y[c + stride] -= flux;
                }
            }
        }
        y
    }

    fn solve(&self, u: &[f64]) -> GResult<Vec<f64>> {
        let n = self.cells();
        let norm_u = dot(u, u).sqrt();
        let mut x = vec![0.0; n];
        if norm_u == 0.0 {
            return Ok(x);
        }
        let mut r = u.to_vec();
        let mut z: Vec<f64> = r.iter().zip(&self.diagonal).map(|(v, d)| v / d).collect();
        let mut p = z.clone();
        let mut rz = dot(&r, &z);
        for _ in 0..(20 * n).max(100) {
            let ap = self.apply(&p);
            let pap = dot(&p, &ap);
            if pap <= 0.0 || !pap.is_finite() {
                return invalid("transport metric lost positive definiteness");
            }
            let alpha = rz / pap;
            for i in 0..n {
                x[i] += alpha * p[i];
                r[i] -= alpha * ap[i];
            }
            if dot(&r, &r).sqrt() <= TRANSPORT_TOLERANCE * norm_u {
                return Ok(x);
            }
            z = r.iter().zip(&self.diagonal).map(|(v, d)| v / d).collect();
            let rz_next = dot(&r, &z);
            let beta = rz_next / rz;
            rz = rz_next;
            for i in 0..n {
                p[i] = z[i] + beta * p[i];
            }
        }
        invalid("transport metric solve did not converge")
    }

    fn add_diagonal(&self, diagonal: &mut [f64]) -> GResult<()> {
        let mut columns: Vec<Vec<(usize, f64)>> = vec![Vec::new(); diagonal.len()];
        for (c, row) in &self.rows {
            for (i, v) in row {
                columns[*i].push((*c, *v));
            }
        }
        let mut u = vec![0.0; self.cells()];
        for (i, column) in columns.iter().enumerate() {
            if column.is_empty() {
                continue;
            }
            for (c, v) in column {
                u[*c] += v;
            }
            let phi = self.solve(&u)?;
            diagonal[i] += column.iter().map(|(c, _)| u[*c] * phi[*c]).sum::<f64>();
            for (c, _) in column {
                u[*c] = 0.0;
            }
        }
        Ok(())
    }

    fn add_apply(&self, x: &[f64], y: &mut [f64]) -> GResult<()> {
        let mut u = vec![0.0; self.cells()];
        for (c, row) in &self.rows {
            u[*c] += row.iter().map(|(i, v)| v * x[*i]).sum::<f64>();
        }
        let phi = self.solve(&u)?;
        for (c, row) in &self.rows {
            let s = phi[*c];
            if s != 0.0 {
                for (i, v) in row {
                    y[*i] += s * v;
                }
            }
        }
        Ok(())
    }
}

fn cholesky(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if s <= 0.0 || !s.is_finite() {
                    return None;
                }
                l[i * n + i] = s.sqrt();
            } else {
                l[i * n + j] = s / l[j * n + j];
            }
        }
    }
    Some(l)
}

fn spd_inverse(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let l = cholesky(a, n)?;
    let mut inv = vec![0.0; n * n];
    for col in 0..n {
        let mut y = vec![0.0; n];
        for i in 0..n {
            let mut s = if i == col { 1.0 } else { 0.0 };
            for k in 0..i {
                s -= l[i * n + k] * y[k];
            }
            y[i] = s / l[i * n + i];
        }
        for i in (0..n).rev() {
            let mut s = y[i];
            for k in (i + 1)..n {
                s -= l[k * n + i] * inv[k * n + col];
            }
            inv[i * n + col] = s / l[i * n + i];
        }
    }
    Some(inv)
}

impl PullbackMetric {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        entries: &[usize],
        components: &[usize],
        nodes: usize,
        rows: Vec<(f64, SparseRow)>,
        transport: Option<TransportOperator>,
        damping: f64,
        regularization: f64,
        node_weights: &[f64],
        prior: Option<&ChannelPrior>,
    ) -> GResult<Self> {
        let n = entries.len();
        let mut jtj = vec![0.0; n];
        for (w, row) in &rows {
            for (i, v) in row {
                jtj[*i] += w * v * v;
            }
        }
        if let Some(t) = &transport {
            if t.rows.iter().any(|(_, row)| row.iter().any(|(i, _)| *i >= n)) {
                return invalid("transport metric row names an entry outside the pullback block");
            }
            t.add_diagonal(&mut jtj)?;
        }
        let largest = jtj.iter().fold(0.0_f64, |m, v| m.max(*v));
        if largest <= 0.0 || !largest.is_finite() {
            return invalid("pullback metric: the block entries move no material");
        }
        let floor = PULLBACK_FLOOR * largest;
        #[allow(clippy::cast_precision_loss)]
        let mean_jtj = jtj.iter().sum::<f64>() / n as f64;
        #[allow(clippy::cast_precision_loss)]
        let mean_weight = node_weights.iter().sum::<f64>() / nodes as f64;
        let spans: Vec<f64> = control_components().iter().map(|c| c.upper - c.lower).collect();
        #[allow(clippy::cast_precision_loss)]
        let mean_span2 = components.iter().map(|c| spans[*c] * spans[*c]).sum::<f64>() / components.len() as f64;
        let damping_diagonal: Vec<f64> = entries
            .iter()
            .zip(&jtj)
            .map(|(e, d)| {
                let (c, node) = (e / nodes, e % nodes);
                let box_weight = mean_span2 / (spans[c] * spans[c]);
                damping * d + regularization * mean_jtj * node_weights[node] / mean_weight * box_weight + floor
            })
            .collect();
        let mut diagonal: Vec<f64> = jtj.iter().zip(&damping_diagonal).map(|(a, b)| a + b).collect();
        let prior = match prior {
            None => None,
            Some(p) => {
                let k = components.len();
                let sub: Vec<f64> = components
                    .iter()
                    .flat_map(|a| components.iter().map(move |b| p.covariance[a * COMPONENTS + b]))
                    .collect();
                let Some(inverse) = spd_inverse(&sub, k) else {
                    return invalid("descent metric prior covariance is not positive definite on the block");
                };
                #[allow(clippy::cast_precision_loss)]
                let mean_inverse = (0..k).map(|i| inverse[i * k + i]).sum::<f64>() / k as f64;
                let scale = p.weight * mean_jtj / mean_inverse;
                let position = |c: usize, node: usize| entries.iter().position(|e| *e == c * nodes + node);
                let mut blocks = Vec::with_capacity(nodes);
                for node in 0..nodes {
                    let positions: Option<Vec<usize>> = components.iter().map(|c| position(*c, node)).collect();
                    let Some(positions) = positions else {
                        return invalid("pullback metric block misses an entry of a prior node");
                    };
                    let factor = scale * node_weights[node] / mean_weight;
                    for (a, pa) in positions.iter().enumerate() {
                        diagonal[*pa] += factor * inverse[a * k + a];
                    }
                    blocks.push((positions, factor));
                }
                Some(PriorBlocks { inverse, k, nodes: blocks })
            }
        };
        Ok(Self { rows, transport, damping_diagonal, diagonal, prior })
    }

    fn apply(&self, x: &[f64]) -> GResult<Vec<f64>> {
        let mut y: Vec<f64> = x.iter().zip(&self.damping_diagonal).map(|(v, d)| v * d).collect();
        for (w, row) in &self.rows {
            let jx: f64 = row.iter().map(|(i, v)| v * x[*i]).sum();
            if jx != 0.0 {
                let s = w * jx;
                for (i, v) in row {
                    y[*i] += s * v;
                }
            }
        }
        if let Some(t) = &self.transport {
            t.add_apply(x, &mut y)?;
        }
        if let Some(p) = &self.prior {
            for (positions, factor) in &p.nodes {
                for (a, pa) in positions.iter().enumerate() {
                    let mut s = 0.0;
                    for (b, pb) in positions.iter().enumerate() {
                        s += p.inverse[a * p.k + b] * x[*pb];
                    }
                    y[*pa] += factor * s;
                }
            }
        }
        Ok(y)
    }

    pub fn riesz(&self, g: &[f64]) -> GResult<Vec<f64>> {
        let n = self.diagonal.len();
        if g.len() != n {
            return invalid("pullback metric received a covector of another block");
        }
        let norm_g = dot(g, g).sqrt();
        let mut x = vec![0.0; n];
        if norm_g == 0.0 {
            return Ok(x);
        }
        let mut r = g.to_vec();
        let mut z: Vec<f64> = r.iter().zip(&self.diagonal).map(|(v, d)| v / d).collect();
        let mut p = z.clone();
        let mut rz = dot(&r, &z);
        for _ in 0..(10 * n).max(50) {
            let ap = self.apply(&p)?;
            let pap = dot(&p, &ap);
            if pap <= 0.0 || !pap.is_finite() {
                return invalid("pullback metric lost positive definiteness");
            }
            let alpha = rz / pap;
            for i in 0..n {
                x[i] += alpha * p[i];
                r[i] -= alpha * ap[i];
            }
            if dot(&r, &r).sqrt() <= PULLBACK_TOLERANCE * norm_g {
                return Ok(x);
            }
            z = r.iter().zip(&self.diagonal).map(|(v, d)| v / d).collect();
            let rz_next = dot(&r, &z);
            let beta = rz_next / rz;
            rz = rz_next;
            for i in 0..n {
                p[i] = z[i] + beta * p[i];
            }
        }
        invalid("pullback metric solve did not converge")
    }
}

#[must_use]
pub fn material_weight(metric: MaterialMetric, fraction: f64) -> f64 {
    match metric {
        MaterialMetric::FisherRao | MaterialMetric::Transport => 1.0 / (fraction * (1.0 - fraction) + FISHER_RAO_EPS),
        MaterialMetric::L2 => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operator(length: f64) -> TransportOperator {
        let shape = [1, 6, 5];
        let occupancy: Vec<f64> = (0..30).map(|c| [0.02, 0.2, 0.5, 0.6, 0.85, 0.98][(c / 5) % 6]).collect();
        TransportOperator::new(Vec::new(), shape, [0.1, 0.1, 0.1], &occupancy, length).expect("operator")
    }

    fn pseudo_random(n: usize, seed: u64) -> Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                #[allow(clippy::cast_precision_loss)]
                let v = (state >> 11) as f64 / (1_u64 << 53) as f64;
                v - 0.5
            })
            .collect()
    }

    #[test]
    fn transport_operator_is_symmetric_positive_definite_and_solved() {
        let k = operator(0.2);
        let x = pseudo_random(30, 1);
        let y = pseudo_random(30, 2);
        let (kx, ky) = (k.apply(&x), k.apply(&y));
        assert!((dot(&x, &ky) - dot(&y, &kx)).abs() <= 1e-12 * dot(&x, &kx).abs().max(1.0));
        assert!(dot(&x, &kx) > 0.0);
        let phi = k.solve(&x).expect("solve");
        let back = k.apply(&phi);
        let error = back.iter().zip(&x).map(|(a, b)| (a - b) * (a - b)).sum::<f64>().sqrt();
        assert!(error <= 1e-10 * dot(&x, &x).sqrt());
    }

    #[test]
    fn transport_metric_measures_a_dipole_below_a_monopole() {
        let k = operator(0.3);
        let (a, b) = (2 * 5 + 2, 3 * 5 + 2);
        let mut dipole = vec![0.0; 30];
        dipole[a] = 1.0;
        dipole[b] = -1.0;
        let mut monopole = vec![0.0; 30];
        monopole[a] = 1.0;
        monopole[b] = 1.0;
        let norm = |u: &[f64]| dot(u, &k.solve(u).expect("solve"));
        assert!(norm(&dipole) < 0.3 * norm(&monopole), "{} {}", norm(&dipole), norm(&monopole));
        let long = operator(1e-3);
        let uniform = vec![1.0; 30];
        let phi = long.solve(&uniform).expect("solve");
        for (c, p) in phi.iter().enumerate() {
            let r = [0.02, 0.2, 0.5, 0.6, 0.85, 0.98][(c / 5) % 6];
            let fisher = 1e-6 / (r * (1.0 - r) + FISHER_RAO_EPS);
            assert!((p / fisher - 1.0).abs() < 0.05, "{c}: {p} {fisher}");
        }
    }

    #[test]
    fn pullback_metric_with_transport_is_a_descent() {
        let shape = [1, 6, 5];
        let occupancy: Vec<f64> = (0..30).map(|c| [0.02, 0.2, 0.5, 0.6, 0.85, 0.98][(c / 5) % 6]).collect();
        let (a, b) = (2 * 5 + 2, 3 * 5 + 2);
        let rows = vec![(a, vec![(0, 1.0), (1, 1.0)]), (b, vec![(0, -1.0), (1, 1.0)])];
        let t = TransportOperator::new(rows, shape, [0.1, 0.1, 0.1], &occupancy, 0.3).expect("operator");
        let entries = [2, 3];
        let components = [2, 3];
        let metric =
            PullbackMetric::new(&entries, &components, 1, Vec::new(), Some(t), 0.1, 0.0, &[1.0], None).expect("metric");
        let g = [1.0, 1.0];
        let r = metric.riesz(&g).expect("riesz");
        assert!(dot(&g, &r) > 0.0);
        assert!(r[0] > r[1], "{r:?}");
    }

    #[test]
    fn transport_diagonal_is_the_metric_norm_of_every_column() {
        let shape = [1, 6, 5];
        let occupancy: Vec<f64> = (0..30).map(|c| [0.02, 0.2, 0.5, 0.6, 0.85, 0.98][(c / 5) % 6]).collect();
        let rows: Vec<(usize, SparseRow)> =
            (10..15).map(|c| (c, vec![(0, 1.0), (1, 1.0)])).chain((15..20).map(|c| (c, vec![(0, 1.0), (1, -1.0)]))).collect();
        let t = TransportOperator::new(rows, shape, [0.1, 0.1, 0.1], &occupancy, 0.3).expect("operator");
        let mut diagonal = vec![0.0; 2];
        t.add_diagonal(&mut diagonal).expect("diagonal");
        for (entry, sign) in [(0, 1.0), (1, -1.0)] {
            let mut u = vec![0.0; 30];
            for c in 10..15 {
                u[c] = 1.0;
            }
            for c in 15..20 {
                u[c] = sign;
            }
            let expected = dot(&u, &t.solve(&u).expect("solve"));
            assert!((diagonal[entry] - expected).abs() <= 1e-9 * expected, "{entry}: {} {expected}", diagonal[entry]);
        }
        assert!(diagonal[1] < diagonal[0]);
    }

    #[test]
    fn transport_declaration_needs_its_length_and_the_pullback_mode() {
        let ok = DescentMetric::from_value(&json!({"mode": "pullback", "material_metric": "transport",
                                                    "transport_length_mm": 0.2}))
        .expect("transport metric");
        assert_eq!(ok.material, MaterialMetric::Transport);
        assert!((ok.transport_length_mm - 0.2).abs() < 1e-15);
        let wire = ok.to_value();
        assert_eq!(wire["material_metric"], json!("transport"));
        assert_eq!(DescentMetric::from_value(&wire).expect("round trip"), ok);
        for raw in [
            json!({"mode": "pullback", "material_metric": "transport"}),
            json!({"mode": "pullback", "material_metric": "transport", "transport_length_mm": 0.0}),
            json!({"mode": "pullback", "transport_length_mm": 0.2}),
            json!({"material_metric": "transport", "transport_length_mm": 0.2}),
        ] {
            assert!(DescentMetric::from_value(&raw).is_err(), "{raw}");
        }
        let fisher = DescentMetric::from_value(&json!({"mode": "pullback"})).expect("pullback");
        assert!(fisher.to_value().get("transport_length_mm").is_none());
    }
}
