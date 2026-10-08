// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;

use super::{FieldJacobians, FluxDrivenField, ResidualAdjointTangent, SecondOrderField};
use crate::matrix::Jacobian;
use crate::time_stepper::StepParameters;

pub trait TraceMotion: Send + Sync {
    fn size(&self) -> usize;


    fn value(&self, n: usize, time_s: f64) -> CaeResult<Vec<f64>>;


    fn rate(&self, n: usize, time_s: f64) -> CaeResult<Vec<f64>>;
}

#[derive(Clone, Debug)]
pub struct TabulatedTrace {
    rows: Vec<Vec<f64>>,
}

impl TabulatedTrace {


    pub fn new(rows: Vec<Vec<f64>>) -> CaeResult<Self> {
        let size = rows.first().map_or(0, Vec::len);
        if size == 0 || rows.iter().any(|r| r.len() != size || r.iter().any(|v| !v.is_finite())) {
            return Err(CaeError::contract(
                "tabulated trace: rows must be non-empty, of equal length and finite",
            ));
        }
        Ok(Self { rows })
    }
}

impl TraceMotion for TabulatedTrace {
    fn size(&self) -> usize {
        self.rows[0].len()
    }

    fn value(&self, n: usize, _time_s: f64) -> CaeResult<Vec<f64>> {
        self.rows.get(n).cloned().ok_or_else(|| {
            CaeError::contract(format!(
                "tabulated trace: step {n} is beyond the table of {} rows",
                self.rows.len()
            ))
        })
    }

    fn rate(&self, n: usize, _time_s: f64) -> CaeResult<Vec<f64>> {
        self.value(n, 0.0).map(|row| vec![0.0; row.len()])
    }
}

#[derive(Clone, Debug)]
pub struct HarmonicTrace {
    mean: Vec<f64>,
    amplitude: Vec<f64>,
    angular_frequency_rad_s: f64,
    phase_rad: f64,
}

impl HarmonicTrace {


    pub fn new(
        mean: Vec<f64>,
        amplitude: Vec<f64>,
        angular_frequency_rad_s: f64,
        phase_rad: f64,
    ) -> CaeResult<Self> {
        if mean.is_empty()
            || mean.len() != amplitude.len()
            || mean.iter().chain(&amplitude).any(|v| !v.is_finite())
            || !angular_frequency_rad_s.is_finite()
            || !phase_rad.is_finite()
        {
            return Err(CaeError::contract(
                "harmonic trace: mean and amplitude must be non-empty, equal and finite",
            ));
        }
        Ok(Self { mean, amplitude, angular_frequency_rad_s, phase_rad })
    }
}

impl TraceMotion for HarmonicTrace {
    fn size(&self) -> usize {
        self.mean.len()
    }

    fn value(&self, _n: usize, time_s: f64) -> CaeResult<Vec<f64>> {
        let s = (self.angular_frequency_rad_s * time_s + self.phase_rad).sin();
        Ok(self.mean.iter().zip(&self.amplitude).map(|(m, a)| m + a * s).collect())
    }

    fn rate(&self, _n: usize, time_s: f64) -> CaeResult<Vec<f64>> {
        let c = self.angular_frequency_rad_s * (self.angular_frequency_rad_s * time_s + self.phase_rad).cos();
        Ok(self.amplitude.iter().map(|a| a * c).collect())
    }
}

pub struct PrescribedTrace<M> {
    motion: M,
    nominal_step_s: f64,
    design_size: usize,
    identity: CsrMatrix,
    zero_square: CsrMatrix,
    zero_design: CsrMatrix,
    sample_names: Vec<String>,
}

impl<M: TraceMotion> PrescribedTrace<M> {


    pub fn new(motion: M, nominal_step_s: f64, design_size: usize) -> CaeResult<Self> {
        let n = motion.size();
        if n == 0 {
            return Err(CaeError::contract("prescribed trace: the motion must have a positive size"));
        }
        if !(nominal_step_s.is_finite() && nominal_step_s > 0.0) {
            return Err(CaeError::contract("prescribed trace: the nominal step must be finite and positive"));
        }
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        Ok(Self {
            motion,
            nominal_step_s,
            design_size,
            identity: CsrMatrix::identity(n),
            zero_square: CsrMatrix::from_triplets(n, n, &[], &[], &[]).map_err(lin)?,
            zero_design: CsrMatrix::from_triplets(n, design_size, &[], &[], &[]).map_err(lin)?,
            sample_names: Vec::new(),
        })
    }

    #[must_use]
    pub fn motion(&self) -> &M {
        &self.motion
    }

    fn time(&self, n: usize, p: StepParameters<'_>) -> f64 {
        n as f64 * p.time_scale * self.nominal_step_s
    }

    fn target(&self, n: usize, p: StepParameters<'_>) -> CaeResult<Vec<f64>> {
        let d = self.motion.value(n, self.time(n, p))?;
        if d.len() != self.motion.size() || d.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(
                "prescribed trace: motion returned a wrong-length or non-finite trace",
            ));
        }
        Ok(d)
    }
}

impl<M: TraceMotion> FluxDrivenField for PrescribedTrace<M> {
    fn state_size(&self) -> usize {
        self.motion.size()
    }

    fn design_size(&self) -> usize {
        self.design_size
    }

    fn sample_names(&self) -> &[String] {
        &self.sample_names
    }

    fn nominal_step_s(&self) -> f64 {
        self.nominal_step_s
    }

    fn initial_state(&self, _design: &[f64]) -> CaeResult<Vec<f64>> {
        self.target(0, StepParameters { design: &[], time_scale: 1.0 })
    }

    fn initial_state_vjp(&self, _design: &[f64], _cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(vec![0.0; self.design_size])
    }

    fn trace_operator(&self) -> &CsrMatrix {
        &self.identity
    }

    fn predict(
        &self,
        n: usize,
        previous: &[f64],
        _before_previous: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Vec<f64> {
        self.target(n, p).unwrap_or_else(|_| previous.to_vec())
    }

    fn residual(
        &self,
        n: usize,
        current: &[f64],
        _previous: &[f64],
        _flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        let d = self.target(n, p)?;
        Ok(current.iter().zip(&d).map(|(z, t)| z - t).collect())
    }

    fn jacobians(
        &self,
        n: usize,
        _current: &[f64],
        _previous: &[f64],
        _flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<FieldJacobians> {
        let dt_dtau = n as f64 * self.nominal_step_s;
        let rate = self.motion.rate(n, self.time(n, p))?;
        Ok(FieldJacobians {
            current: Jacobian::Csr(self.identity.clone()),
            previous: Jacobian::Csr(self.zero_square.clone()),
            flux: Jacobian::Csr(self.zero_square.clone()),
            design: Jacobian::Csr(self.zero_design.clone()),
            time_scale: rate.iter().map(|r| -r * dt_dtau).collect(),
        })
    }

    fn samples(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        Ok(Vec::new())
    }

    fn samples_vjp(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        _sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        Ok((vec![0.0; current.len()], vec![0.0; current.len()], vec![0.0; self.design_size]))
    }

    fn explicit(&self) -> bool {
        true
    }

    fn second_order(&self) -> Option<&dyn SecondOrderField> {
        Some(self)
    }
}

impl<M: TraceMotion> SecondOrderField for PrescribedTrace<M> {
    fn residual_adjoint_tangent(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        flux: &[f64],
        _p: StepParameters<'_>,
        _w: &[f64],
        _d_current: &[f64],
        _d_previous: &[f64],
        _d_flux: &[f64],
        _d_design: Option<&[f64]>,
    ) -> CaeResult<ResidualAdjointTangent> {
        Ok(ResidualAdjointTangent {
            current: vec![0.0; current.len()],
            previous: vec![0.0; current.len()],
            flux: vec![0.0; flux.len()],
            design: vec![0.0; self.design_size],
            time_scale: 0.0,
        })
    }
}
