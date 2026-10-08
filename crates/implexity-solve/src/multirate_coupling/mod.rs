// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


mod cache;
mod derivatives;
mod linear;
mod prescribed;
mod step;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Mutex;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::sparse::CsrMatrix;

use crate::interface_quasi_newton::IqnIls;
use crate::matrix::Jacobian;
use crate::newton_krylov::ExactKrylovPolicy;
use crate::time_stepper::{
    SecondOrderStepper, StepCotangent, StepDiagnostics, StepParameters, StepRecord, StepTangent, TimeStepper,
};

pub use prescribed::{HarmonicTrace, PrescribedTrace, TabulatedTrace, TraceMotion};
pub use step::{
    LEDGER_COUPLING_RESIDUAL, LEDGER_INTERFACE_WORK_A, LEDGER_INTERFACE_WORK_B, LEDGER_INTERFACE_WORK_DEFECT,
};

use cache::StepCache;

#[derive(Clone, Debug)]
pub struct SubcycleRecord {
    pub state: Vec<f64>,
    pub flux: Vec<f64>,
    pub samples: Vec<f64>,
    pub pairing: f64,
    pub ledger: BTreeMap<String, f64>,
    pub diagnostics: StepDiagnostics,
}

#[derive(Clone, Debug)]
pub struct SubcycleTangent {
    pub state: Vec<f64>,
    pub flux: Vec<f64>,
    pub samples: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct SubcycleCotangent {
    pub previous: Vec<f64>,
    pub trace_start: Vec<f64>,
    pub trace_end: Vec<f64>,
    pub design: Vec<f64>,
    pub time_scale: f64,
}


pub trait SubcycledField: Send + Sync {
    fn state_size(&self) -> usize;
    fn trace_size(&self) -> usize;
    fn design_size(&self) -> usize;
    fn sample_names(&self) -> &[String];


    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>>;


    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>>;


    fn subcycle(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<SubcycleRecord>;


    #[allow(clippy::too_many_arguments)]
    fn subcycle_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<SubcycleTangent>;


    #[allow(clippy::too_many_arguments)]
    fn subcycle_adjoint(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<SubcycleCotangent>;
    fn second_order(&self) -> Option<&dyn SecondOrderSubcycle> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct FieldJacobians {
    pub current: Jacobian,
    pub previous: Jacobian,
    pub flux: Jacobian,
    pub design: Jacobian,
    pub time_scale: Vec<f64>,
}


pub trait FluxDrivenField: Send + Sync {
    fn step_ledger(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<BTreeMap<String,f64>> { Ok(BTreeMap::new()) }
    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> { Ok(None) }
    fn check_derivative_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> {
        Ok(())
    }
    fn check_state_domain(&self, _n: usize, _current: &[f64], _previous: &[f64], _p: StepParameters<'_>) -> CaeResult<()> {
        Ok(())
    }
    fn newton_alternative(&self, _n: usize, _current: &[f64], _previous: &[f64], _flux: &[f64], _p: StepParameters<'_>, _direction: &[f64], _attempt: usize) -> CaeResult<Option<Jacobian>> {
        Ok(None)
    }

    fn state_size(&self) -> usize;
    fn design_size(&self) -> usize;
    fn sample_names(&self) -> &[String];
    fn nominal_step_s(&self) -> f64;


    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>>;


    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>>;
    fn trace_operator(&self) -> &CsrMatrix;
    fn check_trace_path(&self, n: usize, start: &[f64], end: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let _ = (n, start, end, p);
        Ok(())
    }
    fn predict(
        &self,
        n: usize,
        previous: &[f64],
        before_previous: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Vec<f64>;


    fn residual(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>>;


    fn jacobians(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<FieldJacobians>;
    fn admissible_step(&self, n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>, trial: f64) -> CaeResult<f64> {
        let _ = (n, current, direction, previous, p);
        Ok(trial)
    }


    fn current_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Jacobian> {
        Ok(self.jacobians(n, current, previous, flux, p)?.current)
    }
    fn current_flux_jacobians(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>) -> CaeResult<(Jacobian, Jacobian)> {
        let j = self.jacobians(n, current, previous, flux, p)?;
        Ok((j.current, j.flux))
    }


    fn samples(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>>;


    fn samples_vjp(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)>;
    fn explicit(&self) -> bool {
        false
    }
    fn second_order(&self) -> Option<&dyn SecondOrderField> {
        None
    }
}

pub trait SecondOrderSubcycle: Send + Sync {


    #[allow(clippy::too_many_arguments)]
    fn subcycle_adjoint_tangent(
        &self,
        n: usize,
        previous: &[f64],
        trace_start: &[f64],
        trace_end: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        flux_bar: &[f64],
        sample_bar: &[f64],
        d_previous: &[f64],
        d_trace_start: &[f64],
        d_trace_end: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<SubcycleCotangent>;
}

#[derive(Clone, Debug)]
pub struct ResidualAdjointTangent {
    pub current: Vec<f64>,
    pub previous: Vec<f64>,
    pub flux: Vec<f64>,
    pub design: Vec<f64>,
    pub time_scale: f64,
}

pub trait SecondOrderField: Send + Sync {


    #[allow(clippy::too_many_arguments)]
    fn residual_adjoint_tangent(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
        w: &[f64],
        d_current: &[f64],
        d_previous: &[f64],
        d_flux: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<ResidualAdjointTangent>;
}

#[derive(Clone, Debug)]
pub enum CouplingMode {
    Loose {
        predictor_order: u8,
    },
    StrongQuasiNewton {
        tolerance: f64,
        max_iterations: usize,
        reuse_steps: usize,
        initial_relaxation: f64,
    },
    StrongNewtonKrylov {
        tolerance: f64,
        max_iterations: usize,
        krylov: ExactKrylovPolicy,
    },
}

impl CouplingMode {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Loose { .. } => "loose",
            Self::StrongQuasiNewton { .. } => "strong_quasi_newton",
            Self::StrongNewtonKrylov { .. } => "strong_newton_krylov",
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Loose { predictor_order } => format!("loose(p={predictor_order})"),
            Self::StrongQuasiNewton { tolerance, max_iterations, reuse_steps, initial_relaxation } => {
                format!(
                    "strong_quasi_newton(tol={tolerance:e},max={max_iterations},reuse={reuse_steps},omega={initial_relaxation})"
                )
            }
            Self::StrongNewtonKrylov { tolerance, max_iterations, krylov } => format!(
                "strong_newton_krylov(tol={tolerance:e},max={max_iterations},rtol={:e},restart={},maxiter={})",
                krylov.rtol, krylov.restart, krylov.maxiter
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MultirateOptions {
    pub mode: CouplingMode,
    pub schur_ratio_limit: f64,
    pub work_defect_limit: f64,
}

impl MultirateOptions {
    #[must_use]
    pub fn new(mode: CouplingMode) -> Self {
        Self { mode, schur_ratio_limit: 0.5, work_defect_limit: 0.01 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FieldNewtonOptions {
    pub relative_tolerance: f64,
    pub max_iterations: usize,
}

impl Default for FieldNewtonOptions {
    fn default() -> Self {
        Self { relative_tolerance: 1e-12, max_iterations: 30 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LinearSolveOptions {
    pub relative_tolerance: f64,
    pub restart: usize,
    pub max_iterations: usize,
}

impl Default for LinearSolveOptions {
    fn default() -> Self {
        Self { relative_tolerance: 1e-11, restart: 40, max_iterations: 400 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateLayout {
    pub field_b: Range<usize>,
    pub field_a: Range<usize>,
    pub lags: Range<usize>,
}

pub const DEFAULT_STEP_CACHE_BYTES: u64 = 256 * 1024 * 1024;

pub const DEFAULT_SECANT_FILTER: f64 = 1e-10;

pub struct MultirateStepper<A, B> {
    a: A,
    b: B,
    options: MultirateOptions,
    identity: String,
    sample_names: Vec<String>,
    trace: CsrMatrix,
    n_b: usize,
    n_a: usize,
    n_t: usize,
    n_d: usize,
    ns_b: usize,
    lag_coefficients: Vec<f64>,
    newton: FieldNewtonOptions,
    linear: LinearSolveOptions,
    secant_filter: f64,
    memory: Mutex<Option<IqnIls>>,
    cache: Mutex<StepCache>,
    admitted: Mutex<BTreeSet<[u8; 32]>>,
}

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

fn binomial(n: usize, k: usize) -> f64 {
    let mut c = 1.0;
    for i in 0..k {
        c = c * (n - i) as f64 / (i + 1) as f64;
    }
    c
}

fn check_positive(label: &str, v: f64) -> CaeResult<()> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(contract(format!("multirate coupling: {label} must be finite and positive, got {v}")))
    }
}

impl<A: SubcycledField, B: FluxDrivenField> MultirateStepper<A, B> {


    pub fn new(a: A, b: B, options: MultirateOptions) -> CaeResult<Self> {
        let n_b = b.state_size();
        let n_a = a.state_size();
        let n_t = a.trace_size();
        let n_d = a.design_size();
        if n_b == 0 || n_t == 0 {
            return Err(contract("multirate coupling: field B state and trace must be non-empty"));
        }
        if b.design_size() != n_d {
            return Err(contract(format!(
                "multirate coupling: field A has {n_d} design values, field B {}; both fields must share the design",
                b.design_size()
            )));
        }
        let trace = b.trace_operator().clone();
        if trace.shape() != (n_t, n_b) {
            return Err(contract(format!(
                "multirate coupling: trace operator has shape {:?}, expected ({n_t}, {n_b}) (trace = flux size × field-B state)",
                trace.shape()
            )));
        }
        if !trace.is_finite() {
            return Err(contract("multirate coupling: trace operator must be finite"));
        }
        check_positive("field-B nominal step", b.nominal_step_s())?;
        let mut sample_names: Vec<String> = b.sample_names().to_vec();
        sample_names.extend(a.sample_names().iter().cloned());
        let mut seen = BTreeSet::new();
        for name in &sample_names {
            if !seen.insert(name.as_str()) {
                return Err(contract(format!(
                    "multirate coupling: duplicate sample name {name:?} across the two fields"
                )));
            }
        }
        let lags = validate_mode(&options.mode, n_t)?;
        if options.schur_ratio_limit.is_nan() || options.schur_ratio_limit <= 0.0 {
            return Err(contract("multirate coupling: schur_ratio_limit must be positive (or +inf)"));
        }
        if options.work_defect_limit.is_nan() || options.work_defect_limit < 0.0 {
            return Err(contract("multirate coupling: work_defect_limit must be non-negative (or +inf)"));
        }
        let lag_coefficients: Vec<f64> = (0..=lags)
            .map(|i| {
                let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
                sign * binomial(lags + 1, i + 1)
            })
            .collect();
        let identity = format!(
            "multirate_coupling/1:{}:b{n_b}:a{n_a}:t{n_t}:d{n_d}:samples[{}]",
            options.mode.describe(),
            sample_names.join(",")
        );
        let ns_b = b.sample_names().len();
        Ok(Self {
            a,
            b,
            options,
            identity,
            sample_names,
            trace,
            n_b,
            n_a,
            n_t,
            n_d,
            ns_b,
            lag_coefficients,
            newton: FieldNewtonOptions::default(),
            linear: LinearSolveOptions::default(),
            secant_filter: DEFAULT_SECANT_FILTER,
            memory: Mutex::new(None),
            cache: Mutex::new(StepCache::new(DEFAULT_STEP_CACHE_BYTES)),
            admitted: Mutex::new(BTreeSet::new()),
        })
    }

    #[must_use]
    pub fn with_identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = identity.into();
        self
    }



    pub fn with_field_newton(mut self, newton: FieldNewtonOptions) -> CaeResult<Self> {
        check_tolerance(newton.relative_tolerance)?;
        check_iterations(newton.max_iterations)?;
        self.newton = newton;
        Ok(self)
    }



    pub fn with_linear_solves(mut self, linear: LinearSolveOptions) -> CaeResult<Self> {
        check_tolerance(linear.relative_tolerance)?;
        if linear.restart == 0 || linear.max_iterations == 0 {
            return Err(contract(
                "multirate coupling: linear solve restart and max_iterations must be positive",
            ));
        }
        self.linear = linear;
        Ok(self)
    }



    pub fn with_secant_filter(mut self, filter: f64) -> CaeResult<Self> {
        if !filter.is_finite() || !(0.0..1.0).contains(&filter) {
            return Err(contract("multirate coupling: secant filter must be in [0, 1)"));
        }
        self.secant_filter = filter;
        if let Ok(mut memory) = self.memory.lock() {
            *memory = None;
        }
        Ok(self)
    }

    #[must_use]
    pub fn with_step_cache_bytes(self, bytes: u64) -> Self {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = StepCache::new(bytes);
        }
        self
    }

    #[must_use]
    pub fn field_a(&self) -> &A {
        &self.a
    }

    #[must_use]
    pub fn field_b(&self) -> &B {
        &self.b
    }

    #[must_use]
    pub fn options(&self) -> &MultirateOptions {
        &self.options
    }

    #[must_use]
    pub fn layout(&self) -> StateLayout {
        let lags = (self.lag_coefficients.len() - 1) * self.n_t;
        StateLayout {
            field_b: 0..self.n_b,
            field_a: self.n_b..self.n_b + self.n_a,
            lags: self.n_b + self.n_a..self.n_b + self.n_a + lags,
        }
    }

    #[must_use]
    pub fn cached_steps(&self) -> usize {
        self.cache.lock().map_or(0, |c| c.len())
    }

    pub fn clear_history(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.clear();
        }
        if let Ok(mut memory) = self.memory.lock() {
            *memory = None;
        }
    }

    fn total_state(&self) -> usize {
        self.layout().lags.end
    }

    fn lag_count(&self) -> usize {
        self.lag_coefficients.len() - 1
    }
}

fn validate_mode(mode: &CouplingMode, n_t: usize) -> CaeResult<usize> {
    match mode {
        CouplingMode::Loose { predictor_order } => {
            if *predictor_order > 3 {
                return Err(contract("multirate coupling: loose predictor order must be at most 3"));
            }
            Ok(usize::from(*predictor_order))
        }
        CouplingMode::StrongQuasiNewton { tolerance, max_iterations, initial_relaxation, .. } => {
            check_tolerance(*tolerance)?;
            check_iterations(*max_iterations)?;
            if !initial_relaxation.is_finite() || *initial_relaxation <= 0.0 || *initial_relaxation > 1.0 {
                return Err(contract("multirate coupling: IQN initial relaxation must be in (0, 1]"));
            }
            Ok(0)
        }
        CouplingMode::StrongNewtonKrylov { tolerance, max_iterations, krylov } => {
            check_tolerance(*tolerance)?;
            check_iterations(*max_iterations)?;
            check_tolerance(krylov.rtol)?;
            if krylov.restart == 0 || krylov.maxiter == 0 {
                return Err(contract(
                    "multirate coupling: Newton-Krylov restart and maxiter must be positive",
                ));
            }
            let workspace = (krylov.restart as u128 + 1) * 2 * n_t as u128 * 8;
            if workspace > krylov.maximum_workspace_bytes as u128 {
                return Err(contract(format!(
                    "multirate coupling: Krylov workspace of {workspace} bytes exceeds maximum_workspace_bytes {}",
                    krylov.maximum_workspace_bytes
                )));
            }
            Ok(0)
        }
    }
}

fn check_tolerance(tol: f64) -> CaeResult<()> {
    if tol.is_finite() && tol > 0.0 && tol < 1.0 {
        Ok(())
    } else {
        Err(contract(format!("multirate coupling: tolerance must be in (0, 1), got {tol}")))
    }
}

fn check_iterations(n: usize) -> CaeResult<()> {
    if n == 0 { Err(contract("multirate coupling: at least one iteration must be allowed")) } else { Ok(()) }
}

impl<A: SubcycledField, B: FluxDrivenField> TimeStepper for MultirateStepper<A, B> {
    fn identity(&self) -> &str {
        &self.identity
    }

    fn state_size(&self) -> usize {
        self.total_state()
    }

    fn design_size(&self) -> usize {
        self.n_d
    }

    fn sample_names(&self) -> &[String] {
        &self.sample_names
    }

    fn nominal_step_s(&self) -> f64 {
        self.b.nominal_step_s()
    }

    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        self.initial(design)
    }

    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.initial_vjp(design, cotangent)
    }

    fn advance(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> CaeResult<StepRecord> {
        self.advance_step(n, previous, p)
    }

    fn tangent(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<StepTangent> {
        self.tangent_step(n, previous, current, p, d_previous, d_design, d_time_scale)
    }

    fn adjoint(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<StepCotangent> {
        let mut out =
            self.adjoint_steps(n, previous, current, p, &[state_bar.to_vec()], &[sample_bar.to_vec()])?;
        out.pop().ok_or_else(|| contract("multirate coupling: empty adjoint batch"))
    }

    fn adjoint_many(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bars: &[Vec<f64>],
        sample_bars: &[Vec<f64>],
    ) -> CaeResult<Vec<StepCotangent>> {
        self.adjoint_steps(n, previous, current, p, state_bars, sample_bars)
    }

    fn second_order(&self) -> Option<&dyn SecondOrderStepper> {
        if self.a.second_order().is_some() && self.b.second_order().is_some() { Some(self) } else { None }
    }
}

impl<A: SubcycledField, B: FluxDrivenField> SecondOrderStepper for MultirateStepper<A, B> {
    fn adjoint_tangent(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        sample_bar: &[f64],
        d_previous: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<StepCotangent> {
        self.adjoint_tangent_step(n, previous, current, p, state_bar, sample_bar, d_previous, d_design)
    }
}

mod borrowed_fields;
