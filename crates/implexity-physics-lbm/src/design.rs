// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Map, Value, json};

use crate::d3q19::Grid;
use crate::nparray::asarray;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TopologyMap {
    pub filter_radius_m: f64,
    pub projection_beta: f64,
    pub projection_eta: f64,
}

impl Default for TopologyMap {
    fn default() -> Self {
        Self { filter_radius_m: 0.0, projection_beta: 0.0, projection_eta: 0.5 }
    }
}

impl TopologyMap {

    pub fn normalize(config: &Value, spacing: f64) -> CaeResult<Self> {
        const KEYS: [&str; 3] = ["filter_radius_m", "projection_beta", "projection_eta"];
        let Some(map) =
            config.as_object().filter(|m| m.len() == 3 && KEYS.iter().all(|k| m.contains_key(*k)))
        else {
            return Err(CaeError::contract(
                "topology_map requires filter_radius_m, projection_beta and projection_eta",
            ));
        };
        let mut out = Map::new();
        for (key, value) in map {
            let Some(v) = asarray(value).real_scalar() else {
                return Err(CaeError::contract(format!("topology_map.{key} must be finite real scalar")));
            };
            out.insert(key.clone(), json!(v));
        }
        let get = |k: &str| out.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let result = Self {
            filter_radius_m: get("filter_radius_m"),
            projection_beta: get("projection_beta"),
            projection_eta: get("projection_eta"),
        };
        if !(0.0 <= result.filter_radius_m && result.filter_radius_m <= 4.0 * spacing) {
            return Err(CaeError::contract("filter_radius_m must lie in [0,4*spacing_m]"));
        }
        if !(0.0 <= result.projection_beta && result.projection_beta <= 32.0)
            || !(0.0 < result.projection_eta && result.projection_eta < 1.0)
        {
            return Err(CaeError::contract("Require beta in [0,32] and eta in (0,1)"));
        }
        Ok(result)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        json!({
            "filter_radius_m": self.filter_radius_m,
            "projection_beta": self.projection_beta,
            "projection_eta": self.projection_eta,
        })
    }
}

#[derive(Clone, Debug)]
pub struct DesignMap {
    grid: Grid,
    solid: Vec<bool>,
    editable: Vec<bool>,
    fixed: Vec<f64>,
    rows: Option<Vec<Vec<(usize, f64)>>>,
    denominators: Vec<f64>,
    beta: f64,
    eta: f64,
}

impl DesignMap {
    #[must_use]
    pub fn new(
        grid: Grid,
        periodic: [bool; 3],
        solid: &[bool],
        design_region: &[bool],
        fixed_design: &[f64],
        spacing_m: f64,
        map: &TopologyMap,
    ) -> Self {
        let n = grid.cells();
        let editable: Vec<bool> = (0..n).map(|x| design_region[x] && !solid[x]).collect();
        let radius = map.filter_radius_m / spacing_m;
        let mut denominators = vec![1.0; n];
        let rows = if radius > 0.0 {
            #[allow(clippy::cast_possible_truncation)]
            let reach = radius.ceil() as i64;
            let mut offsets = Vec::new();
            for a in -reach..=reach {
                for b in -reach..=reach {
                    for c in -reach..=reach {
                        #[allow(clippy::cast_precision_loss)]
                        let norm = ((a * a + b * b + c * c) as f64).sqrt();
                        let weight = (radius - norm).max(0.0);
                        if weight != 0.0 {
                            offsets.push(([a, b, c], weight));
                        }
                    }
                }
            }
            let mut rows = Vec::with_capacity(n);
            for x in 0..n {
                let mut terms = Vec::new();
                let mut den = 0.0;
                for (offset, weight) in &offsets {
                    let back = [-offset[0], -offset[1], -offset[2]];
                    if !grid.inside(x, back, periodic) {
                        continue;
                    }
                    let source = grid.wrap(x, back);
                    if !editable[source] {
                        continue;
                    }
                    terms.push((source, *weight));
                    den += weight;
                }
                denominators[x] = if den > 0.0 { den } else { 1.0 };
                rows.push(terms);
            }
            Some(rows)
        } else {
            None
        };
        Self {
            grid,
            solid: solid.to_vec(),
            editable,
            fixed: fixed_design.to_vec(),
            rows,
            denominators,
            beta: map.projection_beta,
            eta: map.projection_eta,
        }
    }

    #[must_use]
    pub fn editable(&self) -> &[bool] {
        &self.editable
    }

    fn filtered(&self, raw: &[f64], x: usize) -> f64 {
        match &self.rows {
            None => raw[x],
            Some(rows) => {
                let mut numerator = 0.0;
                for (source, weight) in &rows[x] {
                    numerator += weight * raw[*source];
                }
                numerator / self.denominators[x]
            }
        }
    }

    fn projection(&self) -> Option<(f64, f64)> {
        (self.beta > 0.0).then(|| {
            let a = (self.beta * self.eta).tanh();
            (a, a + (self.beta * (1.0 - self.eta)).tanh())
        })
    }

    #[must_use]
    pub fn map(&self, raw: &[f64]) -> Vec<f64> {
        let proj = self.projection();
        (0..self.grid.cells())
            .map(|x| {
                if self.solid[x] {
                    0.0
                } else if self.editable[x] {
                    let f = self.filtered(raw, x);
                    match proj {
                        Some((a, den)) => (a + (self.beta * (f - self.eta)).tanh()) / den,
                        None => f,
                    }
                } else {
                    self.fixed[x]
                }
            })
            .collect()
    }

    #[must_use]
    pub fn vjp(&self, raw: &[f64], phi_bar: &[f64]) -> Vec<f64> {
        let n = self.grid.cells();
        let proj = self.projection();
        let mut out = vec![0.0; n];
        for x in 0..n {
            if !self.editable[x] || phi_bar[x] == 0.0 {
                continue;
            }
            let mut g = phi_bar[x];
            if let Some((_, den)) = proj {
                let t = (self.beta * (self.filtered(raw, x) - self.eta)).tanh();
                g *= self.beta * (1.0 - t * t) / den;
            }
            match &self.rows {
                None => out[x] += g,
                Some(rows) => {
                    let scale = g / self.denominators[x];
                    for (source, weight) in &rows[x] {
                        out[*source] += weight * scale;
                    }
                }
            }
        }
        out
    }


    pub fn partial(&self, raw: &[f64]) -> CaeResult<CsrMatrix> {
        let n = self.grid.cells();
        let proj = self.projection();
        let mut indptr = vec![0usize];
        let mut indices = Vec::new();
        let mut data = Vec::new();
        for x in 0..n {
            if self.editable[x] {
                let slope = proj.map_or(1.0, |(_, den)| {
                    let t = (self.beta * (self.filtered(raw, x) - self.eta)).tanh();
                    self.beta * (1.0 - t * t) / den
                });
                let mut entries: Vec<(usize, f64)> = match &self.rows {
                    None => vec![(x, 1.0)],
                    Some(rows) => rows[x].iter().map(|(s, w)| (*s, w / self.denominators[x])).collect(),
                };
                entries.sort_by_key(|e| e.0);
                let mut merged: Vec<(usize, f64)> = Vec::with_capacity(entries.len());
                for (col, value) in entries {
                    match merged.last_mut() {
                        Some(last) if last.0 == col => last.1 += value,
                        _ => merged.push((col, value)),
                    }
                }
                for (col, value) in merged {
                    indices.push(col);
                    data.push(slope * value);
                }
            }
            indptr.push(indices.len());
        }
        CsrMatrix::try_new(n, n, indptr, indices, data)
            .map_err(|e| CaeError::contract(format!("invalid design map partial: {e}")))
    }
}

#[inline]
#[must_use]
pub fn resistance(step_s: f64, drag_max_per_s: f64, drag_shape: f64, phi: f64) -> f64 {
    step_s * drag_max_per_s * drag_shape * (1.0 - phi) / (drag_shape + phi)
}

#[inline]
#[must_use]
pub fn resistance_derivative(step_s: f64, drag_max_per_s: f64, drag_shape: f64, phi: f64) -> f64 {
    let q = drag_shape;
    let d = q + phi;
    step_s * drag_max_per_s * q * (-d - (1.0 - phi)) / (d * d)
}

