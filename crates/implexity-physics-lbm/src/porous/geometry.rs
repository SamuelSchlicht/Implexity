// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_ad::Scalar;
use implexity_core::{CaeError, CaeResult};

use super::lattice::{Grid, quadrature_gradient_maps};
use super::sp::{self, Sp, SpMap};

pub const SOURCE_PROFILE: &str = "common_positive_quadrature_native_phase_volumes_v1";

fn err(msg: &str) -> CaeError {
    CaeError::contract(msg)
}

#[derive(Clone, Debug)]
pub struct CommonQuadraturePhases {
    q: Sp,
    qmap: SpMap,
    pub fixed_zero_raw: Vec<bool>,
    pub fixed_zero_effective: Vec<bool>,
    pub ghost: Vec<f64>,
}

impl CommonQuadraturePhases {

    pub fn new(q: Sp, fixed_zero_raw: Vec<bool>, ghost: Vec<f64>) -> CaeResult<Self> {
        let rows = sp::mv(&q, &vec![1.0; q.ncols()])?;
        if !q.data().iter().all(|v| v.is_finite() && *v >= 0.0)
            || rows.iter().any(|v| (v - 1.0).abs() > 1e-14)
        {
            return Err(err("positive constant-preserving shared quadrature required"));
        }
        if fixed_zero_raw.len() != q.ncols() {
            return Err(err("authored fixed-zero raw support mask required"));
        }
        if ghost.len() != q.nrows() || ghost.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
            return Err(err("fixed ghost identity in[0,1] required"));
        }
        let free: Vec<f64> = fixed_zero_raw.iter().map(|z| if *z { 0.0 } else { 1.0 }).collect();
        let fixed_zero_effective = sp::mv(&q, &free)?.iter().map(|v| *v == 0.0).collect();
        Ok(Self { qmap: SpMap::new(&q), q, fixed_zero_raw, fixed_zero_effective, ghost })
    }


    pub fn admit(&self, rho: &[f64], c: &[f64]) -> CaeResult<()> {
        let n = self.q.ncols();
        for x in [rho, c] {
            if x.len() != n || x.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
                return Err(err("finite raw occupancy/mixture in[0,1] required"));
            }
        }
        if rho.iter().zip(&self.fixed_zero_raw).any(|(v, z)| *z && *v != 0.0) {
            return Err(err("authored fixed-zero raw support changed"));
        }
        let effective = sp::mv(&self.q, rho)?;
        if effective.iter().zip(&self.fixed_zero_effective).any(|(v, z)| !*z && *v <= 0.0) {
            return Err(err("free effective support may not cross zero"));
        }
        Ok(())
    }

    #[must_use]
    pub fn values<S: Scalar>(&self, rho: &[S], c: &[S]) -> PhaseValues<S> {
        let total = self.qmap.apply(rho);
        let rc: Vec<S> = rho.iter().zip(c).map(|(r, m)| *r * *m).collect();
        let r1c: Vec<S> = rho.iter().zip(c).map(|(r, m)| *r * (S::one() - *m)).collect();
        let b = self.qmap.apply(&rc);
        let a = self.qmap.apply(&r1c);
        let composition = (0..total.len())
            .map(|i| if self.fixed_zero_effective[i] { S::from_f64(self.ghost[i]) } else { b[i] / total[i] })
            .collect();
        PhaseValues { solid_a: a, solid_b: b, solid: total, composition }
    }


    pub fn sparse_partials(&self, rho: &[f64], c: &[f64], r: &Sp, cm: &Sp) -> CaeResult<(Sp, Sp)> {
        self.admit(rho, c)?;
        if r.shape() != cm.shape() || r.nrows() != self.q.ncols() {
            return Err(err("finite matching raw design chains required"));
        }
        for (i, z) in self.fixed_zero_raw.iter().enumerate() {
            if *z && (!r.row(i).0.is_empty() || !cm.row(i).0.is_empty()) {
                return Err(err("fixed clear occupancy AND ghost mixture must exclude free coordinates"));
            }
        }
        let total = sp::mv(&self.q, rho)?;
        let rc: Vec<f64> = rho.iter().zip(c).map(|(a, b)| a * b).collect();
        let b = sp::mv(&self.q, &rc)?;
        let dtotal = sp::mm(&self.q, r)?;
        let inner = sp::add(&sp::mm(&sp::diag(c), r)?, &sp::mm(&sp::diag(rho), cm)?)?;
        let db = sp::mm(&self.q, &inner)?;
        let inv: Vec<f64> = total
            .iter()
            .zip(&self.fixed_zero_effective)
            .map(|(t, z)| if *z { 0.0 } else { 1.0 / t })
            .collect();
        let binv2: Vec<f64> = b.iter().zip(&inv).map(|(b, i)| b * i * i).collect();
        let dc = sp::sub(&sp::mm(&sp::diag(&inv), &db)?, &sp::mm(&sp::diag(&binv2), &dtotal)?)?;
        Ok((dtotal, dc))
    }
}

#[derive(Clone, Debug)]
pub struct PhaseValues<S> {
    pub solid_a: Vec<S>,
    pub solid_b: Vec<S>,
    pub solid: Vec<S>,
    pub composition: Vec<S>,
}

#[derive(Clone, Debug)]
pub struct GeometryBinding {
    pub grid: Grid,
    pub mask: Vec<bool>,
    pub ghost: Vec<f64>,
    pub spacing_m: f64,
    pub h: Sp,
    hmap: SpMap,
    pub q: Sp,
    pub g: Sp,
    pub phases: CommonQuadraturePhases,
}

#[derive(Clone, Debug)]
pub struct GeometryValues<S> {
    pub native_design: Vec<S>,
    pub raw_phi: Vec<S>,
    pub phi_halo: Vec<S>,
    pub phases: PhaseValues<S>,
}

impl GeometryBinding {

    pub fn new(grid: Grid, mask: Vec<bool>, ghost: Vec<f64>, spacing_m: f64) -> CaeResult<Self> {
        let hg = grid.halo();
        let (mut rows, mut cols) = (Vec::new(), Vec::new());
        for idx in 0..hg.cells() {
            let [a, b, c] = hg.ijk(idx);
            if a >= 1 && a <= grid.n[0] {
                let y = (b + grid.n[1] - 1) % grid.n[1];
                let z = (c + grid.n[2] - 1) % grid.n[2];
                rows.push(idx);
                cols.push(grid.id(a - 1, y, z));
            }
        }
        let h = sp::triplets(hg.cells(), grid.cells(), &rows, &cols, &vec![1.0; rows.len()])?;
        let (q, g) = quadrature_gradient_maps(grid)?;
        let free: Vec<f64> = mask.iter().map(|m| if *m { 0.0 } else { 1.0 }).collect();
        let zero: Vec<bool> = sp::mv(&h, &free)?.iter().map(|v| *v == 0.0).collect();
        let phases = CommonQuadraturePhases::new(q.clone(), zero, ghost.clone())?;
        Ok(Self { grid, mask, ghost, spacing_m, hmap: SpMap::new(&h), h, q, g, phases })
    }


    pub fn admit(&self, rho: &[f64], c: &[f64]) -> CaeResult<()> {
        let nc = self.grid.cells();
        if rho.len() != nc || c.len() != nc || rho.iter().chain(c).any(|v| !v.is_finite()) {
            return Err(err("raw occupancy requires finite real exact-shape values"));
        }
        if rho.iter().any(|v| *v < 0.0 || *v >= 1.0) || c.iter().any(|v| *v < 0.0 || *v > 1.0) {
            return Err(err("positive fluid porosity and bounded mixture required"));
        }
        for i in 0..nc {
            if self.mask[i] && (rho[i] != 0.0 || c[i] != self.ghost[i]) {
                return Err(err("fixed clear coordinates changed"));
            }
        }
        self.phases.admit(&sp::mv(&self.h, rho)?, &sp::mv(&self.h, c)?)
    }

    #[must_use]
    pub fn values<S: Scalar>(&self, rho: &[S], c: &[S]) -> GeometryValues<S> {
        let rh = self.hmap.apply(rho);
        let ch = self.hmap.apply(c);
        let v = self.phases.values(&rh, &ch);
        let mut native = v.solid.clone();
        native.extend([S::from_f64(self.spacing_m * 1000.0); 3]);
        native.extend(v.composition.iter().copied());
        GeometryValues {
            native_design: native,
            raw_phi: rho.iter().map(|r| S::one() - *r).collect(),
            phi_halo: rh.iter().map(|r| S::one() - *r).collect(),
            phases: v,
        }
    }


    pub fn partials(&self, rho: &[f64], c: &[f64], rx: &Sp, cx: &Sp) -> CaeResult<(Sp, Sp, Sp)> {
        self.admit(rho, c)?;
        let nc = self.grid.cells();
        if rx.shape() != cx.shape() || rx.nrows() != nc {
            return Err(err("finite matching upstream chains required"));
        }
        for i in 0..nc {
            if self.mask[i] && (!rx.row(i).0.is_empty() || !cx.row(i).0.is_empty()) {
                return Err(err("fixed clear design coordinates must be excluded"));
            }
        }
        let hr = sp::mm(&self.h, rx)?;
        let hc = sp::mm(&self.h, cx)?;
        let (solid, composition) =
            self.phases.sparse_partials(&sp::mv(&self.h, rho)?, &sp::mv(&self.h, c)?, &hr, &hc)?;
        let native = sp::vstack(&[&solid, &sp::zeros(3, rx.ncols()), &composition])?;
        let qphi = sp::neg(&sp::mm(&self.q, &hr)?);
        let gradphi = sp::neg(&sp::mm(&self.g, &hr)?);
        Ok((native, qphi, gradphi))
    }
}
