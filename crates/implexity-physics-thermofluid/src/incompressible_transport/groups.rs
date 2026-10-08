// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_ad::Scalar;
use implexity_solve::local_assembly::LocalResidual;

use super::material::FluidLaw;
use crate::local_group::StepKernel;

#[derive(Debug, Clone)]
pub struct MacParams {
    pub law: FluidLaw,
    pub rho: f64,
    pub us: f64,
    pub ps: f64,
    pub ts: f64,
    pub fs: f64,
    pub ms: f64,
    pub hs: f64,
    pub t0: f64,
    pub pref: f64,
    pub brinkman_max: f64,
    pub exponent: f64,
    pub body: Option<([f64; 3], f64, f64)>,
    pub times: Vec<f64>,
    pub heat: Vec<f64>,
    pub turbulence: Option<MixingLength>,
    pub advection_smoothing: Option<f64>,
}


#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixingLength {
    pub length_m: f64,
    pub rate_floor_s: f64,
}

impl MixingLength {
    pub fn eddy_viscosity<S: Scalar>(&self, rho: f64, phi: S, rate_squared: S) -> S {
        let l = phi * self.length_m;
        l * l * rho * (rate_squared + self.rate_floor_s * self.rate_floor_s).sqrt()
    }
}

impl MacParams {
    pub fn alpha<S: Scalar>(&self, theta: S) -> S {
        theta.powf(self.exponent) * self.brinkman_max
    }
}

fn prod3<S: Scalar>(h: &[S]) -> S {
    h[0] * h[1] * h[2]
}

fn spacing<S: Scalar>(x: &[S]) -> [S; 3] {
    [x[0] * 1e-3, x[1] * 1e-3, x[2] * 1e-3]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellPart {
    Standalone,
    Mechanics,
    Caloric,
    Dissipation,
}

pub struct CellKernel {
    pub p: Arc<MacParams>,
    pub part: CellPart,
    pub count: usize,
}

impl LocalResidual for CellKernel {
    fn residual<S: Scalar>(&self, _item: usize, z: &[S], old: &[S], x: &[S], out: &mut [S]) {
        let p = &self.p;
        let theta = x[0];
        let h = spacing(&x[1..4]);
        let v = prod3(&h);
        let area = [v / h[0], v / h[1], v / h[2]];
        let dt = z[8];
        let source = z[9];
        let u: [[S; 2]; 3] = std::array::from_fn(|a| [z[2 * a] * p.us, z[2 * a + 1] * p.us]);
        let uo: [[S; 2]; 3] = std::array::from_fn(|a| [old[2 * a] * p.us, old[2 * a + 1] * p.us]);
        let pg = z[6] * p.ps;
        let t = z[7] * p.ts + p.t0;
        let to = old[7] * p.ts + p.t0;
        let (mu, _k, _cp) = p.law.properties(t);
        let d: [S; 3] = std::array::from_fn(|a| (u[a][1] - u[a][0]) / h[a]);
        let mu = match &p.turbulence {
            None => mu,
            Some(ml) => {
                mu + ml.eddy_viscosity(p.rho, -theta + 1.0, (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]) * 2.0)
            }
        };
        let alpha = p.alpha(theta);
        match self.part {
            CellPart::Standalone | CellPart::Mechanics => {
                let mut mom = [[S::zero(); 2]; 3];
                for a in 0..3 {
                    for s in 0..2 {
                        mom[a][s] = v * p.rho * 0.5 * (u[a][s] - uo[a][s]) / dt + alpha * v * 0.5 * u[a][s];
                    }
                }
                if let Some((g, beta, tref)) = p.body {
                    let factor = -(t - tref) * beta + 1.0;
                    for (a, ga) in g.iter().enumerate() {
                        for s in 0..2 {
                            mom[a][s] -= v * 0.5 * p.rho * factor * *ga;
                        }
                    }
                }
                for a in 0..3 {
                    let normal = (mu * 2.0 * d[a] - pg) * area[a];
                    mom[a][0] -= normal;
                    mom[a][1] += normal;
                }
                let mut div = S::zero();
                for a in 0..3 {
                    div += (u[a][1] - u[a][0]) * area[a];
                }
                for a in 0..3 {
                    out[2 * a] = mom[a][0] / p.fs;
                    out[2 * a + 1] = mom[a][1] / p.fs;
                }
                out[6] = div / p.ms;
                if self.part == CellPart::Standalone {
                    let thermal =
                        self.caloric(p, theta, v, z, old, to, dt, source) - dissipation(mu, v, &d, alpha, &u);
                    out[7] = thermal / p.hs;
                }
            }
            CellPart::Caloric => {
                out[0] = self.caloric(p, theta, v, z, old, to, dt, source) / p.hs;
            }
            CellPart::Dissipation => {
                out[0] = -dissipation(mu, v, &d, alpha, &u) / p.hs;
            }
        }
    }
}

fn dissipation<S: Scalar>(mu: S, v: S, d: &[S; 3], alpha: S, u: &[[S; 2]; 3]) -> S {
    let mut dd = S::zero();
    let mut uu = S::zero();
    for a in 0..3 {
        dd += d[a] * d[a];
        uu += u[a][0] * u[a][0] + u[a][1] * u[a][1];
    }
    mu * 2.0 * v * dd + alpha * v * 0.5 * uu
}

impl CellKernel {
    #[allow(clippy::too_many_arguments, clippy::unused_self)]
    fn caloric<S: Scalar>(
        &self,
        p: &MacParams,
        theta: S,
        v: S,
        z: &[S],
        old: &[S],
        to: S,
        dt: S,
        source: S,
    ) -> S {
        let dh = p.law.enthalpy_increment(to, (z[7] - old[7]) * p.ts);
        p.law.fraction(theta) * v * (dh * p.rho / dt - source)
    }
}

impl StepKernel for CellKernel {
    fn data_width(&self) -> usize {
        2
    }
    fn step_data(&self, n: usize, out: &mut [f64]) {
        let dt = self.p.times[n] - self.p.times[n.saturating_sub(1)];
        let q = self.p.heat[n];
        for e in 0..self.count {
            out[2 * e] = dt;
            out[2 * e + 1] = q;
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShearItem {
    pub a: usize,
    pub b: usize,
    pub fa: f64,
    pub fb: f64,
    pub vf: f64,
    pub weights: [f64; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShearPart {
    Standalone,
    Mechanics,
    Dissipation,
}

pub struct ShearKernel {
    pub p: Arc<MacParams>,
    pub items: Arc<Vec<ShearItem>>,
    pub part: ShearPart,
}

impl LocalResidual for ShearKernel {
    fn residual<S: Scalar>(&self, item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let p = &self.p;
        let it = &self.items[item];
        let h = spacing(x);
        let v = prod3(&h) * it.vf;
        let mut mu = S::zero();
        for (j, w) in it.weights.iter().enumerate() {
            let t = z[4 + j] * p.ts + p.t0;
            mu += p.law.properties(t).0 * *w;
        }
        let cb = S::from_f64(it.fa) / h[it.b];
        let ca = S::from_f64(it.fb) / h[it.a];
        let coefficients = [-cb, cb, -ca, ca];
        let mut shear = S::zero();
        for (c, zz) in coefficients.iter().zip(z.iter()) {
            shear += *c * (*zz * p.us);
        }
        if let Some(ml) = &p.turbulence {

            let mut phi = S::zero();
            for (j, w) in it.weights.iter().enumerate() {
                phi += (-x[3 + j] + 1.0) * *w;
            }
            mu += ml.eddy_viscosity(p.rho, phi, shear * shear);
        }
        let force: [S; 4] = std::array::from_fn(|j| mu * v * shear * coefficients[j]);
        let diss: [S; 4] = std::array::from_fn(|j| mu * v * shear * shear * it.weights[j]);
        match self.part {
            ShearPart::Standalone => {
                for j in 0..4 {
                    out[j] = force[j] / p.fs;
                    out[4 + j] = -diss[j] / p.hs;
                }
            }
            ShearPart::Mechanics => {
                for j in 0..4 {
                    out[j] = force[j] / p.fs;
                }
            }
            ShearPart::Dissipation => {
                for j in 0..4 {
                    out[j] = -diss[j] / p.hs;
                }
            }
        }
    }
}

impl StepKernel for ShearKernel {}

pub struct InteriorThermalKernel {
    pub p: Arc<MacParams>,
    pub axes: Arc<Vec<usize>>,
}

impl LocalResidual for InteriorThermalKernel {
    fn residual<S: Scalar>(&self, item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let p = &self.p;
        let axis = self.axes[item];
        let h = spacing(&x[..3]);
        let area = prod3(&h) / h[axis];
        let t = [z[0] * p.ts + p.t0, z[1] * p.ts + p.t0];
        let u = z[2] * p.us;
        let k = [
            p.law.properties(t[0]).1 * p.law.fraction(x[3]),
            p.law.properties(t[1]).1 * p.law.fraction(x[4]),
        ];
        let kf = k[0] * 2.0 * k[1] / (k[0] + k[1]);
        let hh = [p.law.enthalpy(t[0]), p.law.enthalpy(t[1])];
        let adv = area * p.rho * (u.max_f64(0.0) * hh[0] + u.min_f64(0.0) * hh[1]);
        let flux = adv + kf * area / h[axis] * (t[0] - t[1]);
        out[0] = flux / p.hs;
        out[1] = -flux / p.hs;
    }
}

impl StepKernel for InteriorThermalKernel {}

pub struct BoundaryThermalKernel {
    pub p: Arc<MacParams>,
    pub axis: usize,
    pub sign: f64,
    pub open: bool,
    pub temperature: bool,
    pub incoming: Vec<f64>,
    pub tb: Vec<f64>,
    pub count: usize,
}

impl BoundaryThermalKernel {
    pub fn power<S: Scalar>(&self, z: &[S], x: &[S], incoming: S, tb: S) -> S {
        let p = &self.p;
        let h = spacing(&x[..3]);
        let area = prod3(&h) / h[self.axis];
        let t = z[0] * p.ts + p.t0;
        let un = z[1] * (self.sign * p.us);
        let mut power = S::zero();
        if self.open {
            let hh = p.law.enthalpy(t);
            let hin = p.law.enthalpy(incoming);
            power = area * p.rho * (un.max_f64(0.0) * hh + un.min_f64(0.0) * hin);
        }
        if self.temperature {
            power += p.law.fraction(x[3]) * 2.0 * p.law.properties(t).1 * area / h[self.axis] * (t - tb);
        }
        power
    }
}

impl LocalResidual for BoundaryThermalKernel {
    fn residual<S: Scalar>(&self, _item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        out[0] = self.power(z, x, z[2], z[3]) / self.p.hs;
    }
}

impl StepKernel for BoundaryThermalKernel {
    fn data_width(&self) -> usize {
        2
    }
    fn step_data(&self, n: usize, out: &mut [f64]) {
        for e in 0..self.count {
            out[2 * e] = self.incoming[n];
            out[2 * e + 1] = self.tb[n];
        }
    }
}

pub struct OpenPressureKernel {
    pub p: Arc<MacParams>,
    pub axis: usize,
    pub sign: f64,
    pub pressure: Vec<f64>,
    pub count: usize,
}

impl LocalResidual for OpenPressureKernel {
    fn residual<S: Scalar>(&self, _item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let h = spacing(x);
        out[0] = prod3(&h) / h[self.axis] * ((z[1] - self.p.pref) * self.sign) / self.p.fs;
    }
}

impl StepKernel for OpenPressureKernel {
    fn data_width(&self) -> usize {
        1
    }
    fn step_data(&self, n: usize, out: &mut [f64]) {
        out[..self.count].fill(self.pressure[n]);
    }
}

pub struct AdvectionKernel {
    pub p: Arc<MacParams>,
    pub meta: Arc<Vec<(usize, f64)>>,
}

impl LocalResidual for AdvectionKernel {
    fn residual<S: Scalar>(&self, item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let p = &self.p;
        let (axis, factor) = self.meta[item];
        let h = spacing(x);
        let area = prod3(&h) / h[axis] * factor;
        let u = [z[0] * p.us, z[1] * p.us];
        let v = (z[2] + z[3]) * (p.us * 0.5);
        let (vp, vm) = match p.advection_smoothing {
            None => (v.max_f64(0.0), v.min_f64(0.0)),
            Some(delta) => {
                let magnitude = (v * v + delta * delta).sqrt();
                ((v + magnitude) * 0.5, (v - magnitude) * 0.5)
            }
        };
        let flux = area * p.rho * (vp * u[0] + vm * u[1]);
        out[0] = flux / p.fs;
        out[1] = -flux / p.fs;
    }
}

impl StepKernel for AdvectionKernel {}

pub struct OpeningAdvectionKernel {
    pub p: Arc<MacParams>,
    pub axis: usize,
    pub sign: f64,
    pub factors: Arc<Vec<f64>>,
}

impl LocalResidual for OpeningAdvectionKernel {
    fn residual<S: Scalar>(&self, item: usize, z: &[S], _old: &[S], x: &[S], out: &mut [S]) {
        let p = &self.p;
        let h = spacing(x);
        let area = prod3(&h) / h[self.axis] * self.factors[item];
        let u = z[0] * p.us;
        let v = (z[1] + z[2]) * (0.5 * p.us);
        out[0] = area * v * u * (self.sign * p.rho) / p.fs;
    }
}

impl StepKernel for OpeningAdvectionKernel {}
