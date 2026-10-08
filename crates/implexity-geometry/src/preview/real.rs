// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::Path;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use serde_json::{Map, Value};

use super::{
    BoxFields, Channel, Continuation, Design, Params, RunMode, TwoRate, channel, grad_world, sigmoid_np,
    soft_floor_np, trilerp_np,
};
use crate::boxes::SampleBox;
use crate::error::{GResult, GeometryError};
use crate::lattice::config::{Channels, LatticeConfig};
use crate::lattice::numerics::{box_blur3, build_grids, cumintegrate};
use crate::lattice::synth::{GYROID, N_BASIS, basis_and_grad, normalise_coefficients};

fn trilerp_clip(f: &[f64], shape: [usize; 3], ci: [f64; 3]) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let c: [f64; 3] = std::array::from_fn(|a| ci[a].clamp(0.0, shape[a] as f64 - 1.0));
    trilerp_np(f, shape, c)
}

struct Statics {
    lat: LatticeConfig,
    channels: Channels,
    domain: [f64; 3],
    h_design: f64,
    k0: f64,
    design_grid: [usize; 3],
    signature: String,
}

pub struct RealEvaluator {
    design: Arc<Design>,
    channels_override: Option<Channels>,
    statics: Mutex<Option<Arc<Statics>>>,
    potential: Mutex<Option<(u64, Option<Arc<[Vec<f64>; 3]>>)>>,
}

fn signature(meta: &Map<String, Value>, params: &Params) -> String {
    let keys: Vec<&String> = params.keys().collect();
    format!(
        "{}|{}|{}|{:?}|{:?}",
        meta.get("domain_mm").map(Value::to_string).unwrap_or_default(),
        meta.get("design_grid").map(Value::to_string).unwrap_or_default(),
        meta.get("period_mm").and_then(Value::as_f64).unwrap_or(4.0),
        keys,
        params.get("a").map(Channel::spatial)
    )
}

fn triple_f(meta: &Map<String, Value>, k: &str, d: [f64; 3]) -> [f64; 3] {
    meta.get(k)
        .and_then(Value::as_array)
        .map_or(d, |a| std::array::from_fn(|i| a.get(i).and_then(Value::as_f64).unwrap_or(d[i])))
}

impl RealEvaluator {

    pub fn new(design: Arc<Design>, channels_override: Option<Channels>) -> GResult<Self> {
        let ev = Self { design, channels_override, statics: Mutex::new(None), potential: Mutex::new(None) };
        ev.rebind()?;
        Ok(ev)
    }


    pub fn rebind(&self) -> GResult<bool> {
        let (params, meta, _) = self.design.snapshot_full()?;
        let sig = signature(&meta, &params);
        let mut st = self.statics.lock().map_err(|_| GeometryError::Value("lock poisoned".into()))?;
        if st.as_ref().is_some_and(|s| s.signature == sig) {
            return Ok(false);
        }
        let period = meta.get("period_mm").and_then(Value::as_f64).unwrap_or(4.0) * 1e-3;
        let base = LatticeConfig::default();
        let lat = if (period - base.period).abs() > 1e-12 {
            LatticeConfig { period, t_min: 0.05 * period, t_max: 0.35 * period, ..base }
        } else {
            base
        };
        let live = |k: &str| params.contains_key(k);
        let channels = self.channels_override.unwrap_or(Channels {
            stretch: live("s"),
            family: live("w"),
            mode: live("nu"),
            secondary: live("w2"),
            residual: live("res"),
            material: live("c"),
            phase: live("dphi"),
        });
        let dg = triple_f(&meta, "design_grid", [32.0, 64.0, 128.0]);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let design_grid = dg.map(|v| v as usize);
        let dom = triple_f(&meta, "domain_mm", [8.0, 16.0, 32.0]).map(|v| v * 1e-3);
        let h_design = dom[0] / dg[0];
        let k0 = 2.0 * std::f64::consts::PI / lat.period;
        *st =
            Some(Arc::new(Statics { lat, channels, domain: dom, h_design, k0, design_grid, signature: sig }));
        if let Ok(mut p) = self.potential.lock() {
            *p = None;
        }
        Ok(true)
    }

    fn statics(&self) -> GResult<Arc<Statics>> {
        self.statics
            .lock()
            .map_err(|_| GeometryError::Value("lock poisoned".into()))?
            .clone()
            .ok_or_else(|| GeometryError::Value("evaluator not bound".into()))
    }

    fn potential(&self, st: &Statics, params: &Params, version: u64) -> Option<Arc<[Vec<f64>; 3]>> {
        if let Ok(p) = self.potential.lock()
            && let Some((v, pot)) = p.as_ref()
            && *v == version
        {
            return pot.clone();
        }
        let out = if st.channels.stretch && params.contains_key("s") {
            let s = &params["s"];
            let grids = build_grids(st.design_grid, [st.h_design; 3], &st.lat);
            let fs = s.spatial();
            let cells = grids.cells();
            let integ: [Vec<f64>; 3] = std::array::from_fn(|i| {
                let comp = s.component(i);
                let sm1: Vec<f64> = (0..cells)
                    .map(|c| {
                        let sf = trilerp_clip(comp, fs, [grids.ci[0][c], grids.ci[1][c], grids.ci[2][c]]);
                        (st.lat.s_max * sf.tanh()).exp() - 1.0
                    })
                    .collect();
                cumintegrate(&sm1, st.design_grid, st.h_design, i).into_iter().map(|v| st.k0 * v).collect()
            });
            Some(Arc::new(integ))
        } else {
            None
        };
        if let Ok(mut p) = self.potential.lock() {
            *p = Some((version, out.clone()));
        }
        out
    }
}

impl TwoRate for RealEvaluator {
    fn name(&self) -> &'static str {
        "geom_full"
    }
    fn rebind(&self) -> GResult<bool> {
        Self::rebind(self)
    }
    fn design(&self) -> &Arc<Design> {
        &self.design
    }
    fn h_design(&self) -> f64 {
        self.statics().map_or(0.0, |s| s.h_design)
    }
    fn interface_eps(&self) -> f64 {
        self.statics().map_or(1e-3, |s| s.lat.interface_eps)
    }
    fn halo(&self, h: f64) -> (usize, usize) {
        let r = self.blur_r(h);
        (r.saturating_add(2), r)
    }
    fn blur_r(&self, h: f64) -> usize {
        let period = self.statics().map_or(4e-3, |s| s.lat.period);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let r = (period / (4.0 * h)).round_ties_even().max(1.0) as usize;
        r
    }
    fn prep(&self, b: &SampleBox, boundary: &str) -> Vec<[f64; 3]> {
        let mut pts = b.points();
        if boundary == "clamp"
            && let Ok(st) = self.statics()
        {
            let eps = 0.5 * st.h_design;
            for p in &mut pts {
                for c in 0..3 {
                    p[c] = p[c].max(eps).min(st.domain[c] - eps);
                }
            }
        }
        pts
    }
    fn on_snapshot(&self, _meta: &Map<String, Value>, _params: &Params) {
        let _ = self.rebind();
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
        let st = self.statics()?;
        let (_, version) = self.design.snapshot()?;
        let n = pts.len();
        let lat = &st.lat;
        let ch = st.channels;
        let k0 = st.k0;
        let integ: [Vec<f64>; 3] = match self.potential(&st, params, version) {

            Some(pot) => std::array::from_fn(|c| {
                pts.par_iter()
                    .map(|p| {
                        trilerp_np(
                            &pot[c],
                            st.design_grid,
                            [p[0] / st.h_design - 0.5, p[1] / st.h_design - 0.5, p[2] / st.h_design - 0.5],
                        )
                    })
                    .collect()
            }),
            None => std::array::from_fn(|_| vec![0.0; n]),
        };

        let ncs = channel(params, "a")?.spatial();
        channel(params, "m")?;
        #[allow(clippy::cast_precision_loss)]
        let ci: Vec<[f64; 3]> = pts
            .par_iter()
            .map(|p| std::array::from_fn(|c| p[c] / st.domain[c] * (ncs[c] as f64 - 1.0)))
            .collect();
        if cancel.is_some_and(|c| c()) {
            return Err(GeometryError::Cancelled);
        }
        let interp = |name: &str, comp: usize| -> Vec<f64> {
            let chn = &params[name];
            let fs = chn.spatial();
            let f = chn.component(comp);
            ci.par_iter().map(|c| trilerp_clip(f, fs, *c)).collect()
        };
        let dp: [Vec<f64>; 3] = std::array::from_fn(|i| {
            if ch.phase && params.contains_key("dphi") { interp("dphi", i) } else { vec![0.0; n] }
        });
        let phi: [Vec<f64>; 3] =
            std::array::from_fn(|i| (0..n).map(|c| pts[c][i] * k0 + integ[i][c] + dp[i][c]).collect());
        let a_i = interp("a", 0);
        let t_mid = 0.5 * (lat.t_min + lat.t_max);
        let t_floor = 0.05 * lat.t_min;
        let t_eff: Vec<f64> = a_i
            .iter()
            .map(|x| soft_floor_np(lat.t_min + (lat.t_max - lat.t_min) * sigmoid_np(*x) + cont[0], t_floor))
            .collect();
        let nu: Vec<f64> = if ch.mode && params.contains_key("nu") {
            interp("nu", 0).iter().map(|x| sigmoid_np(cont[3] * x)).collect()
        } else {
            vec![0.0; n]
        };
        let mtilde: Vec<f64> = interp("m", 0).iter().map(|x| sigmoid_np(cont[1] * x)).collect();
        let phase_fraction: Vec<f64> = if ch.material && params.contains_key("c") {
            interp("c", 0).iter().map(|x| sigmoid_np(cont[2] * x)).collect()
        } else {
            Vec::new()
        };
        let tau: Vec<f64> = (0..n).map(|i| t_eff[i] - nu[i] * t_mid).collect();
        let want_grad = mode != RunMode::Fast;
        let bg: Vec<([f64; N_BASIS], [[f64; N_BASIS]; 3])> =
            (0..n).into_par_iter().map(|i| basis_and_grad([phi[0][i], phi[1][i], phi[2][i]])).collect();
        let mut extra: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
        let mut f = vec![0.0; n];
        let mut dfdphi: [Vec<f64>; 3] = std::array::from_fn(|_| vec![0.0; n]);
        let w_hat: Vec<[f64; N_BASIS]>;
        if ch.family && params.contains_key("w") {
            let w: Vec<Vec<f64>> = (0..N_BASIS).map(|i| interp("w", i)).collect();
            w_hat = (0..n)
                .into_par_iter()
                .map(|c| normalise_coefficients(std::array::from_fn(|i| w[i][c]), lat.w_eps))
                .collect();
            let rows: Vec<(f64, [f64; 3])> = (0..n)
                .into_par_iter()
                .map(|c| {
                    let (bv, bgr) = &bg[c];
                    let mut s = w_hat[c][0] * bv[0];
                    for i in 1..N_BASIS {
                        s += w_hat[c][i] * bv[i];
                    }
                    let mut d = [0.0; 3];
                    if want_grad {
                        for (k, dk) in d.iter_mut().enumerate() {
                            let mut t = w_hat[c][0] * bgr[k][0];
                            for i in 1..N_BASIS {
                                t += w_hat[c][i] * bgr[k][i];
                            }
                            *dk = t;
                        }
                    }
                    (s, d)
                })
                .collect();
            for (c, (s, d)) in rows.into_iter().enumerate() {
                f[c] = s;
                if want_grad {
                    for k in 0..3 {
                        dfdphi[k][c] = d[k];
                    }
                }
            }
            if want_grad {
                for i in 0..N_BASIS {
                    let wi: Vec<f64> = w_hat.iter().map(|v| v[i]).collect();
                    let g = grad_world(&wi, shape, h, a);
                    for k in 0..3 {
                        for c in 0..n {
                            extra[k][c] += bg[c].0[i] * g[k][c];
                        }
                    }
                }
            }
        } else {
            let mut one = [0.0; N_BASIS];
            one[GYROID] = 1.0;
            w_hat = vec![one; n];
            let rows: Vec<[f64; 4]> = (0..n)
                .into_par_iter()
                .map(|c| {
                    let (p1, p2, p3) = (phi[0][c], phi[1][c], phi[2][c]);
                    let (c1, c2, c3) = (p1.cos(), p2.cos(), p3.cos());
                    let (s1, s2, s3) = (p1.sin(), p2.sin(), p3.sin());
                    [s1 * c2 + s2 * c3 + s3 * c1, c1 * c2 - s3 * s1, -s1 * s2 + c2 * c3, -s2 * s3 + c3 * c1]
                })
                .collect();
            for (c, r) in rows.into_iter().enumerate() {
                f[c] = r[0];
                dfdphi[0][c] = r[1];
                dfdphi[1][c] = r[2];
                dfdphi[2][c] = r[3];
            }
        }
        if ch.secondary && params.contains_key("w2") {
            let amp: Vec<f64> = interp("w2", 0).iter().map(|x| lat.sec_scale * x.tanh()).collect();
            let rr = lat.sec_ratio;
            let rows: Vec<(f64, [f64; 3])> = (0..n)
                .into_par_iter()
                .map(|c| {
                    let (sv, sg) = basis_and_grad([rr * phi[0][c], rr * phi[1][c], rr * phi[2][c]]);
                    let mut s = w_hat[c][0] * sv[0];
                    for i in 1..N_BASIS {
                        s += w_hat[c][i] * sv[i];
                    }
                    let mut d = [0.0; 3];
                    if want_grad {
                        for (k, dk) in d.iter_mut().enumerate() {
                            let mut t = w_hat[c][0] * sg[k][0];
                            for i in 1..N_BASIS {
                                t += w_hat[c][i] * sg[k][i];
                            }
                            *dk = t;
                        }
                    }
                    (s, d)
                })
                .collect();
            let mut f2 = vec![0.0; n];
            for (c, (s, t)) in rows.into_iter().enumerate() {
                f2[c] = s;
                f[c] += amp[c] * s;
                if want_grad {
                    for k in 0..3 {
                        dfdphi[k][c] += amp[c] * rr * t[k];
                    }
                }
            }
            if want_grad {
                let ga = grad_world(&amp, shape, h, a);
                for k in 0..3 {
                    for c in 0..n {
                        extra[k][c] += f2[c] * ga[k][c];
                    }
                }
            }
        }
        if ch.residual && params.contains_key("res") {
            let rterm: Vec<f64> = interp("res", 0).iter().map(|x| lat.res_scale * x.tanh()).collect();
            for c in 0..n {
                f[c] += rterm[c];
            }
            if want_grad {
                let gr = grad_world(&rterm, shape, h, a);
                for k in 0..3 {
                    for c in 0..n {
                        extra[k][c] += gr[k][c];
                    }
                }
            }
        }
        let mut out = BoxFields::new();
        let phi_flat: Vec<f64> = phi.iter().flatten().copied().collect();
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
            let s: Vec<f64> = (0..n).map(|c| integ[i][c] + dp[i][c]).collect();
            let mut row = grad_world(&s, shape, h, a);
            for v in &mut row[i] {
                *v += k0;
            }
            jac[i] = row;
        }
        let eps_g = lat.soft_eps_rel * k0;
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
        let geff: Vec<f64> = box_blur3(&gnorm, shape, r)
            .into_iter()
            .map(|x| soft_floor_np(x, lat.grad_floor_rel * k0))
            .collect();
        if mode == RunMode::Geff {
            out.insert("geff".into(), geff);
            out.insert("gnorm".into(), gnorm);
            return Ok(out);
        }
        let eps = lat.interface_eps;
        let folded: Vec<f64> = f.iter().map(|x| (x * x + eps * eps).sqrt()).collect();
        let d: Vec<f64> = (0..n).map(|c| folded[c] / geff[c]).collect();
        let q: Vec<f64> = (0..n).map(|c| ((1.0 - nu[c]) * folded[c] + nu[c] * f[c]) / geff[c]).collect();
        let g: Vec<f64> = (0..n).map(|c| q[c] - tau[c]).collect();
        let rho_lat: Vec<f64> = g.iter().map(|x| sigmoid_np(-x / cont[4])).collect();
        out.insert("rho".into(), mtilde.iter().zip(&rho_lat).map(|(m, r)| m * r).collect());
        out.insert("f".into(), f);
        out.insert("q".into(), q);
        out.insert("tau".into(), tau);
        out.insert("g".into(), g);
        out.insert("nu".into(), nu);
        out.insert("d".into(), d);
        out.insert("geff".into(), geff);
        out.insert("gnorm".into(), gnorm);
        out.insert("mtilde".into(), mtilde);
        out.insert("rho_lat".into(), rho_lat);
        if !phase_fraction.is_empty() { out.insert("phase_fraction".into(), phase_fraction); }
        out.insert("t".into(), t_eff);
        out.insert("phi".into(), phi_flat);
        Ok(out)
    }
}

pub struct RealGeometry;

impl implexity_core::backends::GeometryBackend for RealGeometry {
    fn name(&self) -> &'static str {
        "real"
    }
    fn priority(&self) -> i64 {
        10
    }
    fn implementation(&self) -> &'static str {
        "implexity.geom_real.RealGeometry"
    }
    fn available(&self, design_path: Option<&Path>) -> (bool, String) {
        let path = design_path.map(Path::to_path_buf).or_else(|| {
            implexity_io::locate::Locator::new()
                .ok()
                .and_then(|l| l.resolve_design(None, None, implexity_io::locate::Environ::Process))
        });
        match path {
            Some(p) if p.exists() => (true, "optimiser tree importable and a design is present".into()),
            _ => (false, "no design .npz found".into()),
        }
    }
    fn build(
        &self,
        o: &implexity_core::backends::GeometryBuildOptions,
    ) -> Result<implexity_core::backends::GeometryBuild, String> {
        let path = o
            .design_path
            .clone()
            .or_else(|| {
                implexity_io::locate::Locator::new()
                    .ok()
                    .and_then(|l| l.resolve_design(None, None, implexity_io::locate::Environ::Process))
            })
            .ok_or_else(|| "no design .npz found".to_string())?;
        let design = Arc::new(
            Design::from_npz(&path, o.domain_mm, Some(o.design_grid), o.period_mm)
                .map_err(|e| e.to_string())?,
        );
        let ev = RealEvaluator::new(Arc::clone(&design), None).map_err(|e| e.to_string())?;
        Ok(implexity_core::backends::GeometryBuild { evaluator: Box::new(ev), design: Box::new(design) })
    }
}
