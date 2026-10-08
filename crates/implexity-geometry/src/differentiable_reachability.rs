// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use crate::error::{GResult, GeometryError};
use crate::scalar::Scalar;

pub const REACHABILITY_TELEMETRY_SCHEMA: &str = "implexity-neutral-seeded-reachability-telemetry/1";

fn verr(m: &str) -> GeometryError {
    GeometryError::Value(m.into())
}


pub fn smooth_gate_membership<S: Scalar>(
    phase_fraction: &[S],
    threshold: f64,
    sharpness: f64,
) -> GResult<Vec<S>> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(verr("threshold must be finite data in [0,1]"));
    }
    if !sharpness.is_finite() || sharpness <= 0.0 {
        return Err(verr("sharpness must be positive finite data"));
    }
    if phase_fraction.is_empty() {
        return Err(verr("phase_fraction must be a nonempty array"));
    }
    Ok(phase_fraction.iter().map(|f| ((f.clip_c(0.0, 1.0) - threshold) * sharpness).sigmoid()).collect())
}


pub fn weighted_tail_deficit<S: Scalar>(deficit: &[S], weights: Option<&[f64]>, p: f64) -> GResult<S> {
    if !p.is_finite() || p < 1.0 {
        return Err(verr("p must be finite data greater than or equal to one"));
    }
    if deficit.is_empty() {
        return Err(verr("deficit must be a nonempty array"));
    }
    let n = deficit.len();
    let w: Vec<f64> = match weights {
        None => vec![1.0; n],
        Some(w) if w.len() == n => w.iter().map(|x| x.max(0.0)).collect(),
        Some(w) if w.len() == 1 => vec![w[0].max(0.0); n],
        Some(_) => return Err(verr("weights must broadcast to the deficit shape")),
    };
    let values: Vec<S> = deficit.iter().map(|d| d.max_c(0.0)).collect();
    let value_scale = values
        .iter()
        .zip(&w)
        .map(|(v, wi)| if *wi > 0.0 { v.val() } else { 0.0 })
        .fold(f64::NEG_INFINITY, f64::max);
    let weight_scale = w.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let safe_v = if value_scale > 0.0 { value_scale } else { 1.0 };
    let safe_w = if weight_scale > 0.0 { weight_scale } else { 1.0 };
    let mut numerator = S::cst(0.0);
    let mut denominator = 0.0;
    for (v, wi) in values.iter().zip(&w) {
        let sw = wi / safe_w;
        numerator = numerator + (*v / safe_v).powf(p) * sw;
        denominator += sw;
    }
    let active = numerator.val() > 0.0 && denominator > 0.0;
    if !active {
        return Ok(S::cst(0.0));
    }
    let moment = numerator / denominator;
    Ok(moment.powf(1.0 / p) * value_scale)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReachabilityOptions {
    pub spacing_m: [f64; 3],
    pub reach_m: f64,
    pub exponent: f64,
    pub leak: f64,
    pub tolerance: f64,
    pub max_iterations: usize,
    pub attempt_limit: usize,
}

impl ReachabilityOptions {
    #[must_use]
    pub fn new(spacing_m: [f64; 3], reach_m: f64) -> Self {
        Self {
            spacing_m,
            reach_m,
            exponent: 3.0,
            leak: 1.0e-6,
            tolerance: 1.0e-6,
            max_iterations: 640,
            attempt_limit: 2,
        }
    }
}

struct Operator {
    shape: [usize; 3],
    screen: f64,
    seed_coef: Vec<f64>,
    faces: [Vec<f64>; 3],
    diagonal: Vec<f64>,
}

impl Operator {
    fn stride(&self, a: usize) -> usize {
        [self.shape[1] * self.shape[2], self.shape[2], 1][a]
    }

    fn face_index(&self, a: usize, c: usize) -> Option<usize> {

        let [nx, ny, nz] = self.shape;
        let (i, j, k) = (c / (ny * nz), (c / nz) % ny, c % nz);
        match a {
            0 if i + 1 < nx => Some((i * ny + j) * nz + k),
            1 if j + 1 < ny => Some((i * (ny - 1) + j) * nz + k),
            2 if k + 1 < nz => Some((i * ny + j) * (nz - 1) + k),
            _ => None,
        }
    }

    fn apply(&self, v: &[f64]) -> Vec<f64> {
        let mut out: Vec<f64> = v.iter().zip(&self.seed_coef).map(|(x, s)| self.screen * x + s * x).collect();
        for a in 0..3 {
            let st = self.stride(a);
            for c in 0..v.len() {
                if let Some(f) = self.face_index(a, c) {
                    let flux = self.faces[a][f] * (v[c] - v[c + st]);
                    out[c] += flux;
                    out[c + st] -= flux;
                }
            }
        }
        out
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    crate::numpy::norm(a)
}

fn cg(op: &Operator, b: &[f64], x0: Option<&[f64]>, tol: f64, maxiter: usize) -> Vec<f64> {
    let atol2 = (tol * tol) * dot(b, b);
    let mut x = x0.map_or_else(|| vec![0.0; b.len()], <[f64]>::to_vec);
    let ax = op.apply(&x);
    let mut r: Vec<f64> = b.iter().zip(&ax).map(|(bi, a)| bi - a).collect();
    let mut z: Vec<f64> = r.iter().zip(&op.diagonal).map(|(ri, d)| ri / d).collect();
    let mut p = z.clone();
    let mut gamma = dot(&r, &z);
    let mut k = 0;
    while dot(&r, &r) > atol2 && k < maxiter {
        let ap = op.apply(&p);
        let alpha = gamma / dot(&p, &ap);
        for i in 0..x.len() {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        z = r.iter().zip(&op.diagonal).map(|(ri, d)| ri / d).collect();
        let gamma_new = dot(&r, &z);
        let beta = gamma_new / gamma;
        gamma = gamma_new;
        for i in 0..p.len() {
            p[i] = z[i] + beta * p[i];
        }
        k += 1;
    }
    x
}

fn relative_residual(op: &Operator, x: &[f64], b: &[f64]) -> f64 {
    let ax = op.apply(x);
    let diff: Vec<f64> = ax.iter().zip(b).map(|(a, bi)| a - bi).collect();
    norm(&diff) / norm(b).max(f64::MIN_POSITIVE)
}

struct Solve {
    value: Vec<f64>,
    telemetry: Value,
    residual: f64,
}

fn bounded_solve(op: &Operator, b: &[f64], o: &ReachabilityOptions) -> Solve {
    let first = cg(op, b, None, o.tolerance, o.max_iterations);
    let first_res = relative_residual(op, &first, b);
    let first_finite = first.iter().all(|v| v.is_finite()) && first_res.is_finite();
    let mut value = first;
    let mut retries = 0usize;
    for _ in 1..o.attempt_limit {
        let cur = relative_residual(op, &value, b);
        let finite = value.iter().all(|v| v.is_finite()) && cur.is_finite();
        if !finite || cur > o.tolerance {
            let init = if finite { value.clone() } else { vec![0.0; value.len()] };
            value = cg(op, b, Some(&init), o.tolerance, o.max_iterations);
            retries += 1;
        }
    }
    let final_res = relative_residual(op, &value, b);
    let attempts = retries + 1;
    let telemetry = json!({
        "attempt_count": attempts, "attempt_limit": o.attempt_limit,
        "attempted_iteration_budget": attempts * o.max_iterations,
        "final_relative_residual": final_res, "first_attempt_finite": first_finite,
        "first_attempt_relative_residual": first_res, "initial_relative_residual": first_res,
        "max_iterations_per_attempt": o.max_iterations, "requested_relative_tolerance": o.tolerance,
        "retry_count": retries, "total_iteration_budget": o.attempt_limit * o.max_iterations,
    });
    Solve { value, telemetry, residual: final_res }
}

pub struct Reachability {
    pub value: Vec<f64>,
    pub residual: f64,
    pub telemetry: Value,
    raw: Vec<f64>,
    input: Vec<f64>,
    phase: Vec<f64>,
    seed: Vec<bool>,
    op: Operator,
    opts: ReachabilityOptions,
    reference_spacing: f64,
}


pub fn seeded_phase_reachability(
    phase_fraction: &[f64],
    shape: [usize; 3],
    seed_mask: &[bool],
    o: &ReachabilityOptions,
) -> GResult<Reachability> {
    let n: usize = shape.iter().product();
    if shape.contains(&0) || phase_fraction.len() != n {
        return Err(verr("phase_fraction must be a nonempty three-dimensional array"));
    }
    if seed_mask.len() != n {
        return Err(verr("seed_mask must be a Boolean array with the phase shape"));
    }
    if !seed_mask.iter().any(|s| *s) {
        return Err(verr("seed_mask must contain at least one seed cell"));
    }
    if o.spacing_m.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(verr("spacing_m must be one positive scalar or three positive values"));
    }
    if !o.reach_m.is_finite() || o.reach_m <= 0.0 {
        return Err(verr("reach_m must be positive finite data"));
    }
    if !o.exponent.is_finite() || o.exponent <= 0.0 {
        return Err(verr("exponent must be positive finite data"));
    }
    if !o.leak.is_finite() || !(0.0 < o.leak && o.leak < 1.0) {
        return Err(verr("leak must lie strictly between zero and one"));
    }
    if !o.tolerance.is_finite() || o.tolerance <= 0.0 {
        return Err(verr("tolerance must be positive finite data"));
    }
    if o.max_iterations < 1 {
        return Err(verr("max_iterations must be a positive integer"));
    }
    if o.attempt_limit < 1 {
        return Err(verr("attempt_limit must be a positive integer"));
    }
    let phase: Vec<f64> = phase_fraction.iter().map(|p| p.clamp(0.0, 1.0)).collect();
    let cond: Vec<f64> = phase.iter().map(|p| p.powf(o.exponent) + o.leak).collect();
    let seed_coef: Vec<f64> = cond.iter().zip(seed_mask).map(|(c, s)| if *s { *c } else { 0.0 }).collect();
    let reference_spacing = o.spacing_m.iter().copied().fold(f64::INFINITY, f64::min);
    let mut op = Operator {
        shape,
        screen: (reference_spacing / o.reach_m).powi(2),
        seed_coef,
        faces: [Vec::new(), Vec::new(), Vec::new()],
        diagonal: Vec::new(),
    };
    let faces = face_weights(&op, &cond, o.spacing_m, reference_spacing);
    op.faces = faces;
    let mut diagonal: Vec<f64> = op.seed_coef.iter().map(|s| op.screen + s).collect();
    for a in 0..3 {
        let st = op.stride(a);
        for c in 0..n {
            if let Some(f) = op.face_index(a, c) {
                diagonal[c] += op.faces[a][f];
                diagonal[c + st] += op.faces[a][f];
            }
        }
    }
    op.diagonal = diagonal;
    let rhs = op.seed_coef.clone();
    let s = bounded_solve(&op, &rhs, o);
    let value = s.value.iter().map(|v| v.clamp(0.0, 1.0)).collect();
    Ok(Reachability {
        value,
        residual: s.residual,
        telemetry: s.telemetry,
        raw: s.value,
        input: phase_fraction.to_vec(),
        phase,
        seed: seed_mask.to_vec(),
        op,
        opts: o.clone(),
        reference_spacing,
    })
}

fn face_weights(op: &Operator, cond: &[f64], spacing: [f64; 3], reference: f64) -> [Vec<f64>; 3] {
    let [nx, ny, nz] = op.shape;
    let sizes = [
        (nx.saturating_sub(1)) * ny * nz,
        nx * (ny.saturating_sub(1)) * nz,
        nx * ny * (nz.saturating_sub(1)),
    ];
    let mut out: [Vec<f64>; 3] = std::array::from_fn(|a| vec![0.0; sizes[a]]);
    for a in 0..3 {
        let st = op.stride(a);
        let scale = (reference / spacing[a]).powi(2);
        for c in 0..cond.len() {
            if let Some(f) = op.face_index(a, c) {
                let (x, y) = (cond[c], cond[c + st]);
                out[a][f] = 2.0 * x * y / (x + y) * scale;
            }
        }
    }
    out
}

impl Reachability {

    pub fn vjp(&self, cotangent: &[f64]) -> GResult<Vec<f64>> {
        let n = self.raw.len();
        if cotangent.len() != n {
            return Err(verr("cotangent must match the phase shape"));
        }

        let g: Vec<f64> = cotangent
            .iter()
            .zip(&self.raw)
            .map(|(c, v)| {
                let w = if *v > 0.0 && *v < 1.0 {
                    1.0
                } else if *v == 0.0 || *v == 1.0 {
                    0.5
                } else {
                    0.0
                };
                c * w
            })
            .collect();
        let lam = bounded_solve(&self.op, &g, &self.opts).value;
        let v = &self.raw;
        let o = &self.opts;

        let mut d_cond: Vec<f64> =
            (0..n).map(|c| if self.seed[c] { lam[c] - lam[c] * v[c] } else { 0.0 }).collect();

        for a in 0..3 {
            let st = self.op.stride(a);
            let scale = (self.reference_spacing / o.spacing_m[a]).powi(2);
            let cond: Vec<f64> = self.phase.iter().map(|p| p.powf(o.exponent) + o.leak).collect();
            for c in 0..n {
                if self.op.face_index(a, c).is_some() {
                    let dw = -(lam[c] - lam[c + st]) * (v[c] - v[c + st]);
                    let (x, y) = (cond[c], cond[c + st]);
                    let s2 = (x + y) * (x + y);
                    d_cond[c] += dw * scale * 2.0 * y * y / s2;
                    d_cond[c + st] += dw * scale * 2.0 * x * x / s2;
                }
            }
        }
        let clip_w = |x: f64| {
            if x > 0.0 && x < 1.0 {
                1.0
            } else if x == 0.0 || x == 1.0 {
                0.5
            } else {
                0.0
            }
        };
        Ok(d_cond
            .iter()
            .zip(&self.phase)
            .zip(&self.input)
            .map(|((d, p), x)| d * o.exponent * p.powf(o.exponent - 1.0) * clip_w(*x))
            .collect())
    }
}

