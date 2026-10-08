// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::path::Path;

use implexity_io::npy::{NpyArray, NpyData};
use implexity_optim::design::NamedArrays;
use implexity_optim::mma::{GcmmaOptions, MmaOptimizer, MmaOptions, MmaStateDict};
use implexity_optim::numeric::float_value;
use ndarray::{ArrayD, IxDyn};
use serde_json::{Map, Value, json};

use super::problem::{Design, Problem};
use super::spec::{Constraint, Drive, Free, mma_options, py_float, py_int};
use crate::error::{JobError, JobResult};

pub const MMA_STATE_FILE: &str = "mma_state.npz";

#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    pub fs: Vec<f64>,
    pub dfs: Vec<f64>,
    pub feasible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    x_km1: Option<Vec<f64>>,
    x_km2: Option<Vec<f64>>,
    opt: Option<MmaStateDict>,
}

#[derive(Clone, Debug)]
pub struct MmaDriver {
    free: Vec<Free>,
    design: Design,
    pub opts: Map<String, Value>,
    x_min: Vec<f64>,
    x_max: Vec<f64>,
    n: usize,
    pub constraints: Vec<Constraint>,
    m: usize,
    pub occupancy_idx: Vec<usize>,
    pub physics_idx: Vec<usize>,
    s0: Option<f64>,
    s: Vec<f64>,
    c: Vec<f64>,
    opt: Option<MmaOptimizer>,
    x_km1: Option<Vec<f64>>,
    x_km2: Option<Vec<f64>>,
    calibrated_at: Option<String>,
}

pub type StepInfo = Map<String, Value>;

fn cae(e: impl std::fmt::Display) -> JobError {
    JobError::runtime(e.to_string())
}

impl MmaDriver {

    pub fn new(prob: &Problem, design: &Design) -> JobResult<Self> {
        let spec = &prob.spec;
        let opts = mma_options(spec.settings.get("mma").unwrap_or(&Value::Null))?;
        let mut problems = Vec::new();
        let (mut lo, mut hi) = (Vec::new(), Vec::new());
        for fr in &spec.free {
            let (Some(l), Some(h)) = (fr.lo, fr.hi) else {
                let s = |v: Option<f64>| {
                    v.map_or_else(|| "None".to_string(), implexity_core::py_repr::repr_float)
                };
                problems.push(format!(
                    "free {} has bounds [{}, {}]: the MMA driver moves every design variable inside a box and cannot \
                     take an open side.  Declare both bounds, or set optimizer='adam'.",
                    fr.ref_str(),
                    s(fr.lo),
                    s(fr.hi)
                ));
                continue;
            };
            let (o, sp) = (design.origins[&fr.slot], design.spans[&fr.slot]);
            lo.extend(std::iter::repeat_n((l - o) / sp, fr.size));
            hi.extend(std::iter::repeat_n((h - o) / sp, fr.size));
        }
        if !problems.is_empty() {
            return Err(JobError::optimize(problems));
        }
        let constraints = prob.constraints.clone();
        let occupancy_idx = (0..constraints.len()).filter(|j| !constraints[*j].needs_physics()).collect();
        let physics_idx = (0..constraints.len()).filter(|j| constraints[*j].needs_physics()).collect();
        Ok(Self {
            free: spec.free.clone(),
            design: design.clone(),
            opts,
            n: lo.len(),
            x_min: lo,
            x_max: hi,
            m: constraints.len(),
            constraints,
            occupancy_idx,
            physics_idx,
            s0: None,
            s: Vec::new(),
            c: Vec::new(),
            opt: None,
            x_km1: None,
            x_km2: None,
            calibrated_at: None,
        })
    }

    #[must_use]
    pub fn flatten(&self, d: &NamedArrays) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.n);
        for fr in &self.free {
            if let Some(a) = d.get(&fr.slot) {
                out.extend(a.iter().copied());
            }
        }
        out
    }

    #[must_use]
    pub fn unflatten(&self, x: &[f64]) -> NamedArrays {
        let mut out = NamedArrays::new();
        let mut i = 0;
        for fr in &self.free {
            let a = ArrayD::from_shape_vec(IxDyn(&fr.shape), x[i..i + fr.size].to_vec())
                .unwrap_or_else(|_| ArrayD::zeros(IxDyn(&fr.shape)));
            out.insert(fr.slot.clone(), a);
            i += fr.size;
        }
        out
    }

    fn feas_tol(&self) -> f64 {
        self.opts.get("feas_tol").and_then(|v| py_float(v).ok()).unwrap_or(1e-3)
    }


    pub fn measure(
        &self,
        prob: &Problem,
        z: &NamedArrays,
        drive: Option<&Drive>,
        physics: Option<(&[f64], &[NamedArrays])>,
    ) -> JobResult<Measurement> {
        let p = self.design.p_of(z);
        let mut fs = vec![0.0; self.m];
        let mut dfs = vec![0.0; self.m * self.n];
        for j in &self.occupancy_idx {
            let (v, gp) = prob.occupancy_residual(*j, &p, drive, true)?;
            fs[*j] = v;
            let row = self.flatten(&self.design.grad_z(&gp.unwrap_or_default()));
            dfs[j * self.n..(j + 1) * self.n].copy_from_slice(&row);
        }
        if !self.physics_idx.is_empty() {
            let Some((values, grads)) = physics else {
                return Err(JobError::optimize1(
                    "physics constraints need the fused physics Jacobian at the iterate",
                ));
            };
            for (k, j) in self.physics_idx.iter().enumerate() {
                fs[*j] = values[k];
                let row = self.flatten(&self.design.grad_z(&grads[k]));
                dfs[j * self.n..(j + 1) * self.n].copy_from_slice(&row);
            }
        }
        let max = fs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let feasible = self.m == 0 || max <= self.feas_tol();
        Ok(Measurement { fs, dfs, feasible })
    }

    fn calibrate(
        &mut self,
        f0: f64,
        df0: &[f64],
        fs: &[f64],
        dfs: &[f64],
        where_: &str,
        log: &dyn Fn(&str),
    ) -> JobResult<()> {
        let s0 = 1.0 / f0.abs().max(1e-8);
        self.s0 = Some(s0);
        self.s = fs.iter().map(|v| 1.0 / v.abs().max(1.0)).collect();
        let pinned = self.opts.get("c").filter(|v| !v.is_null()).map(py_float).transpose()?;
        self.c = if let Some(c) = pinned {
            vec![c; self.m]
        } else {
            let g0 = if self.n > 0 {
                df0.iter().map(|v| (s0 * v).abs()).fold(f64::NEG_INFINITY, f64::max)
            } else {
                0.0
            };
            (0..self.m)
                .map(|j| {
                    let gj = if self.n > 0 {
                        dfs[j * self.n..(j + 1) * self.n]
                            .iter()
                            .map(|v| (self.s[j] * v).abs())
                            .fold(f64::NEG_INFINITY, f64::max)
                    } else {
                        0.0
                    };
                    if gj > 0.0 { f64::max(1000.0, 10.0 * g0 / gj) } else { 1000.0 }
                })
                .collect()
        };
        self.calibrated_at = Some(where_.to_string());
        self.build()?;
        let fmt = |v: &[f64]| {
            format!(
                "[{}]",
                v.iter()
                    .map(|x| implexity_core::py_repr::repr_str(&implexity_geometry::pyfmt::fmt_g(*x, 4)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        log(&format!(
            "MMA scaling at {where_}: s0 = {} (f0 = {}), s = {}, c = {}",
            implexity_geometry::pyfmt::fmt_g(s0, 4),
            implexity_geometry::pyfmt::fmt_g(f0, 6),
            fmt(&self.s),
            fmt(&self.c)
        ));
        Ok(())
    }

    fn globalize(&self) -> bool {
        self.opts.get("globalize").and_then(Value::as_bool).unwrap_or(true)
    }

    fn build(&mut self) -> JobResult<()> {
        let o = &self.opts;
        let f = |k: &str| py_float(&o[k]);
        let options = MmaOptions {
            move_limit: f("move_limit")?,
            epsimin: f("epsimin")?,
            max_inner: py_int(&o["max_inner"])?,
            asymptote_init: f("asymptote_init")?,
            asymptote_lo: f("asymptote_lo")?,
            asymptote_hi: f("asymptote_hi")?,
            c: Some(self.c.clone()),
            x_min: self.x_min.clone(),
            x_max: self.x_max.clone(),
            ..MmaOptions::default()
        };
        let n = i64::try_from(self.n).map_err(cae)?;
        let m = i64::try_from(self.m).map_err(cae)?;
        self.opt = Some(if self.globalize() {
            let g =
                GcmmaOptions { max_conservative: py_int(&o["max_conservative"])?, ..GcmmaOptions::default() };
            MmaOptimizer::gcmma(n, m, &options, &g)?
        } else {
            MmaOptimizer::new(n, m, &options)?
        });
        Ok(())
    }


    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        z: &NamedArrays,
        f0: f64,
        gz: &NamedArrays,
        meas: &Measurement,
        where_: &str,
        eval_true: &mut dyn FnMut(&NamedArrays) -> (f64, Vec<f64>, Vec<f64>),
        log: &dyn Fn(&str),
    ) -> JobResult<(NamedArrays, StepInfo)> {
        let x = self.flatten(z);
        let df0 = self.flatten(gz);
        if self.opt.is_none() {
            self.calibrate(f0, &df0, &meas.fs, &meas.dfs, where_, log)?;
        }
        let s0 = self.s0.unwrap_or(1.0);
        let s = self.s.clone();
        let x_km1 = self.x_km1.clone().unwrap_or_else(|| x.clone());
        let x_km2 = self.x_km2.clone().unwrap_or_else(|| x.clone());
        let fs: Vec<f64> = meas.fs.iter().zip(&s).map(|(f, sj)| sj * f).collect();
        let dfs: Vec<f64> = (0..self.m)
            .flat_map(|j| meas.dfs[j * self.n..(j + 1) * self.n].iter().map(|v| s[j] * v).collect::<Vec<_>>())
            .collect();
        let df0s: Vec<f64> = df0.iter().map(|v| s0 * v).collect();
        let occ = self.occupancy_idx.clone();
        let phys = self.physics_idx.clone();
        let m = self.m;
        let s_c = self.s.clone();
        let free = self.free.clone();
        let unflatten = |xt: &[f64]| {
            let mut out = NamedArrays::new();
            let mut i = 0;
            for fr in &free {
                let a = ArrayD::from_shape_vec(IxDyn(&fr.shape), xt[i..i + fr.size].to_vec())
                    .unwrap_or_else(|_| ArrayD::zeros(IxDyn(&fr.shape)));
                out.insert(fr.slot.clone(), a);
                i += fr.size;
            }
            out
        };
        let globalize = self.globalize();
        let opt = self.opt.as_mut().ok_or_else(|| JobError::runtime("MMA optimiser not built"))?;
        let x_new = if globalize {
            let mut probe = |xt: &[f64]| -> (f64, Vec<f64>) {
                let zt = unflatten(xt);
                let (f0t, occ_res, phys_res) = eval_true(&zt);
                let mut fst = vec![0.0; m];
                for (k, j) in occ.iter().enumerate() {
                    fst[*j] = occ_res.get(k).copied().unwrap_or(f64::NAN);
                }
                for (k, j) in phys.iter().enumerate() {
                    fst[*j] = phys_res.get(k).copied().unwrap_or(f64::NAN);
                }
                (s0 * f0t, fst.iter().zip(&s_c).map(|(f, sj)| sj * f).collect())
            };
            opt.step_gcmma(&x, &x_km1, &x_km2, s0 * f0, &df0s, &fs, &dfs, &mut probe).map_err(cae)?
        } else {
            opt.step(&x, &x_km1, &x_km2, s0 * f0, &df0s, &fs, &dfs).map_err(cae)?
        };
        self.x_km2 = Some(x_km1);
        self.x_km1 = Some(x.clone());
        let last = opt.last.clone().ok_or_else(|| JobError::runtime("MMA step recorded no diagnostics"))?;
        let max_move = if self.n > 0 {
            x_new.iter().zip(&x).map(|(a, b)| (a - b).abs()).fold(f64::NEG_INFINITY, f64::max)
        } else {
            0.0
        };
        let mut info = Map::new();
        info.insert("kkt_residual".into(), float_value(last.kkt_residual));
        info.insert("subproblem_kkt".into(), float_value(last.subproblem_kkt));
        info.insert("subproblem_converged".into(), json!(last.subproblem_converged));
        info.insert("inner_iterations".into(), json!(last.inner_iterations));
        info.insert("conservative".into(), json!(last.conservative));
        info.insert(
            "multipliers".into(),
            Value::Array(
                last.dual_variables.iter().zip(&s).map(|(v, sj)| float_value(v * sj / s0)).collect(),
            ),
        );
        info.insert("step_ms".into(), float_value(round3(last.wallclock_s * 1e3)));
        info.insert("max_move".into(), float_value(max_move));
        Ok((self.unflatten(&x_new), info))
    }

    #[must_use]
    pub fn last_kkt(&self) -> Option<f64> {
        self.opt.as_ref().and_then(|o| o.last.as_ref()).map(|l| l.kkt_residual)
    }

    #[must_use]
    pub fn converged(&self) -> bool {
        let tol = self.opts.get("kkt_tol").filter(|v| !v.is_null()).and_then(|v| py_float(v).ok());
        match (tol, &self.opt) {
            (Some(t), Some(o)) => o.converged(t),
            _ => false,
        }
    }


    pub fn reset(&mut self) -> JobResult<()> {
        if let Some(o) = self.opt.as_mut() {
            o.load_state_dict(&MmaStateDict::default())?;
        }
        self.x_km1 = None;
        self.x_km2 = None;
        Ok(())
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            x_km1: self.x_km1.clone(),
            x_km2: self.x_km2.clone(),
            opt: self.opt.as_ref().map(MmaOptimizer::state_dict),
        }
    }


    pub fn restore_snapshot(&mut self, snap: &Snapshot) -> JobResult<()> {
        self.x_km1.clone_from(&snap.x_km1);
        self.x_km2.clone_from(&snap.x_km2);
        if let (Some(o), Some(sd)) = (self.opt.as_mut(), &snap.opt) {
            o.load_state_dict(sd)?;
        }
        Ok(())
    }

    pub fn shrink(&mut self, factor: f64) {
        let ml = self.opts.get("move_limit").and_then(|v| py_float(v).ok()).unwrap_or(0.2) * factor;
        self.opts.insert("move_limit".into(), float_value(ml));
        if let Some(o) = self.opt.as_mut() {
            o.move_limit = ml;
        }
    }

    #[must_use]
    pub fn move_limit(&self) -> f64 {
        self.opts.get("move_limit").and_then(|v| py_float(v).ok()).unwrap_or(f64::NAN)
    }

    #[must_use]
    pub fn nonfinite_trials(&self) -> i64 {
        self.opt.as_ref().map_or(0, MmaOptimizer::nonfinite_trials)
    }

    #[must_use]
    pub fn m(&self) -> usize {
        self.m
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let scales = match self.s0 {
            None => Value::Null,
            Some(s0) => json!({
                "objective": float_value(s0),
                "constraints": self.s.iter().map(|v| float_value(*v)).collect::<Vec<_>>(),
                "c": self.c.iter().map(|v| float_value(*v)).collect::<Vec<_>>(),
                "calibrated_at": self.calibrated_at,
            }),
        };
        json!({
            "kind": if self.globalize() { "gcmma" } else { "mma" },
            "options": self.opts, "design_dim": self.n, "n_constraints": self.m, "scales": scales,
            "constraints": self.constraints.iter().map(Constraint::describe).collect::<Vec<_>>(),
        })
    }


    pub fn save(&self, out_dir: &Path) -> JobResult<(f64, u64)> {
        let mut payload: Vec<(String, NpyArray)> = Vec::new();
        if let Some(x) = &self.x_km1 {
            payload.push(("x_km1".into(), NpyArray::vector_f64(x.clone())));
        }
        if let Some(x) = &self.x_km2 {
            payload.push(("x_km2".into(), NpyArray::vector_f64(x.clone())));
        }
        payload.push(("move_limit".into(), NpyArray::scalar_f64(self.move_limit())));
        if let Some(o) = &self.opt {
            payload.push(("s0".into(), NpyArray::scalar_f64(self.s0.unwrap_or(f64::NAN))));
            payload.push(("s".into(), NpyArray::vector_f64(self.s.clone())));
            payload.push(("c".into(), NpyArray::vector_f64(self.c.clone())));
            if let Some(at) = &self.calibrated_at {
                payload.push(("calibrated_at".into(), NpyArray::scalar_str(at)));
            }
            let sd = o.state_dict();
            if let Some(l) = sd.l {
                payload.push(("L".into(), NpyArray::vector_f64(l)));
            }
            if let Some(u) = sd.u {
                payload.push(("U".into(), NpyArray::vector_f64(u)));
            }
            payload.push(("iter".into(), NpyArray::scalar_i64(sd.iter.unwrap_or(0))));
            payload.push(("kkt".into(), NpyArray::scalar_f64(sd.kkt.unwrap_or(f64::INFINITY))));
            payload.push(("lam".into(), NpyArray::vector_f64(sd.lam.unwrap_or_default())));
            payload.push(("y".into(), NpyArray::vector_f64(sd.y.unwrap_or_default())));
            payload.push(("z".into(), NpyArray::scalar_f64(sd.z.unwrap_or(0.0))));
        }
        implexity_optim::optjob::atomic_savez(&out_dir.join(MMA_STATE_FILE), false, &payload).map_err(cae)
    }


    pub fn load(&mut self, out_dir: &Path, log: &dyn Fn(&str)) -> JobResult<bool> {
        let p = out_dir.join(MMA_STATE_FILE);
        if !p.is_file() {
            log(&format!(
                "no {MMA_STATE_FILE} beside the checkpoint: MMA asymptotes and scales start fresh at the resumed iterate"
            ));
            return Ok(false);
        }
        let npz = implexity_io::npz::load_file(&p).map_err(cae)?;
        let vec =
            |k: &str| npz.get(k).and_then(NpyArray::to_f64).map(|a| a.iter().copied().collect::<Vec<f64>>());
        let scalar = |k: &str| vec(k).and_then(|v| v.first().copied());
        self.x_km1 = vec("x_km1");
        self.x_km2 = vec("x_km2");
        if let Some(ml) = scalar("move_limit") {
            self.opts.insert("move_limit".into(), float_value(ml));
        }
        if let Some(s0) = scalar("s0") {
            self.s0 = Some(s0);
            self.s = vec("s").unwrap_or_default();
            self.c = vec("c").unwrap_or_default();
            if self.s.len() != self.m || self.c.len() != self.m {
                return Err(JobError::value(format!("cannot reshape array into shape ({},)", self.m)));
            }
            self.calibrated_at = Some(match npz.get("calibrated_at").map(|a| &a.data) {
                Some(NpyData::Unicode { values, .. }) => values.first().cloned().unwrap_or_default(),
                _ => "resumed".into(),
            });
            self.build()?;
            let sd = MmaStateDict {
                l: vec("L"),
                u: vec("U"),
                iter: npz.get("iter").and_then(NpyArray::to_i64).and_then(|a| a.iter().next().copied()),
                kkt: scalar("kkt"),
                lam: vec("lam"),
                y: vec("y"),
                z: scalar("z"),
            };
            if let Some(o) = self.opt.as_mut() {
                o.load_state_dict(&sd)?;
                let fmt = |v: &[f64]| {
                    format!(
                        "[{}]",
                        v.iter()
                            .map(|x| implexity_core::py_repr::repr_str(&implexity_geometry::pyfmt::fmt_g(
                                *x, 4
                            )))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                log(&format!(
                    "MMA state restored: iteration {}, s0 = {}, c = {}",
                    sd.iter.unwrap_or(0),
                    implexity_geometry::pyfmt::fmt_g(s0, 4),
                    fmt(&self.c)
                ));
            }
        }
        Ok(true)
    }
}

#[must_use]
pub fn round3(x: f64) -> f64 {
    implexity_mesh::numeric::py_round_digits(x, 3)
}
