// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use implexity_ad::scan::{ScanObjective, ScanStep, TermGradient};
use implexity_ad::{AdError, Dual, HyperDual, Scalar};
use implexity_core::CaeError;
use implexity_linalg::sparse::CsrMatrix;

use super::model::SoftModel;
use crate::util::contract;

pub mod avf;
pub mod second;
pub mod state;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scheme {
    Quasistatic,
    Newmark {
        beta: f64,
        gamma: f64,
    },
    GeneralizedAlpha {
        rho_inf: f64,
    },
    AvfMidpoint {
        gauss_points: usize,
    },
}

#[derive(Debug, Clone, Copy)]
struct Coeffs {
    am: f64,
    af: f64,
    beta: f64,
    gamma: f64,
    inertia: bool,
}

impl Scheme {
    fn coeffs(self) -> Coeffs {
        match self {
            Self::Quasistatic => Coeffs { am: 0.0, af: 0.0, beta: 0.25, gamma: 0.5, inertia: false },
            Self::Newmark { beta, gamma } => Coeffs { am: 0.0, af: 0.0, beta, gamma, inertia: true },

            Self::AvfMidpoint { .. } => Coeffs { am: 0.0, af: 0.0, beta: 0.25, gamma: 0.5, inertia: true },
            Self::GeneralizedAlpha { rho_inf } => {
                let am = (2.0 * rho_inf - 1.0) / (rho_inf + 1.0);
                let af = rho_inf / (rho_inf + 1.0);
                let gamma = 0.5 - am + af;
                Coeffs { am, af, beta: 0.25 * (1.0 - am + af).powi(2), gamma, inertia: true }
            }
        }
    }


    pub fn validate(self) -> Result<(), CaeError> {
        match self {
            Self::Quasistatic => Ok(()),
            Self::Newmark { beta, gamma }
                if beta.is_finite()
                    && gamma.is_finite()
                    && beta > 0.0
                    && gamma >= 0.5
                    && beta >= 0.25 * gamma =>
            {
                Ok(())
            }
            Self::GeneralizedAlpha { rho_inf } if (0.0..=1.0).contains(&rho_inf) => Ok(()),
            Self::AvfMidpoint { gauss_points } if (1..=avf::MAX_GAUSS_POINTS).contains(&gauss_points) => {
                Ok(())
            }
            Self::AvfMidpoint { .. } => contract("avf_midpoint requires 1..8 Gauss-Legendre points"),
            Self::Newmark { .. } => {
                contract("newmark requires gamma >= 1/2 and beta >= gamma/4 > 0 (unconditional stability)")
            }
            Self::GeneralizedAlpha { .. } => {
                contract("generalized_alpha requires a spectral radius rho_inf in [0, 1]")
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NewtonOptions {
    pub max_iterations: usize,
    pub relative_tolerance: f64,
}

impl Default for NewtonOptions {
    fn default() -> Self {
        Self { max_iterations: 50, relative_tolerance: 1e-10 }
    }
}


#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactorizationReuse {
    pub contraction: f64,
    pub max_reuse: usize,
}

impl Default for FactorizationReuse {
    fn default() -> Self {
        Self { contraction: 0.5, max_reuse: 8 }
    }
}

impl FactorizationReuse {

    pub fn validate(self) -> Result<(), CaeError> {
        if !(self.contraction > 0.0 && self.contraction < 1.0) || self.max_reuse == 0 {
            return contract("factorization reuse requires 0 < contraction < 1 and max_reuse >= 1");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Loading {
    pub times: Vec<f64>,
    pub prescribed: Vec<f64>,
    pub displacement_amplitude: Vec<f64>,
    pub force: Vec<f64>,
    pub force_amplitude: Vec<f64>,
    pub pressure: f64,
    pub pressure_amplitude: Vec<f64>,
    pub initial_velocity: Vec<f64>,
}

impl Loading {
    #[must_use]
    pub fn steps(&self) -> usize {
        self.times.len().saturating_sub(1)
    }

    fn validate(&self, n3: usize, fixed: &[bool]) -> Result<(), CaeError> {
        let n = self.steps();
        if !(1..=100_000).contains(&n)
            || self.times[0] != 0.0
            || self
                .times
                .windows(2)
                .any(|w| w[1].partial_cmp(&w[0]) != Some(std::cmp::Ordering::Greater) || !w[1].is_finite())
        {
            return contract("times_s must start at 0 and increase strictly (1..100000 steps)");
        }
        let finite = |v: &[f64]| v.iter().copied().all(f64::is_finite);
        if self.prescribed.len() != n3 || self.force.len() != n3 || self.initial_velocity.len() != n3 {
            return contract(
                "prescribed displacement, nodal force and initial velocity must be node-by-XYZ arrays",
            );
        }
        if [&self.displacement_amplitude, &self.force_amplitude, &self.pressure_amplitude]
            .iter()
            .any(|a| a.len() != n)
        {
            return contract("every amplitude history needs one value per step");
        }
        if !finite(&self.prescribed)
            || !finite(&self.force)
            || !finite(&self.initial_velocity)
            || !finite(&self.displacement_amplitude)
            || !finite(&self.force_amplitude)
            || !finite(&self.pressure_amplitude)
            || !self.pressure.is_finite()
        {
            return contract("loads, amplitudes and initial velocity must be finite");
        }
        if self.initial_velocity.iter().zip(fixed).any(|(v, f)| *f && *v != 0.0) {
            return contract("fixed displacement components must start at rest");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub n3: usize,
    pub np: usize,
    pub nb: usize,
    pub ne_visc: usize,
}

impl Layout {
    #[must_use]
    pub const fn u(&self) -> usize {
        0
    }
    #[must_use]
    pub const fn v(&self) -> usize {
        self.n3
    }
    #[must_use]
    pub const fn a(&self) -> usize {
        2 * self.n3
    }
    #[must_use]
    pub const fn r(&self) -> usize {
        3 * self.n3
    }
    #[must_use]
    pub const fn p(&self) -> usize {
        4 * self.n3
    }
    #[must_use]
    pub const fn s(&self) -> usize {
        4 * self.n3 + self.np
    }
    #[must_use]
    pub const fn q(&self) -> usize {
        self.s() + 6 * self.ne_visc
    }
    #[must_use]
    pub const fn len(&self) -> usize {
        self.q() + 6 * self.nb * self.ne_visc
    }
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone)]
struct Context {
    rho: Vec<f64>,
    theta: Vec<f64>,
    stiffness: Vec<f64>,
    mass: Vec<f64>,
    m: CsrMatrix,
    kref: CsrMatrix,
}

#[derive(Debug, Clone)]
pub struct StepSolution {
    pub state: Vec<f64>,
    pub iterations: usize,
    pub free_residual: f64,
    pub unknowns: Vec<f64>,
    pub factorizations: usize,
}

#[derive(Debug, Clone)]
pub struct StepCotangent {
    pub state: Vec<f64>,
    pub params: Vec<f64>,
    pub extra_load: Vec<f64>,
}

pub struct SoftHistory<'m> {
    pub model: &'m SoftModel,
    pub scheme: Scheme,
    pub loading: Loading,
    pub rayleigh: (f64, f64),
    pub newton: NewtonOptions,
    pub layout: Layout,
    params: Vec<f64>,
    ctx: Context,
    cache: Mutex<(HashMap<usize, Vec<f64>>, VecDeque<usize>)>,
    cache_capacity: usize,
    time_scale: f64,
    periodic_loading: bool,
    pub iterations: Mutex<Vec<(usize, usize)>>,
    reuse: Option<FactorizationReuse>,
    patterns: Vec<(Vec<f64>, Vec<f64>)>,
}

pub const DENSITY_ROUNDING_SLACK: f64 = 64.0 * f64::EPSILON;

fn ad(e: &CaeError) -> AdError {
    AdError::Invalid(e.message().to_string())
}

impl<'m> SoftHistory<'m> {

    pub fn new(
        model: &'m SoftModel,
        scheme: Scheme,
        loading: Loading,
        rayleigh: (f64, f64),
        newton: NewtonOptions,
        mut params: Vec<f64>,
    ) -> Result<Self, CaeError> {
        scheme.validate()?;
        let n3 = 3 * model.n();
        loading.validate(n3, &model.fixed)?;
        let ne = model.ne();


        for r in params.iter_mut().take(ne) {
            let excess = if *r > 1.0 { *r - 1.0 } else { -*r };
            if excess > 0.0 && excess <= DENSITY_ROUNDING_SLACK {
                *r = r.clamp(0.0, 1.0);
            }
        }
        if params.len() != 2 * ne
            || params.iter().any(|v| !v.is_finite())
            || params[..ne].iter().any(|r| !(0.0..=1.0).contains(r))
        {
            return contract("physical densities in [0, 1] and finite fibre angles are required per element");
        }
        if !(rayleigh.0.is_finite() && rayleigh.1.is_finite() && rayleigh.0 >= 0.0 && rayleigh.1 >= 0.0) {
            return contract("rayleigh damping coefficients must be finite and nonnegative");
        }
        if !(newton.max_iterations >= 1
            && newton.relative_tolerance > 0.0
            && newton.relative_tolerance < 1e-2)
        {
            return contract("newton requires max_iterations >= 1 and 0 < relative_tolerance < 1e-2");
        }
        let visc = model.materials.iter().any(|m| m.prony.is_some());
        let nb =
            model.materials.iter().filter_map(|m| m.prony.as_ref().map(|p| p.beta.len())).max().unwrap_or(0);
        let layout = Layout {
            n3,
            np: if model.formulation.mixed() { model.n() } else { 0 },
            nb,
            ne_visc: if visc { ne } else { 0 },
        };
        let rho = params[..ne].to_vec();
        let theta = params[ne..].to_vec();
        let stiffness: Vec<f64> = rho.iter().map(|r| model.interpolation.stiffness(*r)).collect();
        let mass: Vec<f64> = rho.iter().map(|r| model.interpolation.mass(*r)).collect();
        let (m, kref) = if scheme == Scheme::Quasistatic {

            let empty = || CsrMatrix::try_new(n3, n3, vec![0; n3 + 1], Vec::new(), Vec::new());
            (
                empty().map_err(|e| CaeError::contract(e.to_string()))?,
                empty().map_err(|e| CaeError::contract(e.to_string()))?,
            )
        } else {
            model.global_mass_and_reference(&mass, &stiffness)?
        };
        if scheme != Scheme::Quasistatic
            && model
                .materials
                .iter()
                .any(|m| m.density.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater))
        {
            return contract("dynamic schemes require a positive material density");
        }
        let steps = loading.steps();
        Ok(Self {
            model,
            scheme,
            loading,
            rayleigh,
            newton,
            layout,
            params,
            ctx: Context { rho, theta, stiffness, mass, m, kref },
            cache: Mutex::new((HashMap::new(), VecDeque::new())),
            cache_capacity: steps + 1,
            time_scale: 1.0,
            periodic_loading: false,
            iterations: Mutex::new(Vec::new()),
            reuse: None,
            patterns: Vec::new(),
        })
    }


    pub fn with_prescribed_patterns(mut self, patterns: Vec<(Vec<f64>, Vec<f64>)>) -> Result<Self, CaeError> {
        let (n3, steps) = (self.layout.n3, self.loading.steps());
        for (pattern, amplitude) in &patterns {
            if pattern.len() != n3 || amplitude.len() != steps {
                return contract(format!(
                    "prescribed-displacement patterns need {n3} components and {steps} amplitudes"
                ));
            }
            if pattern.iter().chain(amplitude).any(|v| !v.is_finite()) {
                return contract("prescribed-displacement patterns must be finite");
            }
        }
        self.patterns = patterns;
        self.cache.lock().map_err(|_| CaeError::contract("soft history cache poisoned"))?.0.clear();
        Ok(self)
    }

    #[must_use]
    pub fn prescribed_patterns(&self) -> &[(Vec<f64>, Vec<f64>)] {
        &self.patterns
    }

    #[must_use]
    pub fn prescribed_displacement(&self, t: usize, i: usize) -> f64 {
        self.patterns
            .iter()
            .fold(self.loading.prescribed[i] * self.loading.displacement_amplitude[t], |acc, (p, a)| {
                acc + p[i] * a[t]
            })
    }

    #[must_use]
    pub fn prescribes_support_rates(&self) -> bool {
        match self.scheme {
            Scheme::Quasistatic => false,
            Scheme::AvfMidpoint { .. } => true,
            _ => (self.scheme.coeffs().gamma - 0.5).abs() < 1e-12,
        }
    }

    fn motion_sample(&self, k: isize, i: usize) -> (f64, f64) {
        let times = &self.loading.times;
        let n = self.loading.steps();
        let h0 = times[1] - times[0];
        let (time, value) = match usize::try_from(k) {
            Err(_) => (times[0] - h0, 0.0),
            Ok(0) if self.periodic_loading => (times[0], self.prescribed_displacement(n - 1, i)),
            Ok(0) => (times[0], 0.0),
            Ok(k) if k <= n => (times[k], self.prescribed_displacement(k - 1, i)),
            Ok(_) => (times[n] + h0, self.prescribed_displacement(0, i)),
        };
        (self.time_scale * time, value)
    }

    #[must_use]
    pub fn prescribed_rates(&self, t: usize) -> (Vec<f64>, Vec<f64>) {
        let n3 = self.layout.n3;
        let mut velocity = vec![0.0; n3];
        let mut acceleration = vec![0.0; n3];
        let n = self.loading.steps();
        #[allow(clippy::cast_possible_wrap)]
        let k = (t + 1) as isize;
        #[allow(clippy::cast_possible_wrap)]
        let last = n as isize;
        let (first, central) = if k < last || self.periodic_loading { (k - 1, true) } else { (k - 2, false) };
        for i in (0..n3).filter(|i| self.model.fixed[*i]) {
            let (ta, ya) = self.motion_sample(first, i);
            let (tb, yb) = self.motion_sample(first + 1, i);
            let (tc, yc) = self.motion_sample(first + 2, i);
            let (h1, h2) = (tb - ta, tc - tb);
            velocity[i] = if central {
                -h2 / (h1 * (h1 + h2)) * ya + (h2 - h1) / (h1 * h2) * yb + h1 / (h2 * (h1 + h2)) * yc
            } else {
                h2 / (h1 * (h1 + h2)) * ya - (h1 + h2) / (h1 * h2) * yb
                    + (h1 + 2.0 * h2) / (h2 * (h1 + h2)) * yc
            };
            acceleration[i] = 2.0 * (ya / (h1 * (h1 + h2)) - yb / (h1 * h2) + yc / (h2 * (h1 + h2)));
        }
        (velocity, acceleration)
    }


    pub fn with_factorization_reuse(mut self, reuse: Option<FactorizationReuse>) -> Result<Self, CaeError> {
        if let Some(r) = reuse {
            r.validate()?;
        }
        self.reuse = reuse;
        self.cache.lock().map_err(|_| CaeError::contract("soft history cache poisoned"))?.0.clear();
        Ok(self)
    }

    #[must_use]
    pub fn factorization_reuse(&self) -> Option<FactorizationReuse> {
        self.reuse
    }

    pub fn set_cache_capacity(&mut self, capacity: usize) {
        self.cache_capacity = capacity.max(1);
    }

    #[must_use]
    pub fn params(&self) -> &[f64] {
        &self.params
    }


    pub fn initial_state(&self) -> Result<Vec<f64>, CaeError> {
        let l = self.layout;
        let mut x = vec![0.0; l.len()];
        let v0 = &self.loading.initial_velocity;
        x[l.v()..l.v() + l.n3].copy_from_slice(v0);
        if self.scheme != Scheme::Quasistatic && v0.iter().any(|v| *v != 0.0) {
            let cv = self.damping_matvec(v0)?;
            if cv.iter().any(|c| *c != 0.0) {
                let a0 = self.solve_mass(&cv.iter().map(|c| -c).collect::<Vec<_>>())?;
                x[l.a()..l.a() + l.n3].copy_from_slice(&a0);
            }
        }
        Ok(x)
    }


    pub fn initial_state_vjp(&self, x0: &[f64], lambda: &[f64]) -> Result<Vec<f64>, CaeError> {
        let l = self.layout;
        let m = self.model;
        let ne = m.ne();
        let mut out = vec![0.0; 2 * ne];
        let v0 = &self.loading.initial_velocity;
        let la = &lambda[l.a()..l.a() + l.n3];
        if self.scheme == Scheme::Quasistatic || v0.iter().all(|v| *v == 0.0) || la.iter().all(|v| *v == 0.0)
        {
            return Ok(out);
        }
        let masked: Vec<f64> = la.iter().zip(&m.fixed).map(|(v, f)| if *f { 0.0 } else { *v }).collect();
        let z = self.solve_mass(&masked)?;
        let a0 = &x0[l.a()..l.a() + l.n3];
        let (am, bk) = self.rayleigh;
        for (e, t) in m.mesh.elements.iter().enumerate() {
            let me0 = m.element_mass(e, 1.0);
            let ke = m.element_reference_stiffness(e);
            let dm = HyperDual::new(self.ctx.rho[e], 1.0, 0.0, 0.0);
            let dmass = m.interpolation.mass(dm).e1;
            let dstiff = m.interpolation.stiffness(dm).e1;
            let mut s = 0.0;
            for a in 0..4 {
                for b in 0..4 {
                    for i in 0..3 {
                        let za = z[3 * t[a] + i];
                        s += dmass * za * me0[a][b] * (a0[3 * t[b] + i] + am * v0[3 * t[b] + i]);
                        for j in 0..3 {
                            s += dstiff * bk * za * ke[(3 * a + i) * 12 + 3 * b + j] * v0[3 * t[b] + j];
                        }
                    }
                }
            }
            out[e] -= s;
        }
        Ok(out)
    }

    fn damping_matvec(&self, v: &[f64]) -> Result<Vec<f64>, CaeError> {
        let (am, bk) = self.rayleigh;
        let mv = self.ctx.m.matvec(v).map_err(|e| CaeError::contract(e.to_string()))?;
        let kv = self.ctx.kref.matvec(v).map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(mv.iter().zip(&kv).map(|(a, b)| am * a + bk * b).collect())
    }

    fn solve_mass(&self, b: &[f64]) -> Result<Vec<f64>, CaeError> {
        let free: Vec<usize> = (0..self.layout.n3).filter(|i| !self.model.fixed[*i]).collect();
        let mut index = vec![usize::MAX; self.layout.n3];
        for (k, i) in free.iter().enumerate() {
            index[*i] = k;
        }
        let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
        for i in &free {
            let (cols, vals) = self.ctx.m.row(*i);
            for (j, val) in cols.iter().zip(vals) {
                if index[*j] != usize::MAX {
                    r.push(index[*i]);
                    c.push(index[*j]);
                    v.push(*val);
                }
            }
        }
        let a = implexity_linalg::sparse::CscMatrix::from_triplets(free.len(), free.len(), &r, &c, &v)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let rhs: Vec<f64> = free.iter().map(|i| b[*i]).collect();
        let z = implexity_linalg::lu::spsolve(&a, &rhs)
            .map_err(|_| CaeError::contract("the free mass matrix is singular (zero-mass free nodes)"))?;
        let mut out = vec![0.0; self.layout.n3];
        for (k, i) in free.iter().enumerate() {
            out[*i] = z[k];
        }
        Ok(out)
    }

    fn dt(&self, t: usize) -> f64 {
        self.time_scale * (self.loading.times[t + 1] - self.loading.times[t])
    }


    pub fn with_time_scale(mut self, time_scale: f64) -> Result<Self, CaeError> {
        if !(time_scale.is_finite() && time_scale > 0.0) {
            return contract("the time scale must be positive and finite");
        }
        self.time_scale = time_scale;
        self.cache.lock().map_err(|_| CaeError::contract("soft history cache poisoned"))?.0.clear();
        Ok(self)
    }

    #[must_use]
    pub fn time_scale(&self) -> f64 {
        self.time_scale
    }

    #[must_use]
    pub fn with_periodic_loading(mut self) -> Self {
        self.periodic_loading = true;
        self
    }

    #[must_use]
    pub fn step_length(&self, t: usize) -> f64 {
        self.dt(t)
    }

    fn amplitude_before(&self, amplitudes: &[f64], t: usize) -> f64 {
        if t > 0 {
            amplitudes[t - 1]
        } else if self.periodic_loading {
            amplitudes.last().copied().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    fn branch_factors(&self, e: usize, dt: f64) -> (Vec<(f64, f64)>, f64) {
        match &self.model.materials[self.model.element_material[e]].prony {
            Some(p) => (p.factors(dt), p.step_factor(dt)),
            None => (Vec::new(), 1.0),
        }
    }

    fn history_h(&self, x: &[f64], e: usize, dt: f64) -> [f64; 6] {
        let l = self.layout;
        let mut h = [0.0; 6];
        if l.ne_visc == 0 {
            return h;
        }
        let (factors, _) = self.branch_factors(e, dt);
        let s = &x[l.s() + 6 * e..l.s() + 6 * e + 6];
        for (i, (ei, bi)) in factors.iter().enumerate() {
            let q = &x[l.q() + 6 * (e * l.nb + i)..l.q() + 6 * (e * l.nb + i) + 6];
            for k in 0..6 {
                h[k] += ei * q[k] - bi * s[k];
            }
        }
        h
    }

    fn dead_load(&self, t: usize, extra: Option<&[f64]>) -> Vec<f64> {
        let amp = self.loading.force_amplitude[t];
        let mut f: Vec<f64> = self.loading.force.iter().map(|v| v * amp).collect();
        if let Some(x) = extra {
            for (a, b) in f.iter_mut().zip(x) {
                *a += b;
            }
        }
        f
    }

    fn pressure(&self, t: usize) -> f64 {
        self.loading.pressure * self.loading.pressure_amplitude[t]
    }

    fn expand(&self, t: usize, y: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let m = self.model;
        let u: Vec<f64> = (0..self.layout.n3)
            .map(|i| if m.fixed[i] { self.prescribed_displacement(t, i) } else { y[m.unknown[i]] })
            .collect();
        let p: Vec<f64> = (0..self.layout.np).map(|a| y[m.unknown[self.layout.n3 + a]]).collect();
        (u, p)
    }

    fn newmark(&self, t: usize, x: &[f64], u1: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let l = self.layout;
        let c = self.scheme.coeffs();
        if !c.inertia {
            return (vec![0.0; l.n3], vec![0.0; l.n3]);
        }
        let dt = self.dt(t);
        let c1 = 1.0 / (c.beta * dt * dt);
        let c2 = 1.0 / (2.0 * c.beta) - 1.0;
        let a1: Vec<f64> =
            (0..l.n3).map(|i| c1 * (u1[i] - x[l.u() + i] - dt * x[l.v() + i]) - c2 * x[l.a() + i]).collect();
        let mut v1: Vec<f64> = (0..l.n3)
            .map(|i| x[l.v() + i] + dt * ((1.0 - c.gamma) * x[l.a() + i] + c.gamma * a1[i]))
            .collect();
        let mut a1 = a1;
        if self.prescribes_support_rates() {
            let (g, h) = self.prescribed_rates(t);
            for i in (0..l.n3).filter(|i| self.model.fixed[*i]) {
                v1[i] = g[i];
                a1[i] = h[i];
            }
        }
        (v1, a1)
    }

    #[allow(clippy::type_complexity)]
    fn forces(
        &self,
        t: usize,
        x: &[f64],
        u: &[f64],
        p: &[f64],
        extra: Option<&[f64]>,
        jacobian: bool,
    ) -> Result<(Vec<f64>, Vec<f64>, Option<implexity_linalg::sparse::CscMatrix>, [f64; 2]), CaeError> {
        let m = self.model;
        let l = self.layout;
        let dt = self.dt(t);
        let c = self.scheme.coeffs();
        let mut terms = [0.0_f64; 2];
        let (cm, cc) = if c.inertia {
            ((1.0 - c.am) / (c.beta * dt * dt), (1.0 - c.af) * c.gamma / (c.beta * dt))
        } else {
            (0.0, 0.0)
        };
        let (am, bk) = self.rayleigh;
        let wr = 1.0 - c.af;
        let mut r = vec![0.0; l.n3];
        let mut rp = vec![0.0; l.np];
        let mut a = if jacobian { Some(m.zero_matrix()?) } else { None };
        let nl = m.nl();
        m.for_elements(
            |e| {
                let d = m.gather(e, u, p);
                let h = self.history_h(x, e, dt);
                let (_, g) = self.branch_factors(e, dt);
                m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, g, jacobian)
            },
            |e, local| {
                let dofs = m.element_dofs(e);
                for k in 0..12 {
                    r[dofs[k]] += local.gradient[k];
                    terms[0] = terms[0].max(local.gradient[k].abs());
                }
                for k in 12..nl {
                    rp[dofs[k] - l.n3] += local.gradient[k];
                    terms[1] = terms[1].max(local.gradient[k].abs());
                }
                if let Some(a) = a.as_mut() {
                    let mut block = [0.0; 256];
                    for i in 0..nl {
                        for j in 0..nl {
                            block[i * 16 + j] = wr * local.hessian[i * 16 + j];
                        }
                    }
                    if c.inertia {
                        let me = m.element_mass(e, self.ctx.mass[e]);
                        let ke = m.element_reference_stiffness(e);
                        for ai in 0..4 {
                            for bi in 0..4 {
                                for i in 0..3 {
                                    block[(3 * ai + i) * 16 + 3 * bi + i] += (cm + cc * am) * me[ai][bi];
                                    for j in 0..3 {
                                        block[(3 * ai + i) * 16 + 3 * bi + j] += cc
                                            * bk
                                            * self.ctx.stiffness[e]
                                            * ke[(3 * ai + i) * 12 + 3 * bi + j];
                                    }
                                }
                            }
                        }
                    }
                    m.scatter(a, &dofs[..nl], &block, 16);
                }
                Ok(())
            },
        )?;
        let fd = self.dead_load(t, extra);
        for (ri, fi) in r.iter_mut().zip(&fd) {
            *ri -= fi;
        }
        for pot in &m.potentials {
            let f = pot.force(u, &self.params)?;
            if f.len() != l.n3 {
                return contract("nodal potential force must be node-by-XYZ");
            }
            for (ri, fi) in r.iter_mut().zip(&f) {
                *ri += fi;
            }
            if let Some(a) = a.as_mut() {
                m.scatter_triplets(a, &pot.tangent(u, &self.params)?, wr)?;
            }
        }
        let pressure = self.pressure(t);
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                let (res, jac) = m.face_local(f, u, pressure);
                let dofs: [usize; 9] = core::array::from_fn(|k| 3 * face[k / 3] + k % 3);
                for node in face {
                    for i in 0..3 {
                        r[3 * node + i] += res[i];
                    }
                }
                if let Some(a) = a.as_mut() {
                    let mut block = [0.0; 81];
                    for (nk, _) in face.iter().enumerate() {
                        for i in 0..3 {
                            for k in 0..9 {
                                block[(3 * nk + i) * 9 + k] = wr * jac[i][k];
                            }
                        }
                    }
                    m.scatter(a, &dofs, &block, 9);
                }
            }
        }
        Ok((r, rp, a, terms))
    }


    #[allow(clippy::type_complexity)]
    pub fn step_system(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
        jacobian: bool,
    ) -> Result<(Vec<f64>, Vec<f64>, Option<implexity_linalg::sparse::CscMatrix>), CaeError> {
        self.system(t, x, y, extra, jacobian).map(|(res, r, a, _)| (res, r, a))
    }

    #[allow(clippy::type_complexity)]
    fn system(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
        jacobian: bool,
    ) -> Result<(Vec<f64>, Vec<f64>, Option<implexity_linalg::sparse::CscMatrix>, [f64; 2]), CaeError> {
        if let Scheme::AvfMidpoint { gauss_points } = self.scheme {
            return self.avf_system(t, x, y, extra, jacobian, gauss_points);
        }
        let m = self.model;
        let l = self.layout;
        let c = self.scheme.coeffs();
        let (u, p) = self.expand(t, y);
        let (r, rp, a, mut terms) = self.forces(t, x, &u, &p, extra, jacobian)?;
        let mut full: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.af) * r[i] + c.af * x[l.r() + i]).collect();
        terms[0] = terms[0].max(x[l.r()..l.r() + l.n3].iter().fold(0.0, |a, b| a.max(b.abs())));
        if c.inertia {
            let (v1, a1) = self.newmark(t, x, &u);
            let am_mix: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.am) * a1[i] + c.am * x[l.a() + i]).collect();
            let v_mix: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.af) * v1[i] + c.af * x[l.v() + i]).collect();
            let ma = self.ctx.m.matvec(&am_mix).map_err(|e| CaeError::contract(e.to_string()))?;
            let cv = self.damping_matvec(&v_mix)?;
            for i in 0..l.n3 {
                full[i] += ma[i] + cv[i];
                terms[0] = terms[0].max(ma[i].abs()).max(cv[i].abs());
            }
            let dt = self.dt(t);
            let amax = |v: &[f64]| v.iter().fold(0.0_f64, |a, b| a.max(b.abs()));
            let (uk, un, vn) = (amax(&u), amax(&x[..l.n3]), amax(&x[l.v()..l.v() + l.n3]));
            let c1 = 1.0 / (c.beta * dt * dt);
            let mass_row = self.ctx.m.norm_inf();
            let damp_row = self.rayleigh.0 * mass_row + self.rayleigh.1 * self.ctx.kref.norm_inf();
            terms[0] = terms[0].max(c1 * (mass_row + c.gamma * dt * damp_row) * (uk + un + dt * vn));
        }
        let mut res = vec![0.0; m.n_unknowns];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                res[m.unknown[i]] = full[i];
            }
        }
        for a_ in 0..l.np {
            res[m.unknown[l.n3 + a_]] = (1.0 - c.af) * rp[a_];
        }
        Ok((res, r, a, terms))
    }

    fn scales(&self, t: usize, extra: Option<&[f64]>) -> (f64, f64) {
        let m = self.model;
        let mut force = 0.0_f64;
        for (e, v) in m.mesh.volumes.iter().enumerate() {
            force = force.max(m.materials[m.element_material[e]].mu0 * v.powf(2.0 / 3.0));
        }
        let fd = self.dead_load(t, extra);
        force = force.max(fd.iter().fold(0.0, |a, b| a.max(b.abs())));
        let vol = m.mesh.volumes.iter().fold(0.0_f64, |a, b| a.max(*b));
        (force, vol)
    }

    fn check_time_path(&self, previous: &[f64], next: &[f64]) -> Result<(), CaeError> {
        let direction: Vec<f64> = next.iter().zip(previous).map(|(a,b)| a-b).collect();
        let limit = self.model.admissible_step(previous, &direction, &self.params, 1.0)?;
        if limit < 1.0 { return contract("physical step linear surface path is inadmissible"); }
        Ok(())
    }

    fn admitted_newton_step(&self, t: usize, y: &[f64], dy: &[f64], trial: f64) -> Result<f64, CaeError> {
        let (u, _) = self.expand(t, y);
        let end: Vec<f64> = y.iter().zip(dy).map(|(a,b)| a+b).collect();
        let (next, _) = self.expand(t, &end);
        let direction: Vec<f64> = next.iter().zip(&u).map(|(a,b)| a-b).collect();
        self.model.admissible_step(&u, &direction, &self.params, trial)
    }


    pub fn solve_step(
        &self,
        t: usize,
        x: &[f64],
        extra: Option<&[f64]>,
        guess: Option<&[f64]>,
    ) -> Result<StepSolution, CaeError> {
        let m = self.model;
        let l = self.layout;
        if t >= self.loading.steps() || x.len() != l.len() {
            return contract("step index or state length out of range");
        }
        let mut y: Vec<f64> = match guess {
            Some(g) if g.len() == m.n_unknowns => g.to_vec(),
            _ => {
                let mut y = vec![0.0; m.n_unknowns];
                for i in 0..l.n3 {
                    if m.unknown[i] != usize::MAX {
                        y[m.unknown[i]] = x[l.u() + i];
                    }
                }
                for a in 0..l.np {
                    y[m.unknown[l.n3 + a]] = x[l.p() + a];
                }
                y
            }
        };
        self.check_time_path(&x[l.u()..l.u()+l.n3], &self.expand(t, &y).0)?;
        let (fs, vs) = self.scales(t, extra);
        let tol = self.newton.relative_tolerance;
        let nu = m.n_free_u;
        let norms = |res: &[f64]| -> (f64, f64, f64) {
            let fu = res[..nu].iter().fold(0.0_f64, |a, b| a.max(b.abs()));
            let fp = res[nu..].iter().fold(0.0_f64, |a, b| a.max(b.abs()));
            let merit = res[..nu].iter().map(|v| (v / fs).powi(2)).sum::<f64>()
                + res[nu..].iter().map(|v| (v / vs).powi(2)).sum::<f64>();
            (fu, fp, 0.5 * merit)
        };
        let follower = match self.scheme {
            Scheme::AvfMidpoint { .. } => self.avf_pressure(t),
            _ => self.pressure(t),
        };
        let conservative = !m.formulation.mixed() && (m.faces.is_empty() || follower == 0.0);

        let floor = |terms: [f64; 2]| [1e4 * f64::EPSILON * terms[0], 1e4 * f64::EPSILON * terms[1]];
        let (mut res, _, _, terms) = self.system(t, x, &y, extra, false)?;
        let mut fl = floor(terms);
        let (mut fu, mut fp, mut merit) = norms(&res);
        if !merit.is_finite() {
            return crate::util::convergence(
                "nonfinite finite-deformation residual at the step start (inverted elements)",
            );
        }
        let mut iterations = 0;
        let mut factorizations = 0;
        let mut polished = false;

        let mut kept: Option<(implexity_linalg::lu::SparseLu, usize)> = None;
        loop {
            let at_tolerance = fu <= tol * fs && fp <= tol * vs;
            let at_floor = fu <= (tol * fs).max(fl[0]) && fp <= (tol * vs).max(fl[1]);
            if at_tolerance || (at_floor && polished) {
                break;
            }
            if iterations == self.newton.max_iterations {
                if at_floor {
                    break;
                }
                return crate::util::convergence(format!(
                    "finite-deformation Newton iteration limit reached at step {t} (free residual {fu:.3e} N)"
                ));
            }
            let rhs: Vec<f64> = res.iter().map(|v| -v).collect();
            if let Some(reuse) = self.reuse
                && !at_floor
                && let Some((lu, used)) = kept.as_mut()
                && *used < reuse.max_reuse
            {


                *used += 1;
                let dy = lu
                    .solve(&rhs)
                    .map_err(|e| CaeError::convergence(format!("finite-deformation step solve: {e}")))?;
                let alpha = self.admitted_newton_step(t, &y, &dy, 1.0)?;
                let trial: Vec<f64> = y.iter().zip(&dy).map(|(a, b)| a + alpha*b).collect();
                if let Ok((tres, _, _, tterms)) = self.system(t, x, &trial, extra, false) {
                    let (tu, tp, tm) = norms(&tres);
                    if tm.is_finite() && tm <= reuse.contraction * reuse.contraction * merit {
                        y = trial;
                        res = tres;
                        fl = floor(tterms);
                        (fu, fp, merit) = (tu, tp, tm);
                        iterations += 1;
                        continue;
                    }
                }
                kept = None;
            }
            let (_, _, a) = self.step_system(t, x, &y, extra, true)?;
            let a = a.ok_or_else(|| CaeError::contract("internal: missing Jacobian"))?;
            let lu = m.factor(&a)?;
            factorizations += 1;
            let dy = lu
                .solve(&rhs)
                .map_err(|e| CaeError::convergence(format!("finite-deformation step solve: {e}")))?;
            if self.reuse.is_some() {
                kept = Some((lu, 0));
            }
            if at_floor {

                polished = true;
                let alpha = self.admitted_newton_step(t, &y, &dy, 1.0)?;
                let trial: Vec<f64> = y.iter().zip(&dy).map(|(a, b)| a + alpha*b).collect();
                if let Ok((tres, _, _, tterms)) = self.system(t, x, &trial, extra, false) {
                    let (tu, tp, tm) = norms(&tres);
                    if tm.is_finite() && tm < merit {
                        y = trial;
                        res = tres;
                        fl = floor(tterms);
                        (fu, fp, merit) = (tu, tp, tm);
                        iterations += 1;
                    }
                }
                continue;
            }
            let mut accepted = false;
            let mut alpha = self.admitted_newton_step(t, &y, &dy, 1.0)?;

            let slope: f64 = res.iter().zip(&dy).map(|(r, d)| r * d).sum();

            let mut phi0: Option<Option<f64>> = None;
            for _ in 0..40 {
                let trial: Vec<f64> = y.iter().zip(&dy).map(|(a, b)| a + alpha * b).collect();
                if let Ok((tres, _, _, tterms)) = self.system(t, x, &trial, extra, false) {
                    let (tu, tp, tm) = norms(&tres);
                    let tfl = floor(tterms);
                    let converged = tu <= (tol * fs).max(tfl[0]) && tp <= (tol * vs).max(tfl[1]);

                    let ok = tm.is_finite()
                        && (converged || tm <= (1.0 - 1e-4 * alpha) * merit || {
                            if phi0.is_none() {
                                phi0 = Some(if conservative && slope < 0.0 {
                                    let (p0, size) = self.incremental_potential(t, x, &y, extra)?;
                                    (slope.abs() > 1e3 * f64::EPSILON * size).then_some(p0)
                                } else {
                                    None
                                });
                            }
                            match phi0.flatten() {
                                Some(p0) => {
                                    let p1 = self.incremental_potential(t, x, &trial, extra)?.0;
                                    p1.is_finite() && p1 <= p0 + 1e-4 * alpha * slope
                                }
                                None => false,
                            }
                        });
                    if ok {
                        y = trial;
                        res = tres;
                        fl = tfl;
                        (fu, fp, merit) = (tu, tp, tm);
                        accepted = true;
                        break;
                    }
                }
                alpha *= 0.5;
            }
            if !accepted {
                return crate::util::convergence(format!(
                    "finite-deformation line search failed at step {t}, iteration {iterations} (free residual {fu:.3e} N)"
                ));
            }
            iterations += 1;
        }
        let state = self.assemble_state(t, x, &y, extra)?;
        Ok(StepSolution { state, iterations, free_residual: fu, unknowns: y, factorizations })
    }




    pub fn incremental_potential(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<(f64, f64), CaeError> {
        if let Scheme::AvfMidpoint { gauss_points } = self.scheme {
            return self.avf_potential(t, x, y, extra, gauss_points);
        }
        let m = self.model;
        let l = self.layout;
        let c = self.scheme.coeffs();
        let dt = self.dt(t);
        let (u, p) = self.expand(t, y);
        let rates = self.prescribes_support_rates().then(|| self.prescribed_rates(t));
        let mut e_int = 0.0;
        let mut size = 0.0;
        m.for_elements(
            |e| {
                let d = m.gather(e, &u, &p);
                let h = self.history_h(x, e, dt);
                let (_, g) = self.branch_factors(e, dt);
                m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, g, false).map(|r| r.energy)
            },
            |e, v| {
                e_int += v;

                size += v.abs() + 4.0 * m.materials[m.element_material[e]].mu0 * m.mesh.volumes[e];
                Ok(())
            },
        )?;
        for pot in &m.potentials {
            let v = pot.energy(&u, &self.params)?;
            e_int += v;
            size += v.abs();
        }
        let fd = self.dead_load(t, extra);
        let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f64>();
        let absdot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(p, q)| (p * q).abs()).sum::<f64>();
        size += absdot(&fd, &u) + absdot(&x[l.r()..l.r() + l.n3], &u);
        let mut phi = (1.0 - c.af) * (e_int - dot(&fd, &u)) + c.af * dot(&x[l.r()..l.r() + l.n3], &u);
        if c.inertia {
            let c1 = 1.0 / (c.beta * dt * dt);
            let c2 = 1.0 / (2.0 * c.beta) - 1.0;
            let (un, vn, an) = (&x[l.u()..l.u() + l.n3], &x[l.v()..l.v() + l.n3], &x[l.a()..l.a() + l.n3]);

            let mut a_free: Vec<f64> = (0..l.n3).map(|i| -c1 * (un[i] + dt * vn[i]) - c2 * an[i]).collect();
            let mut v_free: Vec<f64> =
                (0..l.n3).map(|i| vn[i] + dt * (1.0 - c.gamma) * an[i] + dt * c.gamma * a_free[i]).collect();
            if let Some((g, h)) = &rates {
                for i in (0..l.n3).filter(|i| m.fixed[*i]) {
                    a_free[i] = h[i] - c1 * u[i];
                    v_free[i] = g[i] - dt * c.gamma * c1 * u[i];
                }
            }
            let b_a: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.am) * a_free[i] + c.am * an[i]).collect();
            let b_v: Vec<f64> = (0..l.n3).map(|i| (1.0 - c.af) * v_free[i] + c.af * vn[i]).collect();
            let mu = self.ctx.m.matvec(&u).map_err(|e| CaeError::contract(e.to_string()))?;
            let cu = self.damping_matvec(&u)?;
            let mba = self.ctx.m.matvec(&b_a).map_err(|e| CaeError::contract(e.to_string()))?;
            let cbv = self.damping_matvec(&b_v)?;
            phi += 0.5 * (1.0 - c.am) * c1 * dot(&u, &mu) + dot(&mba, &u);
            phi += 0.5 * (1.0 - c.af) * dt * c.gamma * c1 * dot(&u, &cu) + dot(&cbv, &u);
            size += (0.5 * (1.0 - c.am) * c1 * absdot(&u, &mu) + absdot(&mba, &u)).abs()
                + (0.5 * (1.0 - c.af) * dt * c.gamma * c1 * absdot(&u, &cu) + absdot(&cbv, &u)).abs();
        }
        Ok((phi, size))
    }


    pub fn state_from_unknowns(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<Vec<f64>, CaeError> {
        if t >= self.loading.steps() || x.len() != self.layout.len() || y.len() != self.model.n_unknowns {
            return contract("step index, state or unknown length out of range");
        }
        self.assemble_state(t, x, y, extra)
    }

    fn assemble_state(
        &self,
        t: usize,
        x: &[f64],
        y: &[f64],
        extra: Option<&[f64]>,
    ) -> Result<Vec<f64>, CaeError> {
        let m = self.model;
        let l = self.layout;
        let (u, p) = self.expand(t, y);
        self.check_time_path(&x[l.u()..l.u()+l.n3], &u)?;
        let (_, r, _) = self.step_system(t, x, y, extra, false)?;
        let (v1, a1) = if matches!(self.scheme, Scheme::AvfMidpoint { .. }) {
            self.avf_kinematics(t, x, &u)
        } else {
            self.newmark(t, x, &u)
        };
        let mut out = vec![0.0; l.len()];
        out[..l.n3].copy_from_slice(&u);
        out[l.v()..l.v() + l.n3].copy_from_slice(&v1);
        out[l.a()..l.a() + l.n3].copy_from_slice(&a1);
        out[l.r()..l.r() + l.n3].copy_from_slice(&r);
        out[l.p()..l.p() + l.np].copy_from_slice(&p);
        if l.ne_visc > 0 {
            let dt = self.dt(t);
            for e in 0..m.ne() {
                let (factors, _) = self.branch_factors(e, dt);
                if factors.is_empty() {
                    continue;
                }
                let d = m.gather(e, &u, &p);
                let ul: [[f64; 3]; 4] = core::array::from_fn(|a| core::array::from_fn(|i| d[3 * a + i]));
                let s1 = m.isochoric_stress(e, &ul, self.ctx.rho[e], self.ctx.theta[e]);
                out[l.s() + 6 * e..l.s() + 6 * e + 6].copy_from_slice(&s1);
                let s0 = &x[l.s() + 6 * e..l.s() + 6 * e + 6];
                for (i, (ei, bi)) in factors.iter().enumerate() {
                    let off = 6 * (e * l.nb + i);
                    for k in 0..6 {
                        out[l.q() + off + k] = ei * x[l.q() + off + k] + bi * (s1[k] - s0[k]);
                    }
                }
            }
        }
        Ok(out)
    }

    fn cached(&self, t: usize) -> Option<Vec<f64>> {
        self.cache.lock().ok().and_then(|c| c.0.get(&t).cloned())
    }

    fn remember(&self, t: usize, y: &[f64]) {
        if let Ok(mut c) = self.cache.lock() {
            if c.0.insert(t, y.to_vec()).is_none() {
                c.1.push_back(t);
            }
            while c.1.len() > self.cache_capacity {
                if let Some(old) = c.1.pop_front() {
                    c.0.remove(&old);
                }
            }
        }
    }

    fn forget(&self, t: usize) {
        if let Ok(mut c) = self.cache.lock() {
            c.0.remove(&t);
            c.1.retain(|k| *k != t);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn viscous_output_vjp(
        &self,
        dt: f64,
        u1: &[f64],
        p1: &[f64],
        w: &[f64],
        wx: &mut [f64],
        wu1: &mut [f64],
        wparams: &mut [f64],
    ) -> Result<(), CaeError> {
        let m = self.model;
        let l = self.layout;
        let ne = m.ne();
        let mut ws1 = vec![0.0; 6 * l.ne_visc];
        if l.ne_visc > 0 {
            for e in 0..ne {
                let (factors, _) = self.branch_factors(e, dt);
                for k in 0..6 {
                    ws1[6 * e + k] = w[l.s() + 6 * e + k];
                }
                for (i, (ei, bi)) in factors.iter().enumerate() {
                    let off = 6 * (e * l.nb + i);
                    for k in 0..6 {
                        let wq = w[l.q() + off + k];
                        wx[l.q() + off + k] += ei * wq;
                        wx[l.s() + 6 * e + k] -= bi * wq;
                        ws1[6 * e + k] += bi * wq;
                    }
                }
            }
        }

        if l.ne_visc > 0 {
            let contributions: Vec<(usize, [f64; 14])> = {
                let mut out = Vec::new();
                m.for_elements(
                    |e| {
                        let ws = &ws1[6 * e..6 * e + 6];
                        if ws.iter().all(|v| *v == 0.0) || self.branch_factors(e, dt).0.is_empty() {
                            return Ok(None);
                        }
                        let wd = [ws[0], ws[1], ws[2], 0.5 * ws[3], 0.5 * ws[4], 0.5 * ws[5]];
                        let d = m.gather(e, u1, p1);
                        let mut g = [0.0; 14];
                        for (k, slot) in g.iter_mut().enumerate() {
                            let seed = |i: usize| if i == k { 1.0 } else { 0.0 };
                            let ul: [[HyperDual; 3]; 4] = core::array::from_fn(|a| {
                                core::array::from_fn(|i| {
                                    HyperDual::new(d[3 * a + i], 0.0, seed(3 * a + i), 0.0)
                                })
                            });
                            let rho = HyperDual::new(self.ctx.rho[e], 0.0, seed(12), 0.0);
                            let th = HyperDual::new(self.ctx.theta[e], 0.0, seed(13), 0.0);
                            let tt = HyperDual::new(0.0, 1.0, 0.0, 0.0);
                            *slot = m.isochoric_stress_dot(e, &ul, rho, th, &wd, tt).e12;
                        }
                        Ok(Some(g))
                    },
                    |e, g| {
                        if let Some(g) = g {
                            out.push((e, g));
                        }
                        Ok(())
                    },
                )?;
                out
            };
            for (e, g) in contributions {
                let dofs = m.element_dofs(e);
                for k in 0..12 {
                    wu1[dofs[k]] += g[k];
                }
                wparams[e] += g[12];
                wparams[ne + e] += g[13];
            }
        }
        Ok(())
    }


    #[allow(clippy::too_many_lines)]
    pub fn step_vjp(
        &self,
        t: usize,
        x: &[f64],
        next: &StepSolution,
        extra: Option<&[f64]>,
        w: &[f64],
    ) -> Result<StepCotangent, CaeError> {
        if let Scheme::AvfMidpoint { gauss_points } = self.scheme {
            return self.avf_step_vjp(t, x, next, extra, w, gauss_points);
        }
        let m = self.model;
        let l = self.layout;
        let ne = m.ne();
        let c = self.scheme.coeffs();
        let dt = self.dt(t);
        let x1 = &next.state;
        let (u1, p1) = (&x1[..l.n3], &x1[l.p()..l.p() + l.np]);
        let mut wx = vec![0.0; l.len()];
        let mut wparams = vec![0.0; 2 * ne];
        let mut wu1 = w[..l.n3].to_vec();
        let mut wp1 = w[l.p()..l.p() + l.np].to_vec();
        let wr = &w[l.r()..l.r() + l.n3];
        self.viscous_output_vjp(dt, u1, p1, w, &mut wx, &mut wu1, &mut wparams)?;

        let prescribed = |i: usize| m.fixed[i] && self.prescribes_support_rates();
        if c.inertia {
            let c1 = 1.0 / (c.beta * dt * dt);
            let c2 = 1.0 / (2.0 * c.beta) - 1.0;
            for i in (0..l.n3).filter(|i| !prescribed(*i)) {
                let wv = w[l.v() + i];
                wx[l.v() + i] += wv;
                wx[l.a() + i] += dt * (1.0 - c.gamma) * wv;
                let wa = w[l.a() + i] + dt * c.gamma * wv;
                wu1[i] += c1 * wa;
                wx[l.u() + i] -= c1 * wa;
                wx[l.v() + i] -= c1 * dt * wa;
                wx[l.a() + i] -= c2 * wa;
            }
        }

        let mut pr_u = vec![0.0; l.n3];
        let mut pr_p = vec![0.0; l.np];
        let pressure = self.pressure(t);
        m.for_elements(
            |e| {
                let d = m.gather(e, u1, p1);
                let h = self.history_h(x, e, dt);
                let (_, g) = self.branch_factors(e, dt);
                m.element_local(e, &d, self.ctx.rho[e], self.ctx.theta[e], &h, g, true)
            },
            |e, local| {
                let dofs = m.element_dofs(e);
                let nl = m.nl();
                for a in 0..nl {
                    let mut s = 0.0;
                    for b in 0..12 {
                        s += local.hessian[b * 16 + a] * wr[dofs[b]];
                    }
                    if a < 12 {
                        pr_u[dofs[a]] += s;
                    } else {
                        pr_p[dofs[a] - l.n3] += s;
                    }
                }
                Ok(())
            },
        )?;
        if pressure != 0.0 {
            for (f, face) in m.faces.iter().enumerate() {
                let (_, jac) = m.face_local(f, u1, pressure);
                for k in 0..9 {
                    let mut s = 0.0;
                    for node in face {
                        for i in 0..3 {
                            s += jac[i][k] * wr[3 * node + i];
                        }
                    }
                    pr_u[3 * face[k / 3] + k % 3] += s;
                }
            }
        }
        for pot in &m.potentials {
            for (r, c, v) in pot.tangent(u1, &self.params)? {
                pr_u[c] += v * wr[r];
            }
        }
        for i in 0..l.n3 {
            wu1[i] += pr_u[i];
        }
        for a in 0..l.np {
            wp1[a] += pr_p[a];
        }

        let mut rhs = vec![0.0; m.n_unknowns];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                rhs[m.unknown[i]] = wu1[i];
            }
        }
        for a in 0..l.np {
            rhs[m.unknown[l.n3 + a]] = wp1[a];
        }
        let (_, _, jac) = self.step_system(t, x, &next.unknowns, extra, true)?;
        let jac = jac.ok_or_else(|| CaeError::contract("internal: missing Jacobian"))?;
        let lu = m.factor(&jac)?;
        let mu = lu
            .solve_transpose(&rhs)
            .map_err(|e| CaeError::convergence(format!("adjoint step solve: {e}")))?;
        let mut mu_u = vec![0.0; l.n3];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                mu_u[i] = mu[m.unknown[i]];
            }
        }
        let mu_p: Vec<f64> = (0..l.np).map(|a| mu[m.unknown[l.n3 + a]]).collect();

        for i in 0..l.n3 {
            wx[l.r() + i] -= c.af * mu_u[i];
        }
        let mut z_a = vec![0.0; l.n3];
        let mut z_v = vec![0.0; l.n3];
        let mut mt_a = vec![0.0; l.n3];
        let mut ct_v = vec![0.0; l.n3];
        if c.inertia {
            let (am, bk) = self.rayleigh;
            let mmu = self.ctx.m.matvec(&mu_u).map_err(|e| CaeError::contract(e.to_string()))?;
            let cmu = self.damping_matvec(&mu_u)?;
            let c1 = 1.0 / (c.beta * dt * dt);
            let c2 = 1.0 / (2.0 * c.beta) - 1.0;
            for i in 0..l.n3 {

                if prescribed(i) {
                    wx[l.v() + i] -= c.af * cmu[i];
                    wx[l.a() + i] -= c.am * mmu[i];
                    continue;
                }
                let zv = (1.0 - c.af) * cmu[i];
                wx[l.v() + i] -= zv + c.af * cmu[i];
                wx[l.a() + i] -= dt * (1.0 - c.gamma) * zv + c.am * mmu[i];
                let za = (1.0 - c.am) * mmu[i] + dt * c.gamma * zv;
                wx[l.u() + i] += c1 * za;
                wx[l.v() + i] += c1 * dt * za;
                wx[l.a() + i] += c2 * za;
                z_a[i] = za;
                z_v[i] = zv;
            }
            for i in 0..l.n3 {
                mt_a[i] = (1.0 - c.am) * x1[l.a() + i] + c.am * x[l.a() + i];
                ct_v[i] = (1.0 - c.af) * x1[l.v() + i] + c.af * x[l.v() + i];
            }
            for e in 0..ne {
                let t_ = m.mesh.elements[e];
                let me0 = m.element_mass(e, 1.0);
                let ke = m.element_reference_stiffness(e);
                let dm = HyperDual::new(self.ctx.rho[e], 1.0, 0.0, 0.0);
                let dmass = m.interpolation.mass(dm).e1;
                let dstiff = m.interpolation.stiffness(dm).e1;
                let (mut sm_a, mut sm_v, mut sk_v) = (0.0, 0.0, 0.0);
                for a in 0..4 {
                    for b in 0..4 {
                        for i in 0..3 {
                            let mu_ai = mu_u[3 * t_[a] + i];
                            sm_a += mu_ai * me0[a][b] * mt_a[3 * t_[b] + i];
                            sm_v += mu_ai * me0[a][b] * ct_v[3 * t_[b] + i];
                            for j in 0..3 {
                                sk_v += mu_ai * ke[(3 * a + i) * 12 + 3 * b + j] * ct_v[3 * t_[b] + j];
                            }
                        }
                    }
                }
                wparams[e] -= dmass * (sm_a + am * sm_v) + dstiff * bk * sk_v;
            }
        }
        let _ = (&z_a, &z_v);
        let omega_scale = 1.0 - c.af;
        let parts: Vec<(usize, [f64; 8])> = {
            let mut out = Vec::new();
            m.for_elements(
                |e| {
                    let dofs = m.element_dofs(e);
                    let nl = m.nl();
                    let omega: [f64; 16] = core::array::from_fn(|k| {
                        if k >= nl {
                            0.0
                        } else if k < 12 {
                            wr[dofs[k]] - omega_scale * mu_u[dofs[k]]
                        } else {
                            -omega_scale * mu_p[dofs[k] - l.n3]
                        }
                    });
                    if omega.iter().all(|v| *v == 0.0) {
                        return Ok(None);
                    }
                    let d = m.gather(e, u1, p1);
                    let h = self.history_h(x, e, dt);
                    let (_, g) = self.branch_factors(e, dt);
                    let with_h = l.ne_visc > 0;
                    let mut out = [0.0; 8];
                    let seeds = if with_h { 8 } else { 2 };
                    for (s, slot) in out.iter_mut().enumerate().take(seeds) {
                        let dd: [HyperDual; 16] =
                            core::array::from_fn(|k| HyperDual::new(d[k], omega[k], 0.0, 0.0));
                        let rho = HyperDual::new(self.ctx.rho[e], 0.0, if s == 0 { 1.0 } else { 0.0 }, 0.0);
                        let th = HyperDual::new(self.ctx.theta[e], 0.0, if s == 1 { 1.0 } else { 0.0 }, 0.0);
                        let hh: [HyperDual; 6] = core::array::from_fn(|k| {
                            HyperDual::new(h[k], 0.0, if s == 2 + k { 1.0 } else { 0.0 }, 0.0)
                        });
                        *slot = m.element_energy(e, &dd, rho, th, &hh, g)?.e12;
                    }
                    Ok(Some(out))
                },
                |e, v| {
                    if let Some(v) = v {
                        out.push((e, v));
                    }
                    Ok(())
                },
            )?;
            out
        };
        for (e, v) in parts {
            wparams[e] += v[0];
            wparams[ne + e] += v[1];
            if l.ne_visc > 0 {
                let (factors, _) = self.branch_factors(e, dt);
                for (i, (ei, bi)) in factors.iter().enumerate() {
                    let off = 6 * (e * l.nb + i);
                    for k in 0..6 {
                        wx[l.q() + off + k] += ei * v[2 + k];
                        wx[l.s() + 6 * e + k] -= bi * v[2 + k];
                    }
                }
            }
        }
        if !m.potentials.is_empty() {
            let omega_u: Vec<f64> = (0..l.n3).map(|i| wr[i] - omega_scale * mu_u[i]).collect();
            for pot in &m.potentials {
                let g = pot.force_params_vjp(u1, &self.params, &omega_u)?;
                if g.len() != 2 * ne {
                    return contract("nodal potential parameter cotangent must hold 2 values per element");
                }
                for (a, b) in wparams.iter_mut().zip(&g) {
                    *a += b;
                }
            }
        }

        let extra_load: Vec<f64> = (0..l.n3).map(|i| -wr[i] + omega_scale * mu_u[i]).collect();
        Ok(StepCotangent { state: wx, params: wparams, extra_load })
    }


    pub fn run(&self) -> Result<Vec<StepSolution>, CaeError> {
        let mut x = self.initial_state()?;
        let mut out = Vec::with_capacity(self.loading.steps());
        for t in 0..self.loading.steps() {
            let s = self.solve_step(t, &x, None, self.cached(t).as_deref())?;
            self.remember(t, &s.unknowns);
            if let Ok(mut it) = self.iterations.lock() {
                it.push((t, s.iterations));
            }
            x.clone_from(&s.state);
            out.push(s);
        }
        Ok(out)
    }

    #[must_use]
    pub fn design(&self) -> (&[f64], &[f64]) {
        (&self.ctx.rho, &self.ctx.theta)
    }

    #[must_use]
    pub fn mass_matrix(&self) -> &CsrMatrix {
        &self.ctx.m
    }


    pub fn damping(&self, v: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.damping_matvec(v)
    }

    #[must_use]
    pub fn external_force(&self, t: usize, u: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let m = self.model;
        let mut f = self.dead_load(t, None);
        let mut jt_u = vec![0.0; u.len()];
        let pressure = self.pressure(t);
        if pressure != 0.0 {
            for (fi, face) in m.faces.iter().enumerate() {
                let (res, jac) = m.face_local(fi, u, pressure);
                for node in face {
                    for i in 0..3 {
                        f[3 * node + i] -= res[i];
                    }
                }
                for k in 0..9 {
                    let mut s = 0.0;
                    for node in face {
                        for i in 0..3 {
                            s -= jac[i][k] * u[3 * node + i];
                        }
                    }
                    jt_u[3 * face[k / 3] + k % 3] += s;
                }
            }
        }
        (f, jt_u)
    }
}

impl ScanStep for SoftHistory<'_> {
    fn step(&self, t: usize, state: &[f64], params: &[f64]) -> Result<Vec<f64>, AdError> {
        if params != self.params.as_slice() {
            return Err(AdError::Invalid(
                "the soft history was built for different design parameters".into(),
            ));
        }
        let s = self.solve_step(t, state, None, self.cached(t).as_deref()).map_err(|e| ad(&e))?;
        self.remember(t, &s.unknowns);
        Ok(s.state)
    }

    fn vjp(
        &self,
        t: usize,
        state: &[f64],
        params: &[f64],
        cotangent: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), AdError> {
        if params != self.params.as_slice() {
            return Err(AdError::Invalid(
                "the soft history was built for different design parameters".into(),
            ));
        }
        let next = self.solve_step(t, state, None, self.cached(t).as_deref()).map_err(|e| ad(&e))?;
        let cot = self.step_vjp(t, state, &next, None, cotangent).map_err(|e| ad(&e))?;
        self.forget(t);
        Ok((cot.state, cot.params))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Measure {
    StrainEnergy,
    KineticEnergy,
    Compliance,
    DisplacementSquared,
    Tracking {
        targets: Vec<(usize, f64, f64)>,
        amplitude: Vec<f64>,
    },
}

impl Measure {

    pub fn value(&self, h: &SoftHistory<'_>, t: usize, x: &[f64]) -> Result<f64, CaeError> {
        if *self != Self::StrainEnergy {
            return self.eval(h, t, x).map(|g| g.value);
        }
        let m = h.model;
        let l = h.layout;
        let (u, p) = (&x[..l.n3], &x[l.p()..l.p() + l.np]);
        let mut total = 0.0;
        m.for_elements(
            |e| m.element_energy::<f64>(e, &m.gather(e, u, p), h.ctx.rho[e], h.ctx.theta[e], &[0.0; 6], 1.0),
            |_, v| {
                total += v;
                Ok(())
            },
        )?;
        Ok(total)
    }


    pub fn eval(&self, h: &SoftHistory<'_>, t: usize, x: &[f64]) -> Result<TermGradient, CaeError> {
        let m = h.model;
        let l = h.layout;
        let ne = m.ne();
        let mut d_state = vec![0.0; x.len()];
        let mut d_params = vec![0.0; 2 * ne];
        let u = &x[..l.n3];
        let value = match self {
            Self::StrainEnergy => {
                let p = &x[l.p()..l.p() + l.np];
                let mut total = 0.0;
                let zero = [0.0; 6];
                m.for_elements(
                    |e| {
                        let d = m.gather(e, u, p);
                        let local =
                            m.element_local(e, &d, h.ctx.rho[e], h.ctx.theta[e], &zero, 1.0, false)?;
                        let mut dp = [0.0; 2];
                        for (s, slot) in dp.iter_mut().enumerate() {
                            let dd: [Dual<1>; 16] = core::array::from_fn(|k| Dual::constant(d[k]));
                            let rho = if s == 0 {
                                Dual::variable(h.ctx.rho[e], 0)
                            } else {
                                Dual::constant(h.ctx.rho[e])
                            };
                            let th = if s == 1 {
                                Dual::variable(h.ctx.theta[e], 0)
                            } else {
                                Dual::constant(h.ctx.theta[e])
                            };
                            *slot = m.element_energy(e, &dd, rho, th, &[Dual::constant(0.0); 6], 1.0)?.eps[0];
                        }
                        Ok((local, dp))
                    },
                    |e, (local, dp)| {
                        total += local.energy;
                        let dofs = m.element_dofs(e);
                        for k in 0..m.nl() {
                            let idx = if k < 12 { dofs[k] } else { l.p() + dofs[k] - l.n3 };
                            d_state[idx] += local.gradient[k];
                        }
                        d_params[e] += dp[0];
                        d_params[ne + e] += dp[1];
                        Ok(())
                    },
                )?;
                total
            }
            Self::KineticEnergy => {
                let v = &x[l.v()..l.v() + l.n3];
                let mv = h.ctx.m.matvec(v).map_err(|e| CaeError::contract(e.to_string()))?;
                d_state[l.v()..l.v() + l.n3].copy_from_slice(&mv);
                for e in 0..ne {
                    let t_ = m.mesh.elements[e];
                    let me0 = m.element_mass(e, 1.0);
                    let dmass = m.interpolation.mass(HyperDual::new(h.ctx.rho[e], 1.0, 0.0, 0.0)).e1;
                    let mut s = 0.0;
                    for a in 0..4 {
                        for b in 0..4 {
                            for i in 0..3 {
                                s += v[3 * t_[a] + i] * me0[a][b] * v[3 * t_[b] + i];
                            }
                        }
                    }
                    d_params[e] += 0.5 * dmass * s;
                }
                0.5 * v.iter().zip(&mv).map(|(a, b)| a * b).sum::<f64>()
            }
            Self::Compliance => {
                let (f, jt_u) = h.external_force(t, u);
                for i in 0..l.n3 {
                    d_state[i] = f[i] + jt_u[i];
                }
                f.iter().zip(u).map(|(a, b)| a * b).sum()
            }
            Self::DisplacementSquared => {
                for i in 0..l.n3 {
                    d_state[i] = 2.0 * u[i];
                }
                u.iter().map(|v| v * v).sum()
            }
            Self::Tracking { targets, amplitude } => {
                let amp = amplitude.get(t).copied().unwrap_or(1.0);
                let mut s = 0.0;
                for (k, target, weight) in targets {
                    let diff = u[*k] - target * amp;
                    s += weight * diff * diff;
                    d_state[*k] += 2.0 * weight * diff;
                }
                s
            }
        };
        Ok(TermGradient { value, d_state, d_params })
    }
}

pub struct Objective<'h, 'm> {
    pub history: &'h SoftHistory<'m>,
    pub measure: Measure,
    pub weights: Option<Vec<f64>>,
}

impl ScanObjective for Objective<'_, '_> {
    fn stage(&self, t: usize, state: &[f64], _params: &[f64]) -> Result<Option<TermGradient>, AdError> {
        let Some(w) = &self.weights else { return Ok(None) };
        let total: f64 = w.iter().sum();
        let wt = w[t] / total;
        if wt == 0.0 {
            return Ok(None);
        }
        let mut g = self.measure.eval(self.history, t, state).map_err(|e| ad(&e))?;
        g.value *= wt;
        g.d_state.iter_mut().for_each(|v| *v *= wt);
        g.d_params.iter_mut().for_each(|v| *v *= wt);
        Ok(Some(g))
    }

    fn terminal(&self, state: &[f64], params: &[f64]) -> Result<TermGradient, AdError> {
        if self.weights.is_some() {
            return Ok(TermGradient {
                value: 0.0,
                d_state: vec![0.0; state.len()],
                d_params: vec![0.0; params.len()],
            });
        }
        let t = self.history.loading.steps() - 1;
        self.measure.eval(self.history, t, state).map_err(|e| ad(&e))
    }
}
