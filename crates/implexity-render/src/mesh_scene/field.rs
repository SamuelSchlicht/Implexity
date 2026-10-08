// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::Value;

use super::{SceneError, number_array, vec3};

#[derive(Clone, Debug, PartialEq)]
pub struct GridField {
    pub origin_mm: [f64; 3],
    pub spacing_mm: [f64; 3],
    pub shape: [usize; 3],
    pub values: Vec<f64>,
}

impl GridField {


    pub fn from_json(name: &str, v: &Value) -> Result<Self, SceneError> {
        let bad = |m: &str| SceneError::Invalid(format!("field {name}: {m}"));
        let origin_mm = vec3(v.get("origin_mm"), &format!("field {name} origin_mm"))?;
        let spacing_mm = vec3(v.get("spacing_mm"), &format!("field {name} spacing_mm"))?;
        if spacing_mm.iter().any(|h| *h <= 0.0) {
            return Err(bad("spacing_mm must be positive"));
        }
        let shape_v = v.get("shape").and_then(Value::as_array).filter(|a| a.len() == 3);
        let shape_v = shape_v.ok_or_else(|| bad("shape must be three sample counts"))?;
        let mut shape = [0_usize; 3];
        for (a, s) in shape_v.iter().enumerate() {
            shape[a] = s
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n >= 1)
                .ok_or_else(|| bad("shape entries must be positive integers"))?;
        }
        let count = shape.iter().try_fold(1_usize, |n, size| n.checked_mul(*size))
            .ok_or_else(|| bad("shape sample count exceeds the addressable range"))?;
        let values = number_array(v.get("values"), &format!("field {name} values"))?;
        if values.len() != count {
            return Err(bad("values do not match shape"));
        }
        Ok(Self { origin_mm, spacing_mm, shape, values })
    }

    #[must_use]
    pub fn range(&self) -> [f64; 2] {
        let lo = self.values.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = self.values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        [lo, hi]
    }

    #[must_use]
    pub fn sample(&self, p: [f64; 3]) -> f64 {
        let mut i0 = [0_usize; 3];
        let mut w = [0.0_f64; 3];
        for a in 0..3 {
            let n = self.shape[a];
            if n == 1 {
                continue;
            }
            let x = ((p[a] - self.origin_mm[a]) / self.spacing_mm[a]).clamp(0.0, (n - 1) as f64);
            let f = x.floor().min((n - 2) as f64);
            i0[a] = f as usize;
            w[a] = x - f;
        }
        let at = |i: usize, j: usize, k: usize| {
            let i = (i0[0] + i).min(self.shape[0] - 1);
            let j = (i0[1] + j).min(self.shape[1] - 1);
            let k = (i0[2] + k).min(self.shape[2] - 1);
            self.values[(i * self.shape[1] + j) * self.shape[2] + k]
        };
        let mut v = 0.0;
        for (di, wi) in [(0, 1.0 - w[0]), (1, w[0])] {
            for (dj, wj) in [(0, 1.0 - w[1]), (1, w[1])] {
                for (dk, wk) in [(0, 1.0 - w[2]), (1, w[2])] {
                    let wt = wi * wj * wk;
                    if wt != 0.0 {
                        v += wt * at(di, dj, dk);
                    }
                }
            }
        }
        v
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Colormap {
    stops: Vec<(f64, [f64; 3])>,
}

impl Colormap {


    pub fn from_json(v: Option<&Value>, label: &str) -> Result<Self, SceneError> {
        let bad = || SceneError::Invalid(format!("{label} must be [[t, [r, g, b]], ...] with t from 0 to 1"));
        let items = v.and_then(Value::as_array).filter(|a| a.len() >= 2).ok_or_else(bad)?;
        let mut stops = Vec::with_capacity(items.len());
        for item in items {
            let pair = item.as_array().filter(|p| p.len() == 2).ok_or_else(bad)?;
            let t = pair[0].as_f64().filter(|t| t.is_finite()).ok_or_else(bad)?;
            let rgb = vec3(Some(&pair[1]), label)?;
            if rgb.iter().any(|c| !(0.0..=255.0).contains(c)) {
                return Err(bad());
            }
            stops.push((t, rgb));
        }
        let increasing = stops.windows(2).all(|w| w[1].0 > w[0].0);
        if !increasing || stops[0].0.abs() > 1e-12 || (stops[stops.len() - 1].0 - 1.0).abs() > 1e-12 {
            return Err(bad());
        }
        Ok(Self { stops })
    }

    #[must_use]
    pub fn at(&self, t: f64) -> [f64; 3] {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        for w in self.stops.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if t <= t1 {
                let s = (t - t0) / (t1 - t0);
                return std::array::from_fn(|a| c0[a] + s * (c1[a] - c0[a]));
            }
        }
        self.stops[self.stops.len() - 1].1
    }
}
