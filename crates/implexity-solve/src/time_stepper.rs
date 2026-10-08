// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use implexity_ad::AdError;
use implexity_ad::scan::ScanStep;
use implexity_core::error::{CaeError, CaeResult};
use sha2::{Digest, Sha256};

use crate::convergence::{Criterion, ScalarL2Criterion};
use crate::exact_matrix::ExactMatrixIdentity;
use crate::factorization::Factorization;
use crate::implicit_block::{BlockCallbacks, BlockOptions, ImplicitBlockSystem};
use crate::matrix::{Jacobian, checked_matrix};
use crate::native_history::{HistoryOptions, HistoryPoint, HistoryProblem};
use crate::preconditioner_lease::{ExactPreconditionerBinding, SealedMatrixReadLease};

#[derive(Clone, Copy, Debug)]
pub struct StepParameters<'a> {
    pub design: &'a [f64],
    pub time_scale: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepDiagnostics {
    pub newton_iterations: usize,
    pub coupling_iterations: usize,
    pub krylov_iterations: usize,
    pub residual_norm: f64,
}

#[derive(Clone, Debug)]
pub struct StepRecord {
    pub state: Vec<f64>,
    pub samples: Vec<f64>,
    pub ledger: BTreeMap<String, f64>,
    pub diagnostics: StepDiagnostics,
}

#[derive(Clone, Debug)]
pub struct StepTangent {
    pub state: Vec<f64>,
    pub samples: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepCotangent {
    pub previous: Vec<f64>,
    pub design: Vec<f64>,
    pub time_scale: f64,
}


pub trait TimeStepper: Send + Sync {
    fn identity(&self) -> &str;
    fn state_size(&self) -> usize;
    fn design_size(&self) -> usize;
    fn sample_names(&self) -> &[String];
    fn nominal_step_s(&self) -> f64;


    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>>;


    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>>;


    fn advance(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> CaeResult<StepRecord>;


    #[allow(clippy::too_many_arguments)]
    fn tangent(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        d_previous: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<StepTangent>;


    fn adjoint(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bar: &[f64],
        sample_bar: &[f64],
    ) -> CaeResult<StepCotangent>;


    fn adjoint_many(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
        state_bars: &[Vec<f64>],
        sample_bars: &[Vec<f64>],
    ) -> CaeResult<Vec<StepCotangent>> {
        if state_bars.len() != sample_bars.len() {
            return Err(CaeError::contract(format!(
                "time stepper adjoint batch has {} state and {} sample cotangents",
                state_bars.len(),
                sample_bars.len()
            )));
        }
        state_bars.iter().zip(sample_bars).map(|(s, b)| self.adjoint(n, previous, current, p, s, b)).collect()
    }
    fn second_order(&self) -> Option<&dyn SecondOrderStepper> {
        None
    }
}

pub trait SecondOrderStepper: Send + Sync {


    #[allow(clippy::too_many_arguments)]
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
    ) -> CaeResult<StepCotangent>;
}

pub(crate) fn check_vector(values: &[f64], expected: usize, what: &str) -> CaeResult<()> {
    if values.len() != expected {
        return Err(CaeError::contract(format!(
            "{what} has length {} but {expected} is required",
            values.len()
        )));
    }
    if values.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::contract(format!("{what} contains non-finite values")));
    }
    Ok(())
}

pub(crate) fn check_parameters(stepper: &dyn TimeStepper, p: StepParameters<'_>) -> CaeResult<()> {
    check_vector(p.design, stepper.design_size(), "time stepper design")?;
    if !(p.time_scale.is_finite() && p.time_scale > 0.0) {
        return Err(CaeError::contract("time stepper time scale must be finite and positive"));
    }
    Ok(())
}

fn ad_error(e: &AdError) -> CaeError {
    match e {
        AdError::Shape(_) | AdError::Invalid(_) => CaeError::contract(e.to_string()),
        AdError::Singular(_) | AdError::NonFinite(_) | AdError::Callback(_) => {
            CaeError::convergence(e.to_string())
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[derive(Clone, Debug, PartialEq)]
pub struct SampleCotangent {
    pub current: Vec<f64>,
    pub previous: Vec<f64>,
    pub design: Vec<f64>,
    pub time_scale: f64,
}

impl SampleCotangent {
    #[must_use]
    pub const fn zero() -> Self {
        Self { current: Vec::new(), previous: Vec::new(), design: Vec::new(), time_scale: 0.0 }
    }

    fn check(&self, state: usize, design: usize) -> CaeResult<()> {
        for (part, n, what) in [
            (&self.current, state, "sample cotangent on the current state"),
            (&self.previous, state, "sample cotangent on the previous state"),
            (&self.design, design, "sample cotangent on the design"),
        ] {
            if !part.is_empty() {
                check_vector(part, n, what)?;
            }
        }
        if !self.time_scale.is_finite() {
            return Err(CaeError::contract("sample cotangent on the time scale is not finite"));
        }
        Ok(())
    }
}

fn add_part(acc: &mut [f64], part: &[f64]) {
    for (a, b) in acc.iter_mut().zip(part) {
        *a += b;
    }
}

pub trait SampleMap: Send + Sync {
    fn names(&self) -> &[String];


    fn samples(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>>;


    #[allow(clippy::too_many_arguments)]
    fn tangent(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        d_current: &[f64],
        d_previous: &[f64],
        d_design: Option<&[f64]>,
        d_time_scale: f64,
    ) -> CaeResult<Vec<f64>>;


    fn vjp(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<SampleCotangent>;
}

#[derive(Clone, Debug, Default)]
pub struct NoSamples;

impl SampleMap for NoSamples {
    fn names(&self) -> &[String] {
        &[]
    }
    fn samples(&self, _n: usize, _c: &[f64], _p: &[f64], _q: StepParameters<'_>) -> CaeResult<Vec<f64>> {
        Ok(Vec::new())
    }
    fn tangent(
        &self,
        _n: usize,
        _c: &[f64],
        _p: &[f64],
        _q: StepParameters<'_>,
        _dc: &[f64],
        _dp: &[f64],
        _dx: Option<&[f64]>,
        _dt: f64,
    ) -> CaeResult<Vec<f64>> {
        Ok(Vec::new())
    }
    fn vjp(
        &self,
        _n: usize,
        _current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<SampleCotangent> {
        check_vector(sample_bar, 0, "sample cotangent")?;
        Ok(SampleCotangent::zero())
    }
}

#[derive(Clone, Debug)]
pub struct StateSamples {
    names: Vec<String>,
    indices: Vec<usize>,
}

impl StateSamples {


    pub fn new(names: Vec<String>, indices: Vec<usize>) -> CaeResult<Self> {
        if names.len() != indices.len() {
            return Err(CaeError::contract("state samples need one name per index"));
        }
        let mut seen = std::collections::BTreeSet::new();
        if names.iter().any(|n| n.is_empty() || !seen.insert(n.as_str())) {
            return Err(CaeError::contract("state sample names must be nonempty and unique"));
        }
        Ok(Self { names, indices })
    }

    #[must_use]
    pub fn all(size: usize) -> Self {
        Self { names: (0..size).map(|i| format!("z[{i}]")).collect(), indices: (0..size).collect() }
    }

    fn check(&self, current: &[f64]) -> CaeResult<()> {
        match self.indices.iter().find(|&&i| i >= current.len()) {
            Some(i) => Err(CaeError::contract(format!(
                "state sample index {i} is outside a state of length {}",
                current.len()
            ))),
            None => Ok(()),
        }
    }
}

impl SampleMap for StateSamples {
    fn names(&self) -> &[String] {
        &self.names
    }
    fn samples(&self, _n: usize, current: &[f64], _p: &[f64], _q: StepParameters<'_>) -> CaeResult<Vec<f64>> {
        self.check(current)?;
        Ok(self.indices.iter().map(|&i| current[i]).collect())
    }
    fn tangent(
        &self,
        _n: usize,
        current: &[f64],
        _p: &[f64],
        _q: StepParameters<'_>,
        d_current: &[f64],
        _dp: &[f64],
        _dx: Option<&[f64]>,
        _dt: f64,
    ) -> CaeResult<Vec<f64>> {
        self.check(current)?;
        check_vector(d_current, current.len(), "state sample tangent direction")?;
        Ok(self.indices.iter().map(|&i| d_current[i]).collect())
    }
    fn vjp(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        _p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<SampleCotangent> {
        self.check(current)?;
        check_vector(sample_bar, self.indices.len(), "state sample cotangent")?;
        let mut bar = vec![0.0; current.len()];
        for (&i, &s) in self.indices.iter().zip(sample_bar) {
            bar[i] += s;
        }
        Ok(SampleCotangent { current: bar, ..SampleCotangent::zero() })
    }
}

pub trait InitialStateMap: Send + Sync {


    fn state(&self, design: &[f64]) -> CaeResult<Vec<f64>>;


    fn vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>>;
}

#[derive(Clone, Debug)]
pub struct FixedInitialState(pub Vec<f64>);

impl InitialStateMap for FixedInitialState {
    fn state(&self, _design: &[f64]) -> CaeResult<Vec<f64>> {
        Ok(self.0.clone())
    }
    fn vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        check_vector(cotangent, self.0.len(), "initial-state cotangent")?;
        Ok(vec![0.0; design.len()])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeScaleMode {
    Fixed,
    AppendedParameter,
}

impl TimeScaleMode {
    fn parameters(self, p: StepParameters<'_>, identity: &str) -> CaeResult<Vec<f64>> {
        match self {
            Self::Fixed => {
                #[allow(clippy::float_cmp)]                                               
                if p.time_scale != 1.0 {
                    return Err(CaeError::contract(format!(
                        "time stepper {identity} has a fixed step size; time_scale must be 1 (got {})",
                        p.time_scale
                    )));
                }
                Ok(p.design.to_vec())
            }
            Self::AppendedParameter => {
                let mut x = Vec::with_capacity(p.design.len() + 1);
                x.extend_from_slice(p.design);
                x.push(p.time_scale);
                Ok(x)
            }
        }
    }

    fn direction(
        self,
        design_size: usize,
        d_design: Option<&[f64]>,
        d_time_scale: f64,
        identity: &str,
    ) -> CaeResult<Vec<f64>> {
        let mut d = match d_design {
            Some(d) => {
                check_vector(d, design_size, "time stepper design direction")?;
                d.to_vec()
            }
            None => vec![0.0; design_size],
        };
        if !d_time_scale.is_finite() {
            return Err(CaeError::contract("time stepper time-scale direction must be finite"));
        }
        match self {
            Self::Fixed => {
                if d_time_scale != 0.0 {
                    return Err(CaeError::contract(format!(
                        "time stepper {identity} has a fixed step size and no time-scale derivative"
                    )));
                }
            }
            Self::AppendedParameter => d.push(d_time_scale),
        }
        Ok(d)
    }

    fn split(self, mut bar: Vec<f64>) -> (Vec<f64>, f64) {
        match self {
            Self::Fixed => (bar, 0.0),
            Self::AppendedParameter => {
                let t = bar.pop().unwrap_or(0.0);
                (bar, t)
            }
        }
    }

    const fn extra(self) -> usize {
        match self {
            Self::Fixed => 0,
            Self::AppendedParameter => 1,
        }
    }
}

#[derive(Clone)]
pub struct HistoryStepperOptions {
    pub history: HistoryOptions,
    pub factorization_cache_bytes: u64,
    pub time_scale: TimeScaleMode,
}

impl Default for HistoryStepperOptions {
    fn default() -> Self {
        Self {
            history: HistoryOptions::default(),
            factorization_cache_bytes: 256 << 20,
            time_scale: TimeScaleMode::Fixed,
        }
    }
}

struct StepCallbacks {
    problem: Arc<dyn HistoryProblem>,
    n: usize,
}

impl BlockCallbacks<[f64]> for StepCallbacks {
    fn residual(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Vec<f64>> {
        self.problem.residual(self.n, z, prev, x)
    }
    fn state_jacobian(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Jacobian> {
        self.problem.state_jacobian(self.n, z, prev, x)
    }
    fn design_jacobian(&self, z: &[f64], x: &[f64], prev: &[f64]) -> CaeResult<Jacobian> {
        self.problem.design_jacobian(self.n, z, prev, x)
    }
}

#[derive(Default)]
struct FactorizationCache {
    entries: VecDeque<([u8; 32], Arc<Factorization>)>,
    bytes: u64,
    builds: usize,
    hits: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FactorizationCacheStats {
    pub builds: usize,
    pub hits: usize,
    pub entries: usize,
    pub bytes: u64,
}

pub struct HistoryProblemStepper {
    identity: String,
    problem: Arc<dyn HistoryProblem>,
    options: HistoryStepperOptions,
    criterion: Criterion,
    state_size: usize,
    design_size: usize,
    nominal_step_s: f64,
    samples: Arc<dyn SampleMap>,
    initial: Arc<dyn InitialStateMap>,
    cache: Mutex<FactorizationCache>,
}

impl std::fmt::Debug for HistoryProblemStepper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryProblemStepper")
            .field("identity", &self.identity)
            .field("state_size", &self.state_size)
            .field("design_size", &self.design_size)
            .field("nominal_step_s", &self.nominal_step_s)
            .field("time_scale", &self.options.time_scale)
            .finish_non_exhaustive()
    }
}

impl HistoryProblemStepper {


    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: impl Into<String>,
        problem: Arc<dyn HistoryProblem>,
        state_size: usize,
        design_size: usize,
        nominal_step_s: f64,
        samples: Arc<dyn SampleMap>,
        initial: Arc<dyn InitialStateMap>,
        options: HistoryStepperOptions,
    ) -> CaeResult<Self> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(CaeError::contract("time stepper identity must be nonempty"));
        }
        if state_size == 0 {
            return Err(CaeError::contract("time stepper state must be nonempty"));
        }
        if design_size + options.time_scale.extra() == 0 {
            return Err(CaeError::contract(
                "an implicit history step needs at least one parameter (design or appended time scale)",
            ));
        }
        if !(nominal_step_s.is_finite() && nominal_step_s > 0.0) {
            return Err(CaeError::contract("time stepper nominal step must be finite and positive"));
        }
        let h = &options.history;
        if !h.tolerance.is_finite() || h.tolerance <= 0.0 {
            return Err(CaeError::contract("history residual tolerance must be finite and positive"));
        }
        if !h.condition_limit.is_finite() || h.condition_limit <= 1.0 {
            return Err(CaeError::contract("history condition limit must be finite and greater than one"));
        }
        if let Some(p) = h.krylov_policy {
            p.validate()?;
        }
        if h.matrix_free_bootstrap && h.matrix_free_factory.is_none() {
            return Err(CaeError::contract("history exact matrix-free bootstrap requires exact actions"));
        }
        if h.capability_factory.is_some() && h.preconditioner_factory.is_some() {
            return Err(CaeError::contract(
                "history exact Krylov solve must select one provider integration path",
            ));
        }
        if h.capability_factory.is_some() && h.matrix_free_factory.is_some() {
            return Err(CaeError::contract(
                "history exact Krylov solve cannot select two operator capabilities",
            ));
        }
        let criterion = match &h.criterion {
            Some(c) => Arc::clone(c),
            None => ScalarL2Criterion::shared(h.tolerance)?,
        };
        if let Some(bound) = criterion.state_size()
            && bound != state_size
        {
            return Err(CaeError::contract(format!(
                "convergence criterion is bound to {bound} unknowns but the state has {state_size}"
            )));
        }
        Ok(Self {
            identity,
            problem,
            options,
            criterion,
            state_size,
            design_size,
            nominal_step_s,
            samples,
            initial,
            cache: Mutex::new(FactorizationCache::default()),
        })
    }

    #[must_use]
    pub fn options(&self) -> &HistoryStepperOptions {
        &self.options
    }

    #[must_use]
    pub fn factorization_cache_stats(&self) -> FactorizationCacheStats {
        let c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        FactorizationCacheStats { builds: c.builds, hits: c.hits, entries: c.entries.len(), bytes: c.bytes }
    }

    pub fn clear_factorization_cache(&self) {
        let mut c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.entries.clear();
        c.bytes = 0;
    }

    fn block(&self, n: usize) -> CaeResult<ImplicitBlockSystem<[f64]>> {
        let h = &self.options.history;
        let callbacks: Arc<dyn BlockCallbacks<[f64]>> =
            Arc::new(StepCallbacks { problem: Arc::clone(&self.problem), n });
        let capability_factory = h.capability_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let f: crate::implicit_block::CapabilityFactory<[f64]> =
                Arc::new(move |id: &ExactMatrixIdentity, state: &[f64], design: &[f64], prev: &[f64]| {
                    outer(
                        id,
                        HistoryPoint {
                            history_step: n,
                            state,
                            previous_state: prev,
                            design,
                            execution_context: None,
                        },
                    )
                });
            f
        });
        let matrix_free_factory = h.matrix_free_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let f: crate::implicit_block::MatrixFreeFactory<[f64]> =
                Arc::new(move |state: &[f64], design: &[f64], prev: &[f64]| {
                    outer(HistoryPoint {
                        history_step: n,
                        state,
                        previous_state: prev,
                        design,
                        execution_context: None,
                    })
                });
            f
        });
        let preconditioner_factory = h.preconditioner_factory.as_ref().map(|outer| {
            let outer = Arc::clone(outer);
            let f: crate::implicit_block::PreconditionerFactory<[f64]> = Arc::new(
                move |lease: &SealedMatrixReadLease,
                      binding: &ExactPreconditionerBinding,
                      state: &[f64],
                      design: &[f64],
                      prev: &[f64]| {
                    outer(
                        lease,
                        binding,
                        HistoryPoint {
                            history_step: n,
                            state,
                            previous_state: prev,
                            design,
                            execution_context: None,
                        },
                    )
                },
            );
            f
        });
        ImplicitBlockSystem::new(
            callbacks,
            BlockOptions {
                local_elimination_partition: None,
                rejected_state_capture: None,
                rejected_state_context: None,
                tolerance: h.tolerance,
                max_iterations: h.max_iterations,
                condition_limit: h.condition_limit,
                krylov_policy: h.krylov_policy,
                capability_factory,
                matrix_free_factory,
                matrix_free_bootstrap: h.matrix_free_bootstrap,
                preconditioner_factory,
                lease_budget: h.lease_budget,
                authority_policy_sha256: h.authority_policy_sha256.clone(),
                authority_operation_sha256: h.authority_operation_sha256.clone(),
                authority_provider_profile_sha256: h.authority_provider_profile_sha256.clone(),
                execution_context: None,
                trace_context: None,
                criterion: Some(Arc::clone(&self.criterion)),
                residual_partition: h.residual_partition.clone(),

                relaxed_tolerance: None,
            },
        )
    }

    fn check_pair(&self, n: usize, previous: &[f64], current: &[f64]) -> CaeResult<()> {
        if n == 0 {
            return Err(CaeError::contract("time steps are numbered from 1"));
        }
        check_vector(previous, self.state_size, "time stepper previous state")?;
        check_vector(current, self.state_size, "time stepper current state")
    }

    fn cache_key(n: usize, current: &[f64], previous: &[f64], x: &[f64]) -> [u8; 32] {
        let mut d = Sha256::new();
        d.update(b"implexity-history-step-jacobian/1\0");
        d.update((n as u64).to_le_bytes());
        for part in [current, previous, x] {
            d.update((part.len() as u64).to_le_bytes());
            for v in part {
                d.update(v.to_bits().to_le_bytes());
            }
        }
        d.finalize().into()
    }

    fn factorization(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        x: &[f64],
    ) -> CaeResult<Arc<Factorization>> {
        let key = Self::cache_key(n, current, previous, x);
        let budget = self.options.factorization_cache_bytes;
        {
            let mut c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(pos) = c.entries.iter().position(|(k, _)| *k == key)
                && let Some(entry) = c.entries.remove(pos)
            {
                let f = Arc::clone(&entry.1);
                c.entries.push_back(entry);
                c.hits += 1;
                return Ok(f);
            }
        }
        let residual = self.problem.residual(n, current, previous, x)?;
        check_vector(&residual, self.state_size, "history step residual").map_err(|_| {
            CaeError::contract(format!("history step {n} residual is invalid at the given states"))
        })?;
        let report = self.criterion.assess(&residual, None)?;
        if !report.certified {
            return Err(CaeError::contract(format!(
                "time stepper {}: step {n} is not converged at the given states; derivatives of an implicit step need its converged state",
                self.identity
            )));
        }
        let matrix = self.problem.state_jacobian(n, current, previous, x)?;
        let factorization = Arc::new(Factorization::new(
            matrix,
            self.state_size,
            self.options.history.condition_limit,
            None,
        )?);
        let bytes = factorization.retained_bytes() as u64;
        let mut c = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.builds += 1;
        if bytes <= budget {
            while c.bytes + bytes > budget {
                match c.entries.pop_front() {
                    Some((_, old)) => c.bytes -= old.retained_bytes() as u64,
                    None => break,
                }
            }
            c.bytes += bytes;
            c.entries.push_back((key, Arc::clone(&factorization)));
        }
        Ok(factorization)
    }

    fn previous_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        x: &[f64],
    ) -> CaeResult<Jacobian> {
        checked_matrix(
            self.problem.previous_jacobian(n, current, previous, x)?,
            (self.state_size, self.state_size),
            "history previous-state Jacobian",
            true,
        )
    }

    fn design_jacobian(&self, n: usize, current: &[f64], previous: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        checked_matrix(
            self.problem.design_jacobian(n, current, previous, x)?,
            (self.state_size, x.len()),
            "history design Jacobian",
            true,
        )
    }

    fn implicit_pullbacks(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        x: &[f64],
        sources: &[Vec<f64>],
    ) -> CaeResult<Vec<(Vec<f64>, Vec<f64>)>> {
        let m = sources.len();
        if m == 0 {
            return Ok(Vec::new());
        }
        let nz = self.state_size;
        let factorization = self.factorization(n, current, previous, x)?;
        let mut rhs = vec![0.0; nz * m];
        for (r, s) in sources.iter().enumerate() {
            for (i, v) in s.iter().enumerate() {
                rhs[i * m + r] = *v;
            }
        }
        let solved = factorization.solve_block(&rhs, m, true)?;
        let b = self.previous_jacobian(n, current, previous, x)?;
        let c = self.design_jacobian(n, current, previous, x)?;
        let mut out = Vec::with_capacity(m);
        let mut w = vec![0.0; nz];
        for r in 0..m {
            for (i, wi) in w.iter_mut().enumerate() {
                *wi = solved.solution[i * m + r];
            }
            let mut prev_bar = b.apply(&w, true)?;
            let mut x_bar = c.apply(&w, true)?;
            for v in prev_bar.iter_mut().chain(x_bar.iter_mut()) {
                *v = -*v;
            }
            out.push((prev_bar, x_bar));
        }
        Ok(out)
    }
}

impl TimeStepper for HistoryProblemStepper {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn state_size(&self) -> usize {
        self.state_size
    }
    fn design_size(&self) -> usize {
        self.design_size
    }
    fn sample_names(&self) -> &[String] {
        self.samples.names()
    }
    fn nominal_step_s(&self) -> f64 {
        self.nominal_step_s
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        check_vector(design, self.design_size, "time stepper design")?;
        let z0 = self.initial.state(design)?;
        check_vector(&z0, self.state_size, "time stepper initial state")?;
        Ok(z0)
    }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        check_vector(design, self.design_size, "time stepper design")?;
        check_vector(cotangent, self.state_size, "initial-state cotangent")?;
        let bar = self.initial.vjp(design, cotangent)?;
        check_vector(&bar, self.design_size, "initial-state design cotangent")?;
        Ok(bar)
    }

    fn advance(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> CaeResult<StepRecord> {
        if n == 0 {
            return Err(CaeError::contract("time steps are numbered from 1"));
        }
        check_parameters(self, p)?;
        check_vector(previous, self.state_size, "time stepper previous state")?;
        let x = self.options.time_scale.parameters(p, &self.identity)?;
        let block = self.block(n)?;
        let result = match block.solve(&x, previous, previous) {
            Ok(r) => r,
            Err(e) => {
                block.discard_staged_exact_factorization();
                return Err(e);
            }
        };
        if result.state.len() != self.state_size || result.state.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract(format!(
                "history step {n} returned an invalid state shape or values"
            )));
        }
        if !result.converged || !result.residual_norm.is_finite() {
            return Err(CaeError::contract(format!(
                "history step {n} did not return an exactly converged result"
            )));
        }
        if self.criterion.is_scalar() && result.residual_norm > self.options.history.tolerance {
            return Err(CaeError::contract(format!(
                "history step {n} did not satisfy the configured residual tolerance"
            )));
        }
        let samples = self.samples.samples(n, &result.state, previous, p)?;
        check_vector(&samples, self.samples.names().len(), "time stepper samples")?;
        Ok(StepRecord {
            samples,
            ledger: BTreeMap::new(),
            diagnostics: StepDiagnostics {
                newton_iterations: result.iterations,
                coupling_iterations: 0,
                krylov_iterations: 0,
                residual_norm: result.residual_norm,
            },
            state: result.state,
        })
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
        self.check_pair(n, previous, current)?;
        check_parameters(self, p)?;
        check_vector(d_previous, self.state_size, "time stepper previous-state direction")?;
        let x = self.options.time_scale.parameters(p, &self.identity)?;
        let dx =
            self.options.time_scale.direction(self.design_size, d_design, d_time_scale, &self.identity)?;
        let factorization = self.factorization(n, current, previous, &x)?;
        let mut rhs = self.previous_jacobian(n, current, previous, &x)?.apply(d_previous, false)?;
        if dx.iter().any(|v| *v != 0.0) {
            let c = self.design_jacobian(n, current, previous, &x)?.apply(&dx, false)?;
            for (r, v) in rhs.iter_mut().zip(&c) {
                *r += v;
            }
        }
        let (mut dz, _, _) = factorization.solve(&rhs, false)?;
        for v in &mut dz {
            *v = -*v;
        }
        let samples =
            self.samples.tangent(n, current, previous, p, &dz, d_previous, d_design, d_time_scale)?;
        check_vector(&samples, self.samples.names().len(), "time stepper sample tangent")?;
        Ok(StepTangent { state: dz, samples })
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
            self.adjoint_many(n, previous, current, p, &[state_bar.to_vec()], &[sample_bar.to_vec()])?;
        out.pop().ok_or_else(|| CaeError::contract("time stepper adjoint returned no cotangent"))
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
        self.check_pair(n, previous, current)?;
        check_parameters(self, p)?;
        if state_bars.len() != sample_bars.len() {
            return Err(CaeError::contract(
                "time stepper adjoint batch has unequal state and sample cotangents",
            ));
        }
        let x = self.options.time_scale.parameters(p, &self.identity)?;
        let mut sources = Vec::with_capacity(state_bars.len());
        let mut sample_parts = Vec::with_capacity(state_bars.len());
        for (state_bar, sample_bar) in state_bars.iter().zip(sample_bars) {
            check_vector(state_bar, self.state_size, "time stepper state cotangent")?;
            check_vector(sample_bar, self.samples.names().len(), "time stepper sample cotangent")?;
            let part = self.samples.vjp(n, current, previous, p, sample_bar)?;
            part.check(self.state_size, self.design_size)?;
            let mut w = state_bar.clone();
            add_part(&mut w, &part.current);
            sources.push(w);
            sample_parts.push(part);
        }
        let pulled = self.implicit_pullbacks(n, current, previous, &x, &sources)?;
        Ok(pulled
            .into_iter()
            .zip(sample_parts)
            .map(|((mut prev_bar, x_bar), part)| {
                let (mut design, mut time_scale) = self.options.time_scale.split(x_bar);
                add_part(&mut prev_bar, &part.previous);
                add_part(&mut design, &part.design);
                time_scale += part.time_scale;
                StepCotangent { previous: prev_bar, design, time_scale }
            })
            .collect())
    }
}

pub struct ScanStepper<S> {
    identity: String,
    step: S,
    state_size: usize,
    design_size: usize,
    nominal_step_s: f64,
    samples: Arc<dyn SampleMap>,
    initial: Arc<dyn InitialStateMap>,
    time_scale: TimeScaleMode,
}

impl<S> std::fmt::Debug for ScanStepper<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanStepper")
            .field("identity", &self.identity)
            .field("state_size", &self.state_size)
            .field("design_size", &self.design_size)
            .field("time_scale", &self.time_scale)
            .finish_non_exhaustive()
    }
}

impl<S: ScanStep + Send + Sync> ScanStepper<S> {


    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: impl Into<String>,
        step: S,
        state_size: usize,
        design_size: usize,
        nominal_step_s: f64,
        samples: Arc<dyn SampleMap>,
        initial: Arc<dyn InitialStateMap>,
        time_scale: TimeScaleMode,
    ) -> CaeResult<Self> {
        let identity = identity.into();
        if identity.is_empty() {
            return Err(CaeError::contract("time stepper identity must be nonempty"));
        }
        if state_size == 0 {
            return Err(CaeError::contract("time stepper state must be nonempty"));
        }
        if !(nominal_step_s.is_finite() && nominal_step_s > 0.0) {
            return Err(CaeError::contract("time stepper nominal step must be finite and positive"));
        }
        Ok(Self { identity, step, state_size, design_size, nominal_step_s, samples, initial, time_scale })
    }

    #[must_use]
    pub fn step(&self) -> &S {
        &self.step
    }

    fn check_pair(&self, n: usize, previous: &[f64], current: &[f64]) -> CaeResult<()> {
        if n == 0 {
            return Err(CaeError::contract("time steps are numbered from 1"));
        }
        check_vector(previous, self.state_size, "time stepper previous state")?;
        check_vector(current, self.state_size, "time stepper current state")
    }
}

impl<S: ScanStep + Send + Sync> TimeStepper for ScanStepper<S> {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn state_size(&self) -> usize {
        self.state_size
    }
    fn design_size(&self) -> usize {
        self.design_size
    }
    fn sample_names(&self) -> &[String] {
        self.samples.names()
    }
    fn nominal_step_s(&self) -> f64 {
        self.nominal_step_s
    }
    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        check_vector(design, self.design_size, "time stepper design")?;
        let z0 = self.initial.state(design)?;
        check_vector(&z0, self.state_size, "time stepper initial state")?;
        Ok(z0)
    }
    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        check_vector(design, self.design_size, "time stepper design")?;
        check_vector(cotangent, self.state_size, "initial-state cotangent")?;
        let bar = self.initial.vjp(design, cotangent)?;
        check_vector(&bar, self.design_size, "initial-state design cotangent")?;
        Ok(bar)
    }

    fn advance(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> CaeResult<StepRecord> {
        if n == 0 {
            return Err(CaeError::contract("time steps are numbered from 1"));
        }
        check_parameters(self, p)?;
        check_vector(previous, self.state_size, "time stepper previous state")?;
        let x = self.time_scale.parameters(p, &self.identity)?;
        let state = self.step.step(n - 1, previous, &x).map_err(|e| ad_error(&e))?;
        if state.len() != self.state_size || state.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::convergence(format!(
                "time stepper {} step {n} produced an invalid state (length {} or non-finite values)",
                self.identity,
                state.len()
            )));
        }
        let samples = self.samples.samples(n, &state, previous, p)?;
        check_vector(&samples, self.samples.names().len(), "time stepper samples")?;
        Ok(StepRecord { state, samples, ledger: BTreeMap::new(), diagnostics: StepDiagnostics::default() })
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
        self.check_pair(n, previous, current)?;
        check_parameters(self, p)?;
        check_vector(d_previous, self.state_size, "time stepper previous-state direction")?;
        let x = self.time_scale.parameters(p, &self.identity)?;
        let dx = self.time_scale.direction(self.design_size, d_design, d_time_scale, &self.identity)?;
        let dz = self.step.jvp(n - 1, previous, &x, d_previous, &dx).map_err(|e| ad_error(&e))?;
        check_vector(&dz, self.state_size, "scan step tangent")?;
        let samples =
            self.samples.tangent(n, current, previous, p, &dz, d_previous, d_design, d_time_scale)?;
        check_vector(&samples, self.samples.names().len(), "time stepper sample tangent")?;
        Ok(StepTangent { state: dz, samples })
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
        self.check_pair(n, previous, current)?;
        check_parameters(self, p)?;
        check_vector(state_bar, self.state_size, "time stepper state cotangent")?;
        check_vector(sample_bar, self.samples.names().len(), "time stepper sample cotangent")?;
        let x = self.time_scale.parameters(p, &self.identity)?;
        let part = self.samples.vjp(n, current, previous, p, sample_bar)?;
        part.check(self.state_size, self.design_size)?;
        let pulled = if part.current.is_empty() {
            self.step.vjp(n - 1, previous, &x, state_bar)
        } else {
            let mut w = state_bar.to_vec();
            add_part(&mut w, &part.current);
            self.step.vjp(n - 1, previous, &x, &w)
        };
        let (mut previous_bar, x_bar) = pulled.map_err(|e| ad_error(&e))?;
        check_vector(&previous_bar, self.state_size, "scan step state pullback")?;
        check_vector(&x_bar, x.len(), "scan step parameter pullback")?;
        let (mut design, mut time_scale) = self.time_scale.split(x_bar);
        add_part(&mut previous_bar, &part.previous);
        add_part(&mut design, &part.design);
        time_scale += part.time_scale;
        Ok(StepCotangent { previous: previous_bar, design, time_scale })
    }
}



#[allow(clippy::too_many_arguments)]
pub fn step_duality_defect(
    stepper: &dyn TimeStepper,
    n: usize,
    previous: &[f64],
    current: &[f64],
    p: StepParameters<'_>,
    d_previous: &[f64],
    d_design: &[f64],
    d_time_scale: f64,
    state_bar: &[f64],
    sample_bar: &[f64],
) -> CaeResult<f64> {
    let t = stepper.tangent(n, previous, current, p, d_previous, Some(d_design), d_time_scale)?;
    let c = stepper.adjoint(n, previous, current, p, state_bar, sample_bar)?;
    let lhs = dot(&t.state, state_bar) + dot(&t.samples, sample_bar);
    let rhs = dot(d_previous, &c.previous) + dot(d_design, &c.design) + d_time_scale * c.time_scale;
    Ok(lhs - rhs)
}
