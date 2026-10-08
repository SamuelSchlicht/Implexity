// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::document::json::canonical;
use crate::error::{GResult, GeometryError};
use crate::linalg3::{det, matvec, solve};

#[derive(Clone, Debug, PartialEq)]
pub struct GridRegistration {
    pub shape: [usize; 3],
    pub origin: [f64; 3],
    pub basis: [[f64; 3]; 3],
    pub centering: String,
    pub axis_order: String,
    pub frame: String,
}

fn verr(m: &str) -> GeometryError {
    GeometryError::Value(m.into())
}

impl GridRegistration {

    pub fn new(
        shape: [usize; 3],
        origin: [f64; 3],
        basis: [[f64; 3]; 3],
        centering: &str,
        axis_order: &str,
        frame: &str,
    ) -> GResult<Self> {
        if shape.contains(&0) {
            return Err(verr("shape must contain three positive dimensions"));
        }
        if centering != "cell" && centering != "node" {
            return Err(verr("centering must be 'cell' or 'node'"));
        }
        let mut ax: Vec<char> = axis_order.chars().collect();
        ax.sort_unstable();
        if ax != ['x', 'y', 'z'] {
            return Err(verr("axis_order must be a permutation of xyz"));
        }
        let r = Self {
            shape,
            origin,
            basis,
            centering: centering.into(),
            axis_order: axis_order.into(),
            frame: frame.into(),
        };
        let m = r.matrix();
        if !m.iter().flatten().all(|v| v.is_finite()) {
            return Err(verr("basis must be a finite 3x3 matrix"));
        }
        let d = det(&m);
        if !d.is_finite() || d.abs() < 1e-15 {
            return Err(verr("basis must be invertible"));
        }
        if !origin.iter().all(|v| v.is_finite()) {
            return Err(verr("origin must be finite"));
        }
        Ok(r)
    }

    #[must_use]
    pub fn matrix(&self) -> [[f64; 3]; 3] {
        std::array::from_fn(|i| std::array::from_fn(|j| self.basis[j][i]))
    }

    #[must_use]
    pub fn offset(&self) -> [f64; 3] {
        let shift = if self.centering == "cell" { 0.5 } else { 0.0 };
        let m = matvec(&self.matrix(), &[shift; 3]);
        std::array::from_fn(|i| self.origin[i] + m[i])
    }

    #[must_use]
    pub fn index_to_world(&self, index: [f64; 3]) -> [f64; 3] {
        let o = self.offset();
        let m = matvec(&self.matrix(), &index);
        std::array::from_fn(|i| o[i] + m[i])
    }


    pub fn world_to_index(&self, point: [f64; 3]) -> GResult<[f64; 3]> {
        let o = self.offset();
        let rhs = std::array::from_fn(|i| point[i] - o[i]);
        solve(&self.matrix(), &rhs).ok_or_else(|| verr("Singular matrix"))
    }


    pub fn contains_world(&self, point: [f64; 3], tolerance: f64) -> GResult<bool> {
        let idx = self.world_to_index(point)?;
        #[allow(clippy::cast_precision_loss)]
        Ok((0..3).all(|a| idx[a] >= -tolerance && idx[a] <= self.shape[a] as f64 - 1.0 + tolerance))
    }


    pub fn nearest_index(&self, point: [f64; 3], clip: bool) -> GResult<[i64; 3]> {
        let idx = self.world_to_index(point)?;
        #[allow(clippy::cast_possible_truncation)]
        let mut r: [i64; 3] = idx.map(|v| v.round_ties_even() as i64);
        #[allow(clippy::cast_possible_wrap)]
        let upper: [i64; 3] = self.shape.map(|n| n as i64 - 1);
        if clip {
            for a in 0..3 {
                r[a] = r[a].max(0).min(upper[a]);
            }
        } else if (0..3).any(|a| r[a] < 0 || r[a] > upper[a]) {
            return Err(verr("point lies outside registered field"));
        }
        Ok(r)
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        let mut wire = json!({
            "schema": "implexity-grid-registration/1",
            "shape": self.shape,
            "origin": self.origin,
            "basis": self.basis,
            "centering": self.centering,
            "axis_order": self.axis_order,
            "frame": self.frame,
        });
        let id = hex::encode(Sha256::digest(canonical(&wire).as_bytes()));
        if let Value::Object(m) = &mut wire {
            m.insert("registration_id".into(), Value::String(id));
        }
        wire
    }


    pub fn from_wire(value: &Value) -> GResult<Self> {
        match value.get("schema") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) if s == "implexity-grid-registration/1" => {}
            _ => return Err(verr("unsupported grid registration schema")),
        }
        let nums = |k: &str| -> GResult<Vec<f64>> {
            value
                .get(k)
                .and_then(Value::as_array)
                .ok_or_else(|| GeometryError::Value(format!("missing {k}")))?
                .iter()
                .map(|v| v.as_f64().ok_or_else(|| verr("could not convert to float")))
                .collect()
        };
        let shape = nums("shape")?;
        let origin = nums("origin")?;
        let basis: Vec<Vec<f64>> = value
            .get("basis")
            .and_then(Value::as_array)
            .ok_or_else(|| verr("missing basis"))?
            .iter()
            .map(|row| {
                row.as_array()
                    .map(|r| r.iter().filter_map(Value::as_f64).collect())
                    .ok_or_else(|| verr("basis must be a finite 3x3 matrix"))
            })
            .collect::<GResult<_>>()?;
        if shape.len() != 3 || shape.iter().any(|v| *v <= 0.0) {
            return Err(verr("shape must contain three positive dimensions"));
        }
        if origin.len() != 3 || basis.len() != 3 || basis.iter().any(|r| r.len() != 3) {
            return Err(verr("basis must be a finite 3x3 matrix"));
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let shape = [shape[0] as usize, shape[1] as usize, shape[2] as usize];
        let s = |k: &str, d: &str| value.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        Self::new(
            shape,
            [origin[0], origin[1], origin[2]],
            std::array::from_fn(|i| [basis[i][0], basis[i][1], basis[i][2]]),
            &s("centering", "cell"),
            &s("axis_order", "xyz"),
            &s("frame", "model"),
        )
    }
}


pub fn axis_aligned_registration(
    shape: [usize; 3],
    bounds_min: [f64; 3],
    bounds_max: [f64; 3],
    centering: &str,
) -> GResult<GridRegistration> {
    if (0..3).any(|a| bounds_max[a] <= bounds_min[a]) {
        return Err(verr("bounds_max must be greater than bounds_min"));
    }
    #[allow(clippy::cast_precision_loss)]
    let denom: [f64; 3] =
        shape.map(|n| if centering == "cell" { n as f64 } else { n.saturating_sub(1).max(1) as f64 });
    let step: [f64; 3] = std::array::from_fn(|a| (bounds_max[a] - bounds_min[a]) / denom[a]);
    let basis = [[step[0], 0.0, 0.0], [0.0, step[1], 0.0], [0.0, 0.0, step[2]]];
    GridRegistration::new(shape, bounds_min, basis, centering, "xyz", "model")
}


pub fn trilinear_sample(
    values: &[f64],
    shape: [usize; 3],
    registration: &GridRegistration,
    point: [f64; 3],
) -> GResult<f64> {
    if shape != registration.shape || values.len() < shape.iter().product::<usize>() {
        return Err(verr("field shape and registration do not match"));
    }
    let q = registration.world_to_index(point)?;
    #[allow(clippy::cast_precision_loss)]
    if (0..3).any(|a| q[a] < 0.0 || q[a] > shape[a] as f64 - 1.0) {
        return Err(verr("sample point lies outside field"));
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let lower: [usize; 3] = q.map(|v| v.floor() as usize);
    let upper: [usize; 3] = std::array::from_fn(|a| (lower[a] + 1).min(shape[a] - 1));
    #[allow(clippy::cast_precision_loss)]
    let f: [f64; 3] = std::array::from_fn(|a| q[a] - lower[a] as f64);
    let mut result = 0.0;
    for bx in [false, true] {
        for by in [false, true] {
            for bz in [false, true] {
                let bits = [bx, by, bz];
                let idx: [usize; 3] = std::array::from_fn(|a| if bits[a] { upper[a] } else { lower[a] });
                let w = (if bx { f[0] } else { 1.0 - f[0] })
                    * (if by { f[1] } else { 1.0 - f[1] })
                    * (if bz { f[2] } else { 1.0 - f[2] });
                result += values[(idx[0] * shape[1] + idx[1]) * shape[2] + idx[2]] * w;
            }
        }
    }
    Ok(result)
}
