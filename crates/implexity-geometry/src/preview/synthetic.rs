// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use serde_json::{Map, Value, json};

use super::{
    BoxFields, Channel, Continuation, Design, Params, RunMode, TwoRate, channel, grad_world, sigmoid_np,
    soft_floor_np,
};
use crate::boxes::SampleBox;
use crate::error::{GResult, GeometryError};
use crate::lattice::numerics::box_blur3;
use crate::lattice::synth::{N_BASIS, basis_from_trig, family_vector, phase_trig};

fn trilerp_cell(shape: [usize; 3], ci: [f64; 3]) -> ([usize; 3], [f64; 3]) {
    #[allow(clippy::cast_precision_loss)]
    let b: [usize; 3] = std::array::from_fn(|a| super::base_index(ci[a], shape[a] as f64 - 2.0));
    #[allow(clippy::cast_precision_loss)]
    let fr: [f64; 3] = std::array::from_fn(|a| ci[a] - b[a] as f64);
    (b, fr)
}

fn trilerp(f: &[f64], shape: [usize; 3], ci: [f64; 3]) -> f64 {
    let (b, fr) = trilerp_cell(shape, ci);
    trilerp_at(f, shape, b, fr)
}

fn trilerp_at(f: &[f64], shape: [usize; 3], b: [usize; 3], fr: [f64; 3]) -> f64 {
    let (sx, sy) = (shape[1] * shape[2], shape[2]);

    let c = &f[(b[0] * shape[1] + b[1]) * shape[2] + b[2]..][..=sx + sy + 1];
    let [fx, fy, fz] = fr;
    let c00 = c[0] * (1.0 - fx) + c[sx] * fx;
    let c01 = c[1] * (1.0 - fx) + c[sx + 1] * fx;
    let c10 = c[sy] * (1.0 - fx) + c[sx + sy] * fx;
    let c11 = c[sy + 1] * (1.0 - fx) + c[sx + sy + 1] * fx;
    (c00 * (1.0 - fy) + c10 * fy) * (1.0 - fz) + (c01 * (1.0 - fy) + c11 * fy) * fz
}


pub fn synthetic_design(control_shape: [usize; 3], domain_mm: [f64; 3], seed: u64) -> GResult<Design> {
    if control_shape.contains(&0) {
        return Err(GeometryError::Value("control shape must be positive".into()));
    }
    let mut rng = implexity_core::rng::default_rng(u128::from(seed));
    let [ncx, ncy, ncz] = control_shape;
    let n = ncx * ncy * ncz;
    let lin = crate::numpy::linspace(0.0, 1.0, ncx);
    let zi: Vec<f64> = (0..n).map(|c| lin[c / (ncy * ncz)]).collect();
    let a: Vec<f64> =
        rng.standard_normal_vec(n).iter().zip(&zi).map(|(r, z)| -1.5 + 4.0 * (z * z) + 0.25 * r).collect();
    let m: Vec<f64> = rng.standard_normal_vec(n).iter().map(|r| 2.5 + 0.4 * r).collect();
    let nu: Vec<f64> =
        rng.standard_normal_vec(n).iter().zip(&zi).map(|(r, z)| -2.0 + 5.0 * z + 0.4 * r).collect();
    let c: Vec<f64> = rng
        .standard_normal_vec(n)
        .iter()
        .zip(&zi)
        .map(|(r, z)| 2.5 - 5.0 * (z - 0.55).abs() / 0.55 + 0.3 * r)
        .collect();
    let dphi: Vec<f64> = rng.standard_normal_vec(3 * n).iter().map(|r| 0.35 * r).collect();
    let s: Vec<f64> = rng.standard_normal_vec(3 * n).iter().map(|r| 0.05 * r).collect();
    let (vg, vp, vd) = (family_vector("gyroid")?, family_vector("schwarz_p")?, family_vector("diamond")?);
    let noise = rng.standard_normal_vec(N_BASIS * n);
    let w: Vec<f64> = (0..N_BASIS * n)
        .map(|k| {
            let (i, cc) = (k / n, k % n);
            let z = zi[cc];
            (1.0 - z) * vg[i] + z * (0.6 * vp[i] + 0.4 * vd[i]) + 0.05 * noise[k]
        })
        .collect();
    let sh = |k: Option<usize>| -> Vec<usize> {
        k.map_or_else(|| vec![ncx, ncy, ncz], |k| vec![k, ncx, ncy, ncz])
    };
    let mut params = Params::new();
    params.insert("a".into(), Channel { shape: sh(None), data: a });
    params.insert("m".into(), Channel { shape: sh(None), data: m });
    params.insert("dphi".into(), Channel { shape: sh(Some(3)), data: dphi });
    params.insert("s".into(), Channel { shape: sh(Some(3)), data: s });
    params.insert("w".into(), Channel { shape: sh(Some(N_BASIS)), data: w });
    params.insert("nu".into(), Channel { shape: sh(None), data: nu });
    params.insert("c".into(), Channel { shape: sh(None), data: c });
    let mut meta = Map::new();
    meta.insert("source".into(), json!("synthetic"));
    meta.insert("domain_mm".into(), json!(domain_mm));
    meta.insert("control_shape".into(), json!(control_shape));
    meta.insert("design_grid".into(), json!([32, 64, 128]));
    meta.insert("interface_w".into(), json!(0.5));
    meta.insert("beta_mask".into(), json!(128.0));
    meta.insert("beta_mat".into(), json!(64.0));
    meta.insert("beta_topo".into(), json!(64.0));
    meta.insert("t_offset".into(), json!(0.15e-3));
    meta.insert("period_mm".into(), json!(4.0));
    meta.insert("channels".into(), json!(params.keys().collect::<Vec<_>>()));
    Ok(Design::new(params, meta))
}

pub struct SyntheticEvaluator {
    design: Arc<Design>,
    period: f64,
    k0: f64,
    domain: [f64; 3],
    h_design: f64,
}

impl SyntheticEvaluator {

    pub fn new(design: Arc<Design>) -> GResult<Self> {
        let meta = design.meta()?;
        let period = meta.get("period_mm").and_then(Value::as_f64).unwrap_or(4.0) * 1e-3;
        let arr = |k: &str| -> [f64; 3] {
            std::array::from_fn(|i| meta.get(k).and_then(|v| v.get(i)).and_then(Value::as_f64).unwrap_or(1.0))
        };
        let domain = arr("domain_mm").map(|v| v * 1e-3);
        let dg = arr("design_grid");
        Ok(Self {
            k0: 2.0 * std::f64::consts::PI / period,
            period,
            domain,
            h_design: domain[0] / dg[0],
            design,
        })
    }
}

const T_MIN: f64 = 0.2e-3;
const T_MAX: f64 = 1.4e-3;
const INTERFACE_EPS: f64 = 1e-3;
const SOFT_EPS_REL: f64 = 1e-2;
const GRAD_FLOOR_REL: f64 = 0.05;
const W_EPS: f64 = 1e-6;

fn wsum(w: &[f64; N_BASIS], b: &[f64; N_BASIS]) -> f64 {
    let mut s = w[0] * b[0];
    for i in 1..N_BASIS {
        s += w[i] * b[i];
    }
    s
}

impl TwoRate for SyntheticEvaluator {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    fn design(&self) -> &Arc<Design> {
        &self.design
    }
    fn h_design(&self) -> f64 {
        self.h_design
    }
    fn interface_eps(&self) -> f64 {
        INTERFACE_EPS
    }
    fn halo(&self, h: f64) -> (usize, usize) {
        (self.blur_r(h).saturating_add(2), self.blur_r(h))
    }
    fn blur_r(&self, h: f64) -> usize {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let r = (self.period / (4.0 * h)).round_ties_even().max(1.0) as usize;
        r
    }
    fn prep(&self, b: &SampleBox, boundary: &str) -> Vec<[f64; 3]> {
        let mut pts = b.points();
        if boundary == "clamp" {
            let eps = 0.5 * self.h_design;
            for p in &mut pts {
                for c in 0..3 {
                    p[c] = p[c].max(eps).min(self.domain[c] - eps);
                }
            }
        }
        pts
    }
    #[allow(clippy::too_many_lines)]
    fn run(
        &self,
        params: &Params,
        pts: &[[f64; 3]],
        shape: [usize; 3],
        h: f64,
        a: [[f64; 3]; 3],
        r: usize,
        mode: RunMode,
        cont: &Continuation,
        cancel: Option<&dyn Fn() -> bool>,
    ) -> GResult<BoxFields> {
        if cancel.is_some_and(|c| c()) {
            return Err(GeometryError::Cancelled);
        }
        let n = pts.len();
        let k0 = self.k0;

        let ncs = channel(params, "a")?.spatial();
        channel(params, "w")?;
        channel(params, "m")?;
        #[allow(clippy::cast_precision_loss)]

        let ci: Vec<[f64; 3]> = pts
            .par_iter()
            .map(|p| std::array::from_fn(|c| p[c] / self.domain[c] * (ncs[c] as f64 - 1.0)))
            .collect();
        let cells: Vec<([usize; 3], [f64; 3])> = ci.par_iter().map(|c| trilerp_cell(ncs, *c)).collect();
        let interp = |name: &str, comp: usize| -> Vec<f64> {
            let chn = &params[name];
            let fs = chn.spatial();
            let f = chn.component(comp);
            if fs == ncs {
                cells.par_iter().map(|(b, fr)| trilerp_at(f, fs, *b, *fr)).collect()
            } else {
                ci.par_iter().map(|c| trilerp(f, fs, *c)).collect()
            }
        };
        let dp: [Vec<f64>; 3] =
            std::array::from_fn(
                |i| if params.contains_key("dphi") { interp("dphi", i) } else { vec![0.0; n] },
            );
        let phi: Vec<[f64; 3]> =
            (0..n).into_par_iter().map(|c| std::array::from_fn(|i| pts[c][i] * k0 + dp[i][c])).collect();
        let w: Vec<Vec<f64>> = (0..N_BASIS).map(|i| interp("w", i)).collect();
        let w_hat: Vec<[f64; N_BASIS]> = (0..n)
            .into_par_iter()
            .map(|c| {
                let v: [f64; N_BASIS] = std::array::from_fn(|i| w[i][c]);
                let mut ss = v[0] * v[0];
                for x in &v[1..] {
                    ss += x * x;
                }
                let norm = (ss + W_EPS * W_EPS).sqrt();
                v.map(|x| x / norm)
            })
            .collect();

        let trig: Vec<[[f64; 4]; 3]> = phi.par_iter().map(|p| p.map(phase_trig)).collect();
        let bv: Vec<[f64; N_BASIS]> = trig.par_iter().map(basis_from_trig).collect();
        let f: Vec<f64> = (0..n).into_par_iter().map(|c| wsum(&w_hat[c], &bv[c])).collect();
        let [t_offset, beta_mask, beta_mat, beta_topo, w_iface] = *cont;
        let t_eff: Vec<f64> = interp("a", 0)
            .iter()
            .map(|x| soft_floor_np(T_MIN + (T_MAX - T_MIN) * sigmoid_np(*x) + t_offset, 0.05 * T_MIN))
            .collect();
        let t_mid = 0.5 * (T_MIN + T_MAX);
        let nu: Vec<f64> = if params.contains_key("nu") {
            interp("nu", 0).iter().map(|x| sigmoid_np(beta_topo * x)).collect()
        } else {
            vec![0.0; n]
        };
        let mtilde: Vec<f64> = interp("m", 0).iter().map(|x| sigmoid_np(beta_mask * x)).collect();
        let phase_fraction: Vec<f64> = if params.contains_key("c") {
            interp("c", 0).iter().map(|x| sigmoid_np(beta_mat * x)).collect()
        } else {
            Vec::new()
        };
        let tau: Vec<f64> = (0..n).map(|i| t_eff[i] - nu[i] * t_mid).collect();
        let mut out = BoxFields::new();
        let phi_flat: Vec<f64> = (0..3).flat_map(|i| phi.iter().map(move |p| p[i])).collect();
        if mode == RunMode::Fast {
            out.insert("f".into(), f);
            out.insert("nu".into(), nu);
            out.insert("tau".into(), tau);
            out.insert("mtilde".into(), mtilde);
            if !phase_fraction.is_empty() { out.insert("phase_fraction".into(), phase_fraction); }
            out.insert("t".into(), t_eff);
            out.insert("phi".into(), phi_flat);
            return Ok(out);
        }
        let mut jac: [[Vec<f64>; 3]; 3] = Default::default();
        for i in 0..3 {
            let mut row = grad_world(&dp[i], shape, h, a);
            for v in &mut row[i] {
                *v += k0;
            }
            jac[i] = row;
        }
        let eps = 1e-5;
        let dfdphi: [Vec<f64>; 3] = std::array::from_fn(|k| {
            (0..n)
                .into_par_iter()
                .map(|c| {
                    let mut tp = trig[c];
                    tp[k] = phase_trig(phi[c][k] + eps);
                    let fp = wsum(&w_hat[c], &basis_from_trig(&tp));
                    let mut tm = trig[c];
                    tm[k] = phase_trig(phi[c][k] - eps);
                    let fm = wsum(&w_hat[c], &basis_from_trig(&tm));
                    (fp - fm) / (2.0 * eps)
                })
                .collect()
        });
        let mut extra: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
        for i in 0..N_BASIS {
            let wi: Vec<f64> = w_hat.iter().map(|v| v[i]).collect();
            let g = grad_world(&wi, shape, h, a);
            for k in 0..3 {
                for c in 0..n {
                    extra[k][c] += bv[c][i] * g[k][c];
                }
            }
        }
        let eps_g = SOFT_EPS_REL * k0;
        let gnorm: Vec<f64> = (0..n)
            .into_par_iter()
            .map(|c| {
                let fg: [f64; 3] = std::array::from_fn(|k| {
                    0.0 + dfdphi[0][c] * jac[0][k][c]
                        + dfdphi[1][c] * jac[1][k][c]
                        + dfdphi[2][c] * jac[2][k][c]
                        + extra[k][c]
                });
                (fg[0] * fg[0] + fg[1] * fg[1] + fg[2] * fg[2] + eps_g * eps_g).sqrt()
            })
            .collect();
        let geff: Vec<f64> =
            box_blur3(&gnorm, shape, r).into_iter().map(|x| soft_floor_np(x, GRAD_FLOOR_REL * k0)).collect();
        if mode == RunMode::Geff {
            out.insert("geff".into(), geff);
            out.insert("gnorm".into(), gnorm);
            return Ok(out);
        }
        let folded: Vec<f64> = f.iter().map(|x| (x * x + INTERFACE_EPS * INTERFACE_EPS).sqrt()).collect();
        let q: Vec<f64> = (0..n).map(|c| ((1.0 - nu[c]) * folded[c] + nu[c] * f[c]) / geff[c]).collect();
        let g: Vec<f64> = (0..n).map(|c| q[c] - tau[c]).collect();
        let rho_lat: Vec<f64> = g.iter().map(|x| sigmoid_np(-x / w_iface)).collect();
        out.insert("rho".into(), mtilde.iter().zip(&rho_lat).map(|(m, r)| m * r).collect());
        out.insert("d".into(), (0..n).map(|c| folded[c] / geff[c]).collect());
        out.insert("f".into(), f);
        out.insert("q".into(), q);
        out.insert("tau".into(), tau);
        out.insert("g".into(), g);
        out.insert("nu".into(), nu);
        out.insert("geff".into(), geff);
        out.insert("gnorm".into(), gnorm);
        out.insert("mtilde".into(), mtilde);
        out.insert("rho_lat".into(), rho_lat);
        if !phase_fraction.is_empty() { out.insert("phase_fraction".into(), phase_fraction); }
        out.insert("t".into(), t_eff);
        Ok(out)
    }
}

pub struct SyntheticGeometry;

impl implexity_core::backends::GeometryBackend for SyntheticGeometry {
    fn name(&self) -> &'static str {
        "synthetic"
    }
    fn implementation(&self) -> &'static str {
        "implexity.geom_synth.SyntheticGeometry"
    }
    fn available(&self, _design_path: Option<&Path>) -> (bool, String) {
        (true, "numpy only; no optimiser tree needed".into())
    }
    fn build(
        &self,
        o: &implexity_core::backends::GeometryBuildOptions,
    ) -> Result<implexity_core::backends::GeometryBuild, String> {
        let design = Arc::new(synthetic_design([9, 9, 17], o.domain_mm, 7).map_err(|e| e.to_string())?);
        let ev = SyntheticEvaluator::new(Arc::clone(&design)).map_err(|e| e.to_string())?;
        Ok(implexity_core::backends::GeometryBuild { evaluator: Box::new(ev), design: Box::new(design) })
    }
}

pub fn register_backends() {
    let _ = implexity_core::backends::register_geometry(Arc::new(super::RealGeometry));
    let _ = implexity_core::backends::register_geometry(Arc::new(SyntheticGeometry));
}

