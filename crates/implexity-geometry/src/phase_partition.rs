// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use crate::error::{GResult, GeometryError};

pub const HELMHOLTZ_FILTER_SCHEMA: &str = "implexity-helmholtz-density-filter/1";

fn verr(m: &str) -> GeometryError {
    GeometryError::Value(m.into())
}

fn dct(n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut q = vec![0.0; n * n];
    #[allow(clippy::cast_precision_loss)]
    let nf = n as f64;
    for k in 0..n {
        let s = if k == 0 { (1.0 / nf).sqrt() } else { (2.0 / nf).sqrt() };
        for j in 0..n {
            #[allow(clippy::cast_precision_loss)]
            let arg = std::f64::consts::PI * k as f64 * (j as f64 + 0.5) / nf;
            q[k * n + j] = s * arg.cos();
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let lam = (0..n)
        .map(|k| if n == 1 { 0.0 } else { 2.0 - 2.0 * (std::f64::consts::PI * k as f64 / nf).cos() })
        .collect();
    (q, lam)
}

fn apply_axis(x: &[f64], shape: [usize; 3], axis: usize, m: &[f64], transpose: bool) -> Vec<f64> {
    let n = shape[axis];
    let mut out = vec![0.0; x.len()];
    let strides = [shape[1] * shape[2], shape[2], 1];
    let st = strides[axis];
    let mut line = vec![0.0; n];
    for base in 0..x.len() {
        let pos = (base / st) % n;
        if pos != 0 {
            continue;
        }
        for (i, l) in line.iter_mut().enumerate() {
            *l = x[base + i * st];
        }
        for r in 0..n {
            let mut s = 0.0;
            for (c, l) in line.iter().enumerate() {
                let w = if transpose { m[c * n + r] } else { m[r * n + c] };
                s += w * l;
            }
            out[base + r * st] = s;
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct HelmholtzDensityFilter {
    pub radius_mm: f64,
    pub length_mm: f64,
    pub grid: [usize; 3],
    bases: [(Vec<f64>, Vec<f64>); 3],
}

impl HelmholtzDensityFilter {

    pub fn new(radius_mm: f64, grid: &[usize]) -> GResult<Self> {
        if !radius_mm.is_finite() || radius_mm <= 0.0 {
            return Err(verr("density filter radius_mm must be finite and positive"));
        }
        if grid.len() != 3 || grid.contains(&0) {
            return Err(verr("density filter requires a three-dimensional cell grid"));
        }
        let grid = [grid[0], grid[1], grid[2]];
        Ok(Self { radius_mm, length_mm: radius_mm / (2.0 * 3f64.sqrt()), grid, bases: grid.map(dct) })
    }

    fn check_h(spacing_mm: &[f64]) -> GResult<[f64; 3]> {
        if spacing_mm.len() != 3 || !spacing_mm.iter().all(|h| h.is_finite() && *h > 0.0) {
            return Err(verr("density filter requires three positive finite spacings"));
        }
        Ok([spacing_mm[0], spacing_mm[1], spacing_mm[2]])
    }

    fn solve(&self, c: &[f64], h: [f64; 3]) -> Vec<f64> {
        let r2 = self.length_mm * self.length_mm;
        let w = h.map(|ha| r2 / (ha * ha));
        let mut x = c.to_vec();
        for a in 0..3 {
            x = apply_axis(&x, self.grid, a, &self.bases[a].0, false);
        }
        let [nx, ny, nz] = self.grid;
        for i in 0..nx {
            for j in 0..ny {
                for k in 0..nz {
                    let d = 1.0
                        + w[0] * self.bases[0].1[i]
                        + w[1] * self.bases[1].1[j]
                        + w[2] * self.bases[2].1[k];
                    x[(i * ny + j) * nz + k] /= d;
                }
            }
        }
        for a in 0..3 {
            x = apply_axis(&x, self.grid, a, &self.bases[a].0, true);
        }
        x
    }

    fn laplacian(&self, f: &[f64], axis: usize) -> Vec<f64> {
        let n = self.grid[axis];
        let strides = [self.grid[1] * self.grid[2], self.grid[2], 1];
        let st = strides[axis];
        (0..f.len())
            .map(|i| {
                let p = (i / st) % n;
                let mut v = 0.0;
                if p > 0 {
                    v += f[i] - f[i - st];
                }
                if p + 1 < n {
                    v += f[i] - f[i + st];
                }
                v
            })
            .collect()
    }


    pub fn apply(&self, control: &[f64], spacing_mm: &[f64]) -> GResult<Vec<f64>> {
        if control.len() != self.grid.iter().product::<usize>() || !control.iter().all(|v| v.is_finite()) {
            return Err(verr("density filter control must be a finite field on its grid"));
        }
        let h = Self::check_h(spacing_mm)?;
        Ok(self.solve(control, h))
    }


    pub fn vjp(
        &self,
        filtered: &[f64],
        spacing_mm: &[f64],
        cotangent: &[f64],
    ) -> GResult<(Vec<f64>, [f64; 3])> {
        let h = Self::check_h(spacing_mm)?;
        let n = self.grid.iter().product::<usize>();
        if filtered.len() != n || cotangent.len() != n {
            return Err(verr("density filter control must be a finite field on its grid"));
        }
        let lam = self.solve(cotangent, h);
        let r2 = self.length_mm * self.length_mm;
        let dh = std::array::from_fn(|a| {
            let g = self.laplacian(filtered, a);
            let coef = -2.0 * r2 / h[a].powi(3);
            -lam.iter().zip(&g).map(|(l, v)| l * coef * v).sum::<f64>()
        });
        Ok((lam, dh))
    }

    #[must_use]
    pub fn report(&self) -> Value {
        json!({"schema": HELMHOLTZ_FILTER_SCHEMA, "method": "helmholtz_pde_neumann", "radius_mm": self.radius_mm,
            "helmholtz_length_mm": self.length_mm, "grid": self.grid, "range_preserving": true, "mass_preserving": true,
            "exact_adjoint": true})
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComplementaryPhaseMap {
    pub beta: f64,
}

impl ComplementaryPhaseMap {

    pub fn new(beta: f64) -> GResult<Self> {
        if !beta.is_finite() || !(0.0..=32.0).contains(&beta) {
            return Err(verr("phase projection beta must be finite and in [0,32]"));
        }
        Ok(Self { beta })
    }

    fn check(control: &[f64]) -> GResult<()> {
        if control.is_empty() || !control.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
            return Err(verr("phase control must be a finite [0,1] field"));
        }
        Ok(())
    }


    pub fn values(&self, control: &[f64]) -> GResult<(Vec<f64>, Vec<f64>)> {
        Self::check(control)?;
        let s: Vec<f64> = if self.beta == 0.0 {
            control.to_vec()
        } else {
            let t = (0.5 * self.beta).tanh();
            control.iter().map(|c| 0.5 * (1.0 + (self.beta * (c - 0.5)).tanh() / t)).collect()
        };
        let f = s.iter().map(|v| 1.0 - v).collect();
        Ok((s, f))
    }


    pub fn derivative(&self, control: &[f64]) -> GResult<Vec<f64>> {
        Self::check(control)?;
        if self.beta == 0.0 {
            return Ok(vec![1.0; control.len()]);
        }
        let k = 0.5 * self.beta / (0.5 * self.beta).tanh();
        Ok(control.iter().map(|c| k / (self.beta * (c - 0.5)).cosh().powi(2)).collect())
    }


    pub fn measures(&self, control: &[f64], spacing_m: &[f64]) -> GResult<Value> {
        let (s, f) = self.values(control)?;
        if spacing_m.len() != 3
            || !spacing_m.iter().all(|h| h.is_finite())
            || spacing_m.iter().copied().fold(f64::INFINITY, f64::min) <= 0.0
        {
            return Err(verr("positive 3-D cell spacing required"));
        }
        let v = spacing_m[0] * spacing_m[1] * spacing_m[2];
        #[allow(clippy::cast_precision_loss)]
        let n = s.len() as f64;
        let gray: Vec<f64> = s.iter().zip(&f).map(|(a, b)| 4.0 * a * b).collect();
        Ok(json!({"solid_volume_m3": crate::numpy::sum(&s) * v, "fluid_volume_m3": crate::numpy::sum(&f) * v,
            "total_volume_m3": n * v, "solid_volume_fraction": crate::numpy::mean(&s),
            "complementarity_max_error": s.iter().zip(&f).map(|(a, b)| (a + b - 1.0).abs()).fold(0.0, f64::max),
            "gray_measure": crate::numpy::mean(&gray), "projection_beta": self.beta}))
    }
}

