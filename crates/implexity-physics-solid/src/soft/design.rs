// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::CaeError;

use crate::util::contract;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stiffness {
    Constant,
    Simp {
        penalty: f64,
        e_min: f64,
    },
    Ramp {
        q: f64,
        e_min: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MassInterpolation {
    Linear,
    Pedersen,
    Constant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interpolation {
    pub stiffness: Stiffness,
    pub wang: Option<(f64, f64)>,
    pub mass: MassInterpolation,
}

impl Default for Interpolation {
    fn default() -> Self {
        Self {
            stiffness: Stiffness::Simp { penalty: 3.0, e_min: 1e-6 },
            wang: Some((500.0, 0.01)),
            mass: MassInterpolation::Pedersen,
        }
    }
}

impl Interpolation {
    #[must_use]
    pub const fn none() -> Self {
        Self { stiffness: Stiffness::Constant, wang: None, mass: MassInterpolation::Linear }
    }


    pub fn validate(&self) -> Result<(), CaeError> {
        let ok_emin = |e: f64| e.is_finite() && e > 0.0 && e < 1.0;
        match self.stiffness {
            Stiffness::Constant => {}
            Stiffness::Simp { penalty, e_min } if penalty.is_finite() && penalty >= 1.0 && ok_emin(e_min) => {
            }
            Stiffness::Ramp { q, e_min } if q.is_finite() && q >= 0.0 && ok_emin(e_min) => {}
            _ => {
                return contract(
                    "interpolation requires penalty >= 1 (simp) or q >= 0 (ramp) and 0 < e_min < 1",
                );
            }
        }
        if let Some((b, eta)) = self.wang
            && !(b.is_finite() && b > 0.0 && eta.is_finite() && eta > 0.0 && eta < 1.0)
        {
            return contract("wang interpolation requires beta > 0 and 0 < eta < 1");
        }
        Ok(())
    }

    pub fn stiffness<S: Scalar>(&self, rho: S) -> S {
        match self.stiffness {
            Stiffness::Constant => S::one(),
            Stiffness::Simp { penalty, e_min } => rho.powf(penalty) * (1.0 - e_min) + e_min,
            Stiffness::Ramp { q, e_min } => rho / ((-rho + 1.0) * q + 1.0) * (1.0 - e_min) + e_min,
        }
    }

    pub fn gamma<S: Scalar>(&self, rho: S) -> S {
        match self.wang {
            None => S::one(),
            Some((b, eta)) => {
                let t = (b * eta).tanh();
                ((rho - eta) * b).tanh() / (t + (b * (1.0 - eta)).tanh()) + t / (t + (b * (1.0 - eta)).tanh())
            }
        }
    }

    pub fn mass<S: Scalar>(&self, rho: S) -> S {
        match self.mass {
            MassInterpolation::Linear => rho,
            MassInterpolation::Constant => S::one(),
            MassInterpolation::Pedersen => {
                if rho.value() > 0.1 {
                    rho
                } else {
                    rho.powi(6) * 6e5 - rho.powi(7) * 5e6
                }
            }
        }
    }

    #[must_use]
    pub fn wang_active(&self) -> bool {
        self.wang.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct FieldMap {
    rows: Vec<Vec<(usize, f64)>>,
    pub region: Vec<bool>,
    pub fixed: Vec<f64>,
    pub projection: Option<(f64, f64)>,
}

impl FieldMap {

    pub fn new(
        centroids: &[[f64; 3]],
        volumes: &[f64],
        radius: f64,
        region: Vec<bool>,
        fixed: Vec<f64>,
        projection: Option<(f64, f64)>,
    ) -> Result<Self, CaeError> {
        let n = centroids.len();
        if region.len() != n || fixed.len() != n || volumes.len() != n || fixed.iter().any(|v| !v.is_finite())
        {
            return contract("design_region and fixed_design must hold one finite entry per element");
        }
        if !(radius.is_finite() && radius >= 0.0) {
            return contract("filter_radius_m must be finite and nonnegative");
        }
        if let Some((b, eta)) = projection
            && !(b.is_finite() && b > 0.0 && eta.is_finite() && eta > 0.0 && eta < 1.0)
        {
            return contract("projection requires beta > 0 and 0 < eta < 1");
        }
        let rows = if radius == 0.0 {
            (0..n).map(|e| vec![(e, 1.0)]).collect()
        } else {
            hat_rows(centroids, volumes, radius)
        };
        Ok(Self { rows, region, fixed, projection })
    }

    fn source(&self, x: &[f64]) -> Vec<f64> {
        x.iter().zip(&self.region).zip(&self.fixed).map(|((x, r), f)| if *r { *x } else { *f }).collect()
    }

    fn project(&self, v: f64) -> (f64, f64) {
        match self.projection {
            None => (v, 1.0),
            Some((b, eta)) => {
                let t = (b * eta).tanh();
                let den = t + (b * (1.0 - eta)).tanh();
                let th = (b * (v - eta)).tanh();
                ((t + th) / den, b * (1.0 - th * th) / den)
            }
        }
    }

    #[must_use]
    pub fn forward(&self, x: &[f64]) -> Vec<f64> {
        let s = self.source(x);
        self.rows
            .iter()
            .enumerate()
            .map(|(e, row)| {
                if self.region[e] {
                    self.project(row.iter().map(|(j, w)| w * s[*j]).sum()).0
                } else {
                    self.fixed[e]
                }
            })
            .collect()
    }

    #[must_use]
    pub fn pullback(&self, x: &[f64], g: &[f64]) -> Vec<f64> {
        let s = self.source(x);
        let mut out = vec![0.0; x.len()];
        for (e, row) in self.rows.iter().enumerate() {
            if !self.region[e] {
                continue;
            }
            let filtered: f64 = row.iter().map(|(j, w)| w * s[*j]).sum();
            let slope = self.project(filtered).1 * g[e];
            for (j, w) in row {
                if self.region[*j] {
                    out[*j] += w * slope;
                }
            }
        }
        out
    }
}

fn hat_rows(centroids: &[[f64; 3]], volumes: &[f64], radius: f64) -> Vec<Vec<(usize, f64)>> {
    use std::collections::HashMap;
    let key = |p: &[f64; 3]| -> [i64; 3] {
        #[allow(clippy::cast_possible_truncation)]
        p.map(|x| (x / radius).floor() as i64)
    };
    let mut cells: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
    for (i, p) in centroids.iter().enumerate() {
        cells.entry(key(p)).or_default().push(i);
    }
    centroids
        .iter()
        .map(|p| {
            let k = key(p);
            let mut row = Vec::new();
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(list) = cells.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                            for &j in list {
                                let d = (0..3).map(|a| (p[a] - centroids[j][a]).powi(2)).sum::<f64>().sqrt();
                                let w = (radius - d).max(0.0) * volumes[j];
                                if w > 0.0 {
                                    row.push((j, w));
                                }
                            }
                        }
                    }
                }
            }
            row.sort_by_key(|(j, _)| *j);
            let total: f64 = row.iter().map(|(_, w)| w).sum();
            row.iter().map(|(j, w)| (*j, w / total)).collect()
        })
        .collect()
}

