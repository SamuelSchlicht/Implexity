// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use crate::error::{AResult, AuthoringError};
use crate::py::{Arr, repr};
use crate::spatial_values::{ArrayMutation, persist_array_parameter, resolve_array_parameter};

fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("SpatialValueError", message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrushStroke {
    pub center: [f64; 3],
    pub radius: f64,
    pub strength: f64,
    pub mode: String,
    pub target: Option<f64>,
    pub falloff: String,
    pub axes: [f64; 3],
}

impl BrushStroke {
    #[must_use]
    pub fn new(center: [f64; 3], radius: f64, strength: f64) -> Self {
        Self {
            center,
            radius,
            strength,
            mode: "add".into(),
            target: None,
            falloff: "smoothstep".into(),
            axes: [1.0; 3],
        }
    }


    pub fn validate(&self) -> AResult<()> {
        if self.radius <= 0.0 || !self.radius.is_finite() {
            return Err(err("Brush radius must be finite and positive"));
        }
        if !self.strength.is_finite() {
            return Err(err("Brush strength must be finite"));
        }
        if !["add", "subtract", "set", "smooth"].contains(&self.mode.as_str()) {
            return Err(err(format!("Unsupported brush mode {}", repr(&Value::from(self.mode.clone())))));
        }
        if self.mode == "set" && !self.target.is_some_and(f64::is_finite) {
            return Err(err("Set brush requires a finite target"));
        }
        if !["linear", "smoothstep", "gaussian", "constant"].contains(&self.falloff.as_str()) {
            return Err(err(format!(
                "Unsupported brush falloff {}",
                repr(&Value::from(self.falloff.clone()))
            )));
        }
        if self.axes.iter().any(|v| *v <= 0.0 || !v.is_finite()) {
            return Err(err("Brush axes must contain three finite positive values"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrushGrid {
    pub shape: [usize; 3],
    pub origin: [f64; 3],
    pub spacing: [f64; 3],
}

impl BrushGrid {
    #[must_use]
    pub fn coordinates(&self) -> [Vec<f64>; 3] {
        let axes: Vec<Vec<f64>> = (0..3)
            .map(|a| (0..self.shape[a]).map(|i| self.origin[a] + i as f64 * self.spacing[a]).collect())
            .collect();
        let n: usize = self.shape.iter().product();
        let mut out = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
        for i in 0..self.shape[0] {
            for j in 0..self.shape[1] {
                for k in 0..self.shape[2] {
                    out[0].push(axes[0][i]);
                    out[1].push(axes[1][j]);
                    out[2].push(axes[2][k]);
                }
            }
        }
        out
    }
}

fn falloff(q: f64, kind: &str) -> f64 {
    let inside = q < 1.0;
    match kind {
        "constant" => {
            if inside {
                1.0
            } else {
                0.0
            }
        }
        "linear" => {
            if inside {
                1.0 - q
            } else {
                0.0
            }
        }
        "gaussian" => {
            let raw = (-4.5 * q * q).exp();
            let edge = (-4.5f64).exp();
            if inside { (raw - edge) / (1.0 - edge) } else { 0.0 }
        }
        _ => {
            let t = crate::py::clip(1.0 - q, 0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        }
    }
}

fn smooth_local(array: &[f64], shape: [usize; 3]) -> Vec<f64> {
    let n = array.len();
    let mut total = vec![0.0; n];
    let mut count = vec![0.0; n];
    for axis in 0..3 {
        for shift in [-1i64, 0, 1] {
            for i in 0..shape[0] {
                for j in 0..shape[1] {
                    for k in 0..shape[2] {
                        let c = [i, j, k];
                        let f = (i * shape[1] + j) * shape[2] + k;
                        let len = shape[axis] as i64;
                        let src = (c[axis] as i64 - shift).rem_euclid(len) as usize;
                        let mut s = c;
                        s[axis] = src;
                        let shifted = array[(s[0] * shape[1] + s[1]) * shape[2] + s[2]];
                        let valid = match shift.cmp(&0) {
                            std::cmp::Ordering::Less => {
                                if c[axis] as i64 >= len + shift {
                                    0.0
                                } else {
                                    1.0
                                }
                            }
                            std::cmp::Ordering::Greater => {
                                if (c[axis] as i64) < shift {
                                    0.0
                                } else {
                                    1.0
                                }
                            }
                            std::cmp::Ordering::Equal => 1.0,
                        };
                        total[f] += shifted * valid;
                        count[f] += valid;
                    }
                }
            }
        }
    }
    total.iter().zip(&count).map(|(t, c)| t / c.max(1.0)).collect()
}


pub fn apply_strokes(
    baseline: &[f64],
    grid: &BrushGrid,
    strokes: &[BrushStroke],
    lower: Option<f64>,
    upper: Option<f64>,
) -> AResult<Vec<f64>> {
    let mut result = baseline.to_vec();
    let [x, y, z] = grid.coordinates();
    for s in strokes {
        s.validate()?;
        let amount = s.strength.abs();
        let weights: Vec<f64> = (0..result.len())
            .map(|i| {
                let dx = (x[i] - s.center[0]) / (s.radius * s.axes[0]);
                let dy = (y[i] - s.center[1]) / (s.radius * s.axes[1]);
                let dz = (z[i] - s.center[2]) / (s.radius * s.axes[2]);
                falloff((dx * dx + dy * dy + dz * dz).sqrt(), &s.falloff)
            })
            .collect();
        match s.mode.as_str() {
            "add" => result.iter_mut().zip(&weights).for_each(|(r, w)| *r += w * amount),
            "subtract" => result.iter_mut().zip(&weights).for_each(|(r, w)| *r -= w * amount),
            "set" => {
                let target = s.target.unwrap_or(0.0);
                for (r, w) in result.iter_mut().zip(&weights) {
                    let alpha = crate::py::clip(w * amount, 0.0, 1.0);
                    *r += alpha * (target - *r);
                }
            }
            _ => {
                let smoothed = smooth_local(&result, grid.shape);
                for ((r, w), sm) in result.iter_mut().zip(&weights).zip(&smoothed) {
                    let alpha = crate::py::clip(w * amount, 0.0, 1.0);
                    *r += alpha * (sm - *r);
                }
            }
        }
        if lower.is_some() || upper.is_some() {
            let lo = lower.unwrap_or(f64::NEG_INFINITY);
            let hi = upper.unwrap_or(f64::INFINITY);
            for r in &mut result {
                *r = crate::field_interaction::np_clip(*r, lo, hi);
            }
        }
    }
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct FieldBrushTransaction {
    pub document_at_begin: Value,
    pub node_id: String,
    pub parameter: String,
    pub grid: BrushGrid,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub share_policy: String,
    pub strokes: Vec<BrushStroke>,
    pub baseline: Arr,
    closed: bool,
}

impl FieldBrushTransaction {

    pub fn new(
        document: &Value,
        node_id: &str,
        parameter: &str,
        grid: BrushGrid,
        lower: Option<f64>,
        upper: Option<f64>,
        share_policy: &str,
    ) -> AResult<Self> {
        let baseline = resolve_array_parameter(document, node_id, parameter)?;
        if baseline.shape != grid.shape {
            return Err(err(format!(
                "Field shape {} does not match grid {}",
                crate::field_interaction::shape_repr(&baseline.shape),
                crate::field_interaction::shape_repr(&grid.shape)
            )));
        }
        Ok(Self {
            document_at_begin: document.clone(),
            node_id: node_id.into(),
            parameter: parameter.into(),
            grid,
            lower,
            upper,
            share_policy: share_policy.into(),
            strokes: Vec::new(),
            baseline,
            closed: false,
        })
    }

    fn ensure_open(&self) -> AResult<()> {
        if self.closed { Err(err("Field-brush transaction is already closed")) } else { Ok(()) }
    }

    #[must_use]
    pub fn closed(&self) -> bool {
        self.closed
    }


    pub fn set_strokes(&mut self, strokes: &[BrushStroke]) -> AResult<()> {
        self.ensure_open()?;
        for s in strokes {
            s.validate()?;
        }
        self.strokes = strokes.to_vec();
        Ok(())
    }


    pub fn add_stroke(&mut self, stroke: BrushStroke) -> AResult<()> {
        self.ensure_open()?;
        stroke.validate()?;
        self.strokes.push(stroke);
        Ok(())
    }


    pub fn preview_array(&self) -> AResult<Arr> {
        self.ensure_open()?;
        let data = apply_strokes(&self.baseline.data, &self.grid, &self.strokes, self.lower, self.upper)?;
        Ok(Arr::new(self.baseline.shape.clone(), data))
    }


    pub fn preview_document(&self) -> AResult<ArrayMutation> {
        let arr = self.preview_array()?;
        persist_array_parameter(
            &self.document_at_begin,
            &self.node_id,
            &self.parameter,
            &arr,
            &self.share_policy,
            None,
        )
    }


    pub fn commit(&mut self) -> AResult<ArrayMutation> {
        let mutation = self.preview_document()?;
        self.closed = true;
        Ok(mutation)
    }


    pub fn cancel(&mut self) -> AResult<Value> {
        self.ensure_open()?;
        self.closed = true;
        Ok(self.document_at_begin.clone())
    }
}
