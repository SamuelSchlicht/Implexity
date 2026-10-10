// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END

use implexity_ad::{HyperDual, Scalar};
use implexity_core::CaeError;

use super::spectral::{Power, trace_function};
use crate::hyperelastic::kinematics::{Mat3, det, matmul, transpose};
use crate::util::contract;

pub const ARRUDA_BOYCE: [f64; 5] = [
    0.5,
    1.0 / 20.0,
    11.0 / 1050.0,
    19.0 / 7000.0,
    519.0 / 673_750.0,
];

#[derive(Debug, Clone, PartialEq)]
pub enum IsoLaw {
    NeoHookean { mu: f64 },
    MooneyRivlin { c10: f64, c01: f64 },
    Yeoh { c: Vec<f64> },
    Ogden { mu: Vec<f64>, alpha: Vec<f64> },
    Gent { mu: f64, jm: f64 },
    ArrudaBoyce { mu: f64, lambda_m: f64 },
    StVenantKirchhoff { mu: f64, lambda: f64 },
    CoupledNeoHookean { mu: f64, lambda: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Volumetric {
    Quadratic,
    Logarithmic,
    SimoTaylor,
    Miehe,
    OgdenBeta(f64),
    Incompressible,
    Coupled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fibres {
    pub k1: f64,
    pub k2: f64,
    pub kappa: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Prony {
    pub beta: Vec<f64>,
    pub tau: Vec<f64>,
}

impl Prony {
    #[must_use]
    pub fn factors(&self, dt: f64) -> Vec<(f64, f64)> {
        self.beta
            .iter()
            .zip(&self.tau)
            .map(|(b, t)| ((-dt / t).exp(), b * (-0.5 * dt / t).exp()))
            .collect()
    }

    #[must_use]
    pub fn step_factor(&self, dt: f64) -> f64 {
        1.0 + self.factors(dt).iter().map(|(_, b)| b).sum::<f64>()
    }

    #[must_use]
    pub fn storage_ratio(&self, omega: f64) -> f64 {
        1.0 + self
            .beta
            .iter()
            .zip(&self.tau)
            .map(|(b, t)| {
                let x = (omega * t).powi(2);
                b * x / (1.0 + x)
            })
            .sum::<f64>()
    }

    #[must_use]
    pub fn loss_ratio(&self, omega: f64) -> f64 {
        self.beta
            .iter()
            .zip(&self.tau)
            .map(|(b, t)| b * omega * t / (1.0 + (omega * t).powi(2)))
            .sum::<f64>()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectionalProny {
    pub matrix: Option<Prony>,
    pub fibres: Vec<Option<Prony>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RelaxationScale(pub [f64; 5]);

impl From<f64> for RelaxationScale {
    fn from(value: f64) -> Self {
        Self([value; 5])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    pub law: IsoLaw,
    pub bulk: f64,
    pub volumetric: Volumetric,
    pub fibres: Option<Fibres>,
    pub prony: Option<Prony>,
    pub directional_prony: Option<DirectionalProny>,
    pub density: f64,
    pub matrix_scale: f64,
    pub fibre_scale: f64,
    pub mu0: f64,
    pub kappa0: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Kin<S> {
    pub c: Mat3<S>,
    pub j: S,
    pub ln_j: S,
    pub i1: S,
    pub i2: S,
    pub jm23: S,
    pub i1b: S,
    pub i2b: S,
}

impl<S: Scalar> Kin<S> {
    pub fn of(f: &Mat3<S>) -> Self {
        let c = matmul(&transpose(f), f);
        let j = det(f);
        Self::from_c(c, j)
    }

    pub fn from_c(c: Mat3<S>, j: S) -> Self {
        let i1 = c[0][0] + c[1][1] + c[2][2];
        let mut cc = S::zero();
        for r in 0..3 {
            for s in 0..3 {
                cc += c[r][s] * c[r][s];
            }
        }
        let i2 = (i1 * i1 - cc) * 0.5;
        let ln_j = j.ln();
        let jm23 = (ln_j * (-2.0 / 3.0)).exp();
        Self {
            c,
            j,
            ln_j,
            i1,
            i2,
            jm23,
            i1b: i1 * jm23,
            i2b: i2 * jm23 * jm23,
        }
    }
}

impl IsoLaw {
    pub fn validate(&self) -> Result<(), CaeError> {
        let finite = |v: &[f64]| v.iter().copied().all(f64::is_finite);
        match self {
            Self::NeoHookean { mu } if finite(&[*mu]) && *mu > 0.0 => Ok(()),
            Self::MooneyRivlin { c10, c01 }
                if finite(&[*c10, *c01]) && *c10 > 0.0 && c10 + c01 > 0.0 =>
            {
                Ok(())
            }
            Self::Yeoh { c } if !c.is_empty() && c.len() <= 6 && finite(c) && c[0] > 0.0 => Ok(()),
            Self::Ogden { mu, alpha }
                if !mu.is_empty()
                    && mu.len() <= 6
                    && mu.len() == alpha.len()
                    && finite(mu)
                    && finite(alpha)
                    && mu.iter().zip(alpha).all(|(m, a)| *a != 0.0 && m * a > 0.0) =>
            {
                Ok(())
            }
            Self::Gent { mu, jm } if finite(&[*mu, *jm]) && *mu > 0.0 && *jm > 0.0 => Ok(()),
            Self::ArrudaBoyce { mu, lambda_m }
                if finite(&[*mu, *lambda_m]) && *mu > 0.0 && *lambda_m > 1.0 =>
            {
                Ok(())
            }
            Self::CoupledNeoHookean { mu, lambda } | Self::StVenantKirchhoff { mu, lambda }
                if finite(&[*mu, *lambda]) && *mu > 0.0 && *lambda >= 0.0 =>
            {
                Ok(())
            }
            Self::NeoHookean { .. } => {
                contract("neo_hookean requires a finite positive shear modulus mu_Pa")
            }
            Self::MooneyRivlin { .. } => {
                contract("mooney_rivlin requires finite c10_Pa > 0 and c10_Pa + c01_Pa > 0")
            }
            Self::Yeoh { .. } => {
                contract("yeoh requires 1..6 finite coefficients c_Pa with c_Pa[0] > 0")
            }
            Self::Ogden { .. } => contract(
                "ogden requires 1..6 finite (mu_Pa, alpha) pairs with nonzero alpha and mu_p alpha_p > 0 for every term",
            ),
            Self::Gent { .. } => contract("gent requires finite mu_Pa > 0 and jm > 0"),
            Self::ArrudaBoyce { .. } => {
                contract("arruda_boyce requires finite mu_Pa > 0 and lambda_m > 1")
            }
            Self::CoupledNeoHookean { .. } => {
                contract("neo_hookean_coupled requires finite mu_Pa > 0 and lambda_Pa >= 0")
            }
            Self::StVenantKirchhoff { .. } => {
                contract("st_venant_kirchhoff requires finite mu_Pa > 0 and lambda_Pa >= 0")
            }
        }
    }

    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::NeoHookean { .. } => "neo_hookean",
            Self::MooneyRivlin { .. } => "mooney_rivlin",
            Self::Yeoh { .. } => "yeoh",
            Self::Ogden { .. } => "ogden",
            Self::Gent { .. } => "gent",
            Self::ArrudaBoyce { .. } => "arruda_boyce",
            Self::CoupledNeoHookean { .. } => "neo_hookean_coupled",
            Self::StVenantKirchhoff { .. } => "st_venant_kirchhoff",
        }
    }

    pub fn energy<S: Scalar>(&self, k: &Kin<S>) -> S {
        match self {
            Self::NeoHookean { mu } => (k.i1b - 3.0) * (*mu * 0.5),
            Self::MooneyRivlin { c10, c01 } => (k.i1b - 3.0) * *c10 + (k.i2b - 3.0) * *c01,
            Self::Yeoh { c } => {
                let x = k.i1b - 3.0;
                let mut xp = x;
                let mut acc = S::zero();
                for ci in c {
                    acc += xp * *ci;
                    xp *= x;
                }
                acc
            }
            Self::Ogden { mu, alpha } => {
                let mut acc = S::zero();
                for (m, a) in mu.iter().zip(alpha) {
                    let tr = trace_function(&k.c, &Power(0.5 * a));
                    let scale = (k.ln_j * (-a / 3.0)).exp();
                    acc += (scale * tr - 3.0) * (m / a);
                }
                acc
            }
            Self::Gent { mu, jm } => ((k.i1b - 3.0) / (-*jm)).ln_1p() * (-mu * jm * 0.5),
            Self::ArrudaBoyce { mu, lambda_m } => {
                let mut acc = S::zero();
                let mut xp = k.i1b;
                let mut three = 3.0;
                let lm2 = lambda_m * lambda_m;
                let mut lp = 1.0;
                for ci in ARRUDA_BOYCE {
                    acc += (xp - three) * (ci / lp);
                    xp *= k.i1b;
                    three *= 3.0;
                    lp *= lm2;
                }
                acc * *mu
            }
            Self::CoupledNeoHookean { mu, .. } => (k.i1 - 3.0) * (*mu * 0.5) - k.ln_j * *mu,
            Self::StVenantKirchhoff { mu, lambda } => {
                let cc = k.i1 * k.i1 - k.i2 * 2.0;
                let tr = (k.i1 - 3.0) * 0.5;
                tr * tr * (*lambda * 0.5) + (cc - k.i1 * 2.0 + 3.0) * (*mu * 0.25)
            }
        }
    }
}

impl Volumetric {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Quadratic => "quadratic",
            Self::Logarithmic => "logarithmic",
            Self::SimoTaylor => "simo_taylor",
            Self::Miehe => "miehe",
            Self::OgdenBeta(_) => "ogden_beta",
            Self::Incompressible => "incompressible",
            Self::Coupled => "coupled",
        }
    }

    #[must_use]
    pub fn unit(self, j: f64) -> (f64, f64, f64) {
        match self {
            Self::Quadratic => (0.5 * (j - 1.0) * (j - 1.0), j - 1.0, 1.0),
            Self::Logarithmic => {
                let l = j.ln();
                (0.5 * l * l, l / j, (1.0 - l) / (j * j))
            }
            Self::SimoTaylor => (
                0.25 * (j * j - 1.0 - 2.0 * j.ln()),
                0.5 * (j - 1.0 / j),
                0.5 * (1.0 + 1.0 / (j * j)),
            ),
            Self::Miehe => (j - 1.0 - j.ln(), 1.0 - 1.0 / j, 1.0 / (j * j)),
            Self::OgdenBeta(b) => {
                let jb = j.powf(-b);
                (
                    (b * j.ln() + jb - 1.0) / (b * b),
                    (1.0 / j - jb / j) / b,
                    (-1.0 / (j * j) + (b + 1.0) * jb / (j * j)) / b,
                )
            }
            Self::Incompressible | Self::Coupled => (0.0, 0.0, 0.0),
        }
    }

    #[must_use]
    pub fn unit_third(self, j: f64) -> f64 {
        match self {
            Self::Quadratic | Self::Incompressible | Self::Coupled => 0.0,
            Self::Logarithmic => (2.0 * j.ln() - 3.0) / (j * j * j),
            Self::SimoTaylor => -1.0 / (j * j * j),
            Self::Miehe => -2.0 / (j * j * j),
            Self::OgdenBeta(b) => {
                (2.0 / (j * j * j) - (b + 1.0) * (b + 2.0) * j.powf(-b - 3.0)) / b
            }
        }
    }

    fn convex_limit(self) -> f64 {
        match self {
            Self::Logarithmic => std::f64::consts::E,
            Self::OgdenBeta(b) if b > -1.0 => (b + 1.0).powf(1.0 / b),
            _ => f64::INFINITY,
        }
    }

    pub fn energy<S: Scalar>(self, bulk: f64, j: S) -> S {
        let jv = j.value();
        let (u, du, d2u) = self.unit(jv);
        j.chain3(bulk * u, bulk * du, bulk * d2u, || {
            bulk * self.unit_third(jv)
        })
    }

    pub fn conjugate_point(self, bulk: f64, p: f64) -> Result<f64, CaeError> {
        let q = p / bulk;
        match self {
            Self::Incompressible | Self::Coupled => Ok(1.0),
            Self::Quadratic => Ok(1.0 + q),
            Self::SimoTaylor => Ok(q + (q * q + 1.0).sqrt()),
            Self::Miehe => {
                if q >= 1.0 {
                    return crate::util::convergence(
                        "volumetric stress exceeds the admissible range of the miehe function",
                    );
                }
                Ok(1.0 / (1.0 - q))
            }
            Self::Logarithmic | Self::OgdenBeta(_) => {
                let hi = self.convex_limit();
                let (qmax, lo_start) = if hi.is_finite() {
                    (self.unit(hi).1, 0.0)
                } else {
                    (f64::INFINITY, 0.0)
                };
                if q.partial_cmp(&qmax) != Some(std::cmp::Ordering::Less) {
                    return crate::util::convergence(format!(
                        "volumetric stress exceeds the admissible range of the {} function",
                        self.name()
                    ));
                }

                let (mut lo, mut up) = (lo_start, if hi.is_finite() { hi } else { 1.0 });
                while !hi.is_finite() && self.unit(up).1 < q {
                    up *= 2.0;
                    if up > 1e12 {
                        return crate::util::convergence("volumetric conjugate did not bracket");
                    }
                }
                let mut x = 1.0_f64.clamp(0.5 * (lo + up), up);
                if x <= lo || x >= up {
                    x = 0.5 * (lo + up);
                }
                for _ in 0..200 {
                    let (_, d, dd) = self.unit(x);
                    let r = d - q;
                    if r > 0.0 {
                        up = x;
                    } else {
                        lo = x;
                    }
                    if r.abs() <= 4.0 * f64::EPSILON * (1.0 + q.abs()) {
                        return Ok(x);
                    }
                    let mut next = x - r / dd;
                    if !(next > lo && next < up) || !next.is_finite() {
                        next = 0.5 * (lo + up);
                    }
                    if (next - x).abs() <= 1e-16 * x.abs() {
                        return Ok(next);
                    }
                    x = next;
                }
                Ok(x)
            }
        }
    }

    pub fn conjugate<S: Scalar>(self, bulk: f64, p: S) -> Result<S, CaeError> {
        if matches!(self, Self::Incompressible) {
            return Ok(S::zero());
        }
        let pv = p.value();
        let js = self.conjugate_point(bulk, pv)?;
        let (u, _, d2u) = self.unit(js);
        Ok(p.chain3(
            pv * (js - 1.0) - bulk * u,
            js - 1.0,
            1.0 / (bulk * d2u),
            || -self.unit_third(js) / (bulk * bulk * d2u * d2u * d2u),
        ))
    }
}

pub fn fibre_energy<S: Scalar>(f: &Fibres, k: &Kin<S>, directions: &[[S; 3]]) -> S {
    let mut acc = S::zero();
    for a in directions {
        let mut i4 = S::zero();
        for r in 0..3 {
            for s in 0..3 {
                i4 += a[r] * k.c[r][s] * a[s];
            }
        }
        let e = (k.i1b - 3.0) * f.kappa + (i4 * k.jm23 - 1.0) * (1.0 - 3.0 * f.kappa);
        if e.value() > 0.0 {
            acc += (e * e * f.k2).exp_m1() * (f.k1 / (2.0 * f.k2));
        }
    }
    acc
}

pub fn fibre_i4<S: Scalar>(c: &Mat3<S>, a: &[S; 3]) -> S {
    let mut i4 = S::zero();
    for r in 0..3 {
        for s in 0..3 {
            i4 += a[r] * c[r][s] * a[s];
        }
    }
    i4
}

impl Material {
    pub fn new(
        law: IsoLaw,
        bulk: f64,
        volumetric: Volumetric,
        fibres: Option<Fibres>,
        prony: Option<Prony>,
        density: f64,
    ) -> Result<Self, CaeError> {
        law.validate()?;
        let coupled = matches!(
            law,
            IsoLaw::CoupledNeoHookean { .. } | IsoLaw::StVenantKirchhoff { .. }
        );
        if coupled != matches!(volumetric, Volumetric::Coupled) {
            return contract(
                "neo_hookean_coupled and st_venant_kirchhoff carry their own volumetric part (volumetric function 'coupled'), every other law requires a decoupled volumetric function",
            );
        }
        if let Volumetric::OgdenBeta(b) = volumetric
            && (!b.is_finite() || b == 0.0)
        {
            return contract("ogden_beta volumetric function requires a finite nonzero beta");
        }
        if !matches!(volumetric, Volumetric::Incompressible | Volumetric::Coupled)
            && !(bulk.is_finite() && bulk > 0.0)
        {
            return contract("a finite positive bulk modulus bulk_Pa is required");
        }
        if let Some(f) = &fibres
            && !(f.k1.is_finite()
                && f.k1 >= 0.0
                && f.k2.is_finite()
                && f.k2 > 0.0
                && (0.0..=1.0 / 3.0).contains(&f.kappa))
        {
            return contract(
                "fibres require finite k1_Pa >= 0, k2 > 0 and dispersion kappa in [0, 1/3]",
            );
        }
        if let Some(p) = &prony
            && (p.beta.is_empty()
                || p.beta.len() > 12
                || p.beta.len() != p.tau.len()
                || p.beta.iter().any(|b| !b.is_finite() || *b < 0.0)
                || p.tau.iter().any(|t| !t.is_finite() || *t <= 0.0))
        {
            return contract("prony series requires 1..12 pairs of finite beta >= 0 and tau_s > 0");
        }
        if !(density.is_finite() && density >= 0.0) {
            return contract("density_kg_m3 must be finite and nonnegative");
        }
        let mut m = Self {
            law,
            bulk,
            volumetric,
            fibres,
            prony,
            directional_prony: None,
            density,
            matrix_scale: 1.0,
            fibre_scale: 1.0,
            mu0: 0.0,
            kappa0: 0.0,
        };
        m.mu0 = m.initial_shear();
        if !(m.mu0.is_finite() && m.mu0 > 0.0) {
            return contract("the law has no positive initial shear modulus");
        }
        m.kappa0 = match (&m.law, volumetric) {
            (
                IsoLaw::CoupledNeoHookean { mu, lambda } | IsoLaw::StVenantKirchhoff { mu, lambda },
                _,
            ) => lambda + 2.0 * mu / 3.0,
            (_, Volumetric::Incompressible) => f64::INFINITY,
            _ => bulk,
        };
        Ok(m)
    }

    pub fn with_directional_prony(mut self, spectra: DirectionalProny) -> Result<Self, CaeError> {
        if self.prony.is_some()
            || self.coupled()
            || spectra.fibres.len() > 4
            || (self.fibres.is_none() && !spectra.fibres.is_empty())
        {
            return contract(
                "directional_prony requires a decoupled law, no common prony and at most four fibre spectra",
            );
        }
        for p in spectra
            .matrix
            .iter()
            .chain(spectra.fibres.iter().filter_map(Option::as_ref))
        {
            if p.beta.is_empty()
                || p.beta.len() > 12
                || p.beta.len() != p.tau.len()
                || p.beta.iter().any(|b| !b.is_finite() || *b < 0.0)
                || p.tau.iter().any(|t| !t.is_finite() || *t <= 0.0)
            {
                return contract(
                    "directional spectra require 1..12 finite beta >= 0 and tau_s > 0 pairs",
                );
            }
        }
        self.directional_prony = Some(spectra);
        Ok(self)
    }

    pub(crate) fn relaxation_channel(&self, mut branch: usize) -> usize {
        if self.prony.is_some() {
            return 0;
        }
        if let Some(p) = &self.directional_prony {
            for (channel, series) in core::iter::once(p.matrix.as_ref())
                .chain(p.fibres.iter().map(Option::as_ref))
                .enumerate()
            {
                if let Some(series) = series {
                    if branch < series.beta.len() {
                        return channel;
                    }
                    branch -= series.beta.len();
                }
            }
        }
        unreachable!("relaxation branch outside validated spectrum")
    }

    pub fn relaxation_branches(&self) -> Vec<(usize, f64, f64)> {
        if let Some(p) = &self.prony {
            return p
                .beta
                .iter()
                .zip(&p.tau)
                .map(|(b, t)| (0, *b, *t))
                .collect();
        }
        let mut out = Vec::new();
        if let Some(p) = &self.directional_prony {
            for (channel, series) in core::iter::once(p.matrix.as_ref())
                .chain(p.fibres.iter().map(Option::as_ref))
                .enumerate()
            {
                if let Some(series) = series {
                    out.extend(
                        series
                            .beta
                            .iter()
                            .zip(&series.tau)
                            .map(|(b, t)| (channel, *b, *t)),
                    );
                }
            }
        }
        out
    }

    pub fn channel_energy<S: Scalar>(
        &self,
        k: &Kin<S>,
        directions: &[[S; 3]],
        channel: usize,
    ) -> S {
        if self.directional_prony.is_none() || channel >= 5 {
            return self.isochoric(k, directions);
        }
        if channel == 0 {
            self.law.energy(k) * self.matrix_scale
        } else if let (Some(f), Some(a)) = (&self.fibres, directions.get(channel - 1)) {
            fibre_energy(f, k, core::slice::from_ref(a)) * self.fibre_scale
        } else {
            S::zero()
        }
    }

    pub fn scaled_isochoric<S: Scalar>(
        &self,
        k: &Kin<S>,
        directions: &[[S; 3]],
        g: RelaxationScale,
    ) -> S {
        if self.directional_prony.is_none() {
            return self.isochoric(k, directions) * g.0[0];
        }
        let mut w = self.law.energy(k) * (g.0[0] * self.matrix_scale);
        if let Some(f) = &self.fibres {
            for (i, a) in directions.iter().enumerate() {
                w += fibre_energy(f, k, core::slice::from_ref(a)) * (g.0[i + 1] * self.fibre_scale);
            }
        }
        w
    }

    pub fn with_evolution(
        mut self,
        evolution: &super::evolution::PrescribedEvolution,
    ) -> Result<Self, CaeError> {
        if self.coupled() {
            return contract("material evolution requires a decoupled soft-solid law");
        }
        let (_, f) = evolution.endpoint()?;
        self.matrix_scale = f.matrix_stiffness;
        self.fibre_scale = f.fibre_stiffness;
        let scale = |p: &mut Prony, beta: f64, tau: f64| {
            for b in &mut p.beta {
                *b *= beta;
            }
            for t in &mut p.tau {
                *t *= tau;
            }
        };
        if let Some(p) = &mut self.prony {
            if self.fibres.is_some()
                && (f.matrix_beta != f.fibre_beta || f.matrix_tau != f.fibre_tau)
            {
                return contract(
                    "different matrix and fibre evolution spectra require directional_prony",
                );
            }
            scale(p, f.matrix_beta, f.matrix_tau);
        }
        if let Some(p) = &mut self.directional_prony {
            if let Some(m) = &mut p.matrix {
                scale(m, f.matrix_beta, f.matrix_tau);
            }
            for p in p.fibres.iter_mut().filter_map(Option::as_mut) {
                scale(p, f.fibre_beta, f.fibre_tau);
            }
        }
        self.mu0 = self.initial_shear();
        if !self.mu0.is_finite() || self.mu0 <= 0.0 {
            return contract("evolution yields an invalid initial shear modulus");
        }
        for (_, b, t) in self.relaxation_branches() {
            if !b.is_finite() || b < 0.0 || !t.is_finite() || t <= 0.0 {
                return contract("evolution yields an invalid relaxation spectrum");
            }
        }
        Ok(self)
    }

    fn initial_shear(&self) -> f64 {
        let s = HyperDual::new(0.0, 1.0, 1.0, 0.0);
        let one = HyperDual::constant(1.0);
        let zero = HyperDual::constant(0.0);
        let f = [[one, s, zero], [zero, one, zero], [zero, zero, one]];
        self.law.energy(&Kin::of(&f)).e12 * self.matrix_scale
    }

    #[must_use]
    pub fn coupled(&self) -> bool {
        matches!(self.volumetric, Volumetric::Coupled)
    }

    pub fn isochoric<S: Scalar>(&self, k: &Kin<S>, directions: &[[S; 3]]) -> S {
        let mut w = self.law.energy(k) * self.matrix_scale;
        if let Some(f) = &self.fibres {
            w += fibre_energy(f, k, directions) * self.fibre_scale;
        }
        w
    }

    pub fn volumetric_energy<S: Scalar>(&self, k: &Kin<S>) -> S {
        match (&self.law, self.volumetric) {
            (IsoLaw::CoupledNeoHookean { lambda, .. }, _) => k.ln_j * k.ln_j * (*lambda * 0.5),
            (IsoLaw::StVenantKirchhoff { .. }, _) => S::zero(),
            (_, v) => v.energy(self.bulk, k.j),
        }
    }
}
