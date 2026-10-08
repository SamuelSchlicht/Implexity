// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use implexity_core::{CaeError, CaeResult};
use implexity_linalg::lu::{Parallelism, SparseLu};
use implexity_solve::factorization::symbolic_for;
use implexity_linalg::sparse::CsrMatrix;
use implexity_solve::matrix::Jacobian;
use implexity_solve::multirate_coupling::{
    FieldJacobians, FluxDrivenField, ResidualAdjointTangent, SecondOrderField,
};
use implexity_solve::time_stepper::{
    SecondOrderStepper, StepCotangent, StepDiagnostics, StepParameters, StepRecord, StepTangent, TimeStepper,
};

use super::observables::Observables;
use super::voxel::VoxelDesignMap;
use crate::soft::model::SoftModel;
use crate::soft::stepper::{FactorizationReuse, Loading, NewtonOptions, Scheme, SoftHistory};
use crate::util::contract;

#[derive(Debug, Clone)]
pub enum DesignLayout {
    ElementParams,
    VoxelDensity(VoxelDesignMap),
}

#[derive(Debug, Clone)]
pub struct SoftStepConfig {
    pub scheme: Scheme,
    pub loading: Loading,
    pub periodic_loading: bool,
    pub rayleigh: (f64, f64),
    pub newton: NewtonOptions,
}

pub const LEDGER_KEYS: [&str; 8] = [
    "kinetic_energy_J",
    "potential_energy_J",
    "external_work_J",
    "support_work_J",
    "damping_dissipation_J",
    "viscous_work_J",
    "quadrature_defect_J",
    "algorithmic_dissipation_J",
];

type HistoryKey = Vec<u64>;

struct ScaledJacobians {
    current: CsrMatrix,
    previous: CsrMatrix,
    design: CsrMatrix,
    flux: CsrMatrix,
    time_scale: Vec<f64>,
}

pub struct SoftStepCore<'m> {
    model: &'m SoftModel,
    config: SoftStepConfig,
    design: DesignLayout,
    observables: Observables,
    names: Vec<String>,
    state_len: usize,
    local_groups: Vec<Vec<usize>>,
    scale: Vec<f64>,
    row: Vec<f64>,
    trace: CsrMatrix,
    dt_nom: f64,
    histories: Mutex<Vec<(HistoryKey, Arc<SoftHistory<'m>>)>>,
    reuse: Option<FactorizationReuse>,
    patterns: Vec<(Vec<f64>, Vec<f64>)>,
}

impl std::fmt::Debug for SoftStepCore<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftStepCore")
            .field("scheme", &self.config.scheme)
            .field("state_len", &self.state_len)
            .field("design", &self.design)
            .field("samples", &self.names)
            .finish_non_exhaustive()
    }
}

fn bits(values: &[f64]) -> impl Iterator<Item = u64> + '_ {
    values.iter().map(|v| v.to_bits())
}

impl<'m> SoftStepCore<'m> {

    pub fn new(
        model: &'m SoftModel,
        config: SoftStepConfig,
        design: DesignLayout,
        observables: Observables,
    ) -> Result<Self, CaeError> {
        let times = &config.loading.times;
        let steps = config.loading.steps();
        if steps == 0 {
            return contract("the soft-solid field needs at least one loading step");
        }
        let dt_nom = times[1] - times[0];
        if times.windows(2).any(|w| ((w[1] - w[0]) - dt_nom).abs() > 1e-12 * dt_nom) {
            return contract("the soft-solid field needs uniform steps (macro steps of equal length)");
        }
        if let DesignLayout::VoxelDensity(map) = &design
            && map.elements() != model.ne()
        {
            return contract("the voxel design map must cover the model's tetrahedra");
        }
        let probe = SoftHistory::new(
            model,
            config.scheme,
            config.loading.clone(),
            config.rayleigh,
            config.newton,
            [vec![1.0; model.ne()], vec![0.0; model.ne()]].concat(),
        )?;
        let l = probe.layout;
        let mut local_groups = Vec::with_capacity(l.ne_visc + l.n3);
        for e in 0..l.ne_visc {
            let mut group: Vec<usize> = (l.s() + 6 * e..l.s() + 6 * e + 6).collect();
            group.extend(l.q() + 6 * e * l.nb..l.q() + 6 * (e + 1) * l.nb);
            local_groups.push(group);
        }
        for i in 0..l.n3 {
            local_groups.push(vec![l.v() + i, l.a() + i]);
        }
        let mu = model.materials.iter().map(|m| m.mu0).fold(0.0_f64, f64::max);
        #[allow(clippy::cast_precision_loss)]
        let length = (6.0 * model.mesh.volumes.iter().sum::<f64>() / model.ne() as f64).cbrt();
        if !(mu.is_finite() && mu > 0.0 && length.is_finite() && length > 0.0) {
            return contract("the soft-solid field needs positive shear moduli and element volumes");
        }
        let k = mu * length;
        let mut scale = vec![1.0; l.len()];
        let mut row = vec![1.0; l.len()];
        for i in 0..l.n3 {
            scale[l.v() + i] = dt_nom;
            scale[l.a() + i] = dt_nom * dt_nom;
            scale[l.r() + i] = 1.0 / k;
            row[i] = if model.fixed[i] { 1.0 } else { 1.0 / k };
            row[l.v() + i] = dt_nom;
            row[l.a() + i] = dt_nom * dt_nom;
            row[l.r() + i] = 1.0 / k;
        }
        for a in 0..l.np {
            scale[l.p() + a] = length / mu;
            row[l.p() + a] = 1.0 / (length * length);
        }
        for i in l.s()..l.len() {
            scale[i] = length / mu;
            row[i] = length / mu;
        }
        let trace = CsrMatrix::from_triplets(
            l.n3,
            l.len(),
            &(0..l.n3).collect::<Vec<_>>(),
            &(0..l.n3).map(|i| l.u() + i).collect::<Vec<_>>(),
            &vec![1.0; l.n3],
        )
        .map_err(|e| CaeError::contract(format!("soft-solid trace operator: {e}")))?;
        let names = observables.names().to_vec();
        Ok(Self {
            model,
            config,
            design,
            observables,
            names,
            state_len: l.len(),
            local_groups,
            scale,
            row,
            trace,
            dt_nom,
            histories: Mutex::new(Vec::new()),
            reuse: None,
            patterns: Vec::new(),
        })
    }


    pub fn with_prescribed_patterns(mut self, patterns: Vec<(Vec<f64>, Vec<f64>)>) -> Result<Self, CaeError> {

        SoftHistory::new(
            self.model,
            self.config.scheme,
            self.config.loading.clone(),
            self.config.rayleigh,
            self.config.newton,
            [vec![1.0; self.model.ne()], vec![0.0; self.model.ne()]].concat(),
        )?
        .with_prescribed_patterns(patterns.clone())?;
        self.patterns = patterns;
        if let Ok(mut cache) = self.histories.lock() {
            cache.clear();
        }
        Ok(self)
    }


    pub fn with_factorization_reuse(mut self, reuse: Option<FactorizationReuse>) -> Result<Self, CaeError> {
        if let Some(r) = reuse {
            r.validate()?;
        }
        self.reuse = reuse;
        if let Ok(mut cache) = self.histories.lock() {
            cache.clear();
        }
        Ok(self)
    }

    #[must_use]
    pub fn model(&self) -> &'m SoftModel {
        self.model
    }

    pub fn local_elimination_groups(&self) -> &[Vec<usize>] { &self.local_groups }

    #[must_use]
    pub fn scale(&self) -> &[f64] {
        &self.scale
    }

    #[must_use]
    pub fn to_physical(&self, scaled: &[f64]) -> Vec<f64> {
        scaled.iter().zip(&self.scale).map(|(z, s)| z / s).collect()
    }

    #[must_use]
    pub fn to_scaled(&self, physical: &[f64]) -> Vec<f64> {
        physical.iter().zip(&self.scale).map(|(z, s)| z * s).collect()
    }

    #[must_use]
    pub fn design_size(&self) -> usize {
        match &self.design {
            DesignLayout::ElementParams => 2 * self.model.ne(),
            DesignLayout::VoxelDensity(map) => map.voxels(),
        }
    }


    pub fn params(&self, design: &[f64]) -> Result<Vec<f64>, CaeError> {
        if design.len() != self.design_size() {
            return contract("soft-solid design vector has the wrong length");
        }
        Ok(match &self.design {
            DesignLayout::ElementParams => design.to_vec(),
            DesignLayout::VoxelDensity(map) => [map.forward(design)?, vec![0.0; self.model.ne()]].concat(),
        })
    }

    fn params_pullback(&self, params_bar: &[f64]) -> Result<Vec<f64>, CaeError> {
        match &self.design {
            DesignLayout::ElementParams => Ok(params_bar.to_vec()),
            DesignLayout::VoxelDensity(map) => map.pullback(&params_bar[..self.model.ne()]),
        }
    }

    fn params_compose(&self, jx: &CsrMatrix) -> Result<CsrMatrix, CaeError> {
        match &self.design {
            DesignLayout::ElementParams => Ok(jx.clone()),
            DesignLayout::VoxelDensity(map) => {
                let ne = self.model.ne();
                let owner = map.owner();
                let (mut r, mut c, mut v) = (Vec::new(), Vec::new(), Vec::new());
                for i in 0..jx.nrows() {
                    let (cols, vals) = jx.row(i);
                    for (j, val) in cols.iter().zip(vals) {
                        if *j < ne {
                            r.push(i);
                            c.push(owner[*j]);
                            v.push(*val);
                        }
                    }
                }
                CsrMatrix::from_triplets(jx.nrows(), map.voxels(), &r, &c, &v)
                    .map_err(|e| CaeError::contract(format!("soft-solid design Jacobian: {e}")))
            }
        }
    }


    pub fn loading_step(&self, n: usize) -> Result<usize, CaeError> {
        let steps = self.config.loading.steps();
        if n == 0 {
            return contract("soft-solid steps are numbered from 1");
        }
        if self.config.periodic_loading {
            Ok((n - 1) % steps)
        } else if n <= steps {
            Ok(n - 1)
        } else {
            contract(format!("soft-solid step {n} lies beyond the {steps} loading steps"))
        }
    }


    pub fn history(&self, p: StepParameters<'_>) -> Result<Arc<SoftHistory<'m>>, CaeError> {
        let params = self.params(p.design)?;
        let key: HistoryKey = std::iter::once(p.time_scale.to_bits()).chain(bits(&params)).collect();
        if let Ok(cache) = self.histories.lock()
            && let Some((_, h)) = cache.iter().find(|(k, _)| *k == key)
        {
            return Ok(Arc::clone(h));
        }
        let mut h = SoftHistory::new(
            self.model,
            self.config.scheme,
            self.config.loading.clone(),
            self.config.rayleigh,
            self.config.newton,
            params,
        )?
        .with_time_scale(p.time_scale)?
        .with_factorization_reuse(self.reuse)?
        .with_prescribed_patterns(self.patterns.clone())?;
        if self.config.periodic_loading {
            h = h.with_periodic_loading();
        }
        h.set_cache_capacity(1);
        let h = Arc::new(h);
        if let Ok(mut cache) = self.histories.lock() {
            cache.push((key, Arc::clone(&h)));
            if cache.len() > 2 {
                cache.remove(0);
            }
        }
        Ok(h)
    }

    fn check(&self, v: &[f64], what: &str) -> Result<(), CaeError> {
        if v.len() == self.state_len {
            Ok(())
        } else {
            contract(format!("soft-solid {what} has length {} instead of {}", v.len(), self.state_len))
        }
    }


    pub fn initial_state(&self, design: &[f64]) -> Result<Vec<f64>, CaeError> {
        let h = self.history(StepParameters { design, time_scale: 1.0 })?;
        Ok(self.to_scaled(&h.initial_state()?))
    }


    pub fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> Result<Vec<f64>, CaeError> {
        self.check(cotangent, "initial-state cotangent")?;
        let h = self.history(StepParameters { design, time_scale: 1.0 })?;
        let x0 = h.initial_state()?;
        let lambda: Vec<f64> = cotangent.iter().zip(&self.scale).map(|(c, s)| c * s).collect();
        self.params_pullback(&h.initial_state_vjp(&x0, &lambda)?)
    }


    pub fn residual(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Result<Vec<f64>, CaeError> {
        self.check(current, "state")?;
        self.check(previous, "previous state")?;
        let t = self.loading_step(n)?;
        let h = self.history(p)?;
        let r = h.state_residual(t, &self.to_physical(previous), &self.to_physical(current), flux)?;
        Ok(r.iter().zip(&self.row).map(|(a, b)| a * b).collect())
    }

    fn jacobians(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Result<ScaledJacobians, CaeError> {
        self.check(current, "state")?;
        self.check(previous, "previous state")?;
        let t = self.loading_step(n)?;
        let h = self.history(p)?;
        let j = h.state_jacobians(t, &self.to_physical(previous), &self.to_physical(current), flux)?;
        let inv: Vec<f64> = self.scale.iter().map(|s| 1.0 / s).collect();
        let err = |e: implexity_linalg::LinalgError| CaeError::contract(format!("soft-solid Jacobian: {e}"));
        let ones = |k: usize| vec![1.0; k];
        let design = self.params_compose(&j.params)?;
        let nd = design.ncols();
        Ok(ScaledJacobians {
            current: j.current.scaled(&self.row, &inv).map_err(err)?,
            previous: j.previous.scaled(&self.row, &inv).map_err(err)?,
            design: design.scaled(&self.row, &ones(nd)).map_err(err)?,
            flux: j.extra.scaled(&self.row, &ones(j.extra.ncols())).map_err(err)?,
            time_scale: j.dt.iter().zip(&self.row).map(|(d, r)| d * r * self.dt_nom).collect(),
        })
    }

    fn current_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<CsrMatrix> {
        self.check(current, "state")?;
        self.check(previous, "previous state")?;
        let h = self.history(p)?;
        let j = h.state_current_jacobian(
            self.loading_step(n)?,
            &self.to_physical(previous),
            &self.to_physical(current),
            Some(flux),
        )?;
        let inv: Vec<f64> = self.scale.iter().map(|s| 1.0 / s).collect();
        j.scaled(&self.row, &inv).map_err(|e| CaeError::contract(format!("soft-solid Jacobian: {e}")))
    }

    fn current_flux_jacobians(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>) -> CaeResult<(CsrMatrix, CsrMatrix)> {
        let current = self.current_jacobian(n, current, previous, flux, p)?;
        let j = self.history(p)?.state_flux_jacobian()?;
        let flux = j.scaled(&self.row, &vec![1.0; j.ncols()]).map_err(|e| CaeError::contract(format!("soft-solid Jacobian: {e}")))?;
        Ok((current, flux))
    }

    fn params_direction(&self, d_design: &[f64]) -> Result<Vec<f64>, CaeError> {
        if d_design.len() != self.design_size() {
            return contract("soft-solid design direction has the wrong length");
        }
        Ok(match &self.design {
            DesignLayout::ElementParams => d_design.to_vec(),
            DesignLayout::VoxelDensity(map) => [map.forward(d_design)?, vec![0.0; self.model.ne()]].concat(),
        })
    }


    #[allow(clippy::too_many_arguments)]
    pub fn residual_adjoint_tangent(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: Option<&[f64]>,
        p: StepParameters<'_>,
        w: &[f64],
        d_current: &[f64],
        d_previous: &[f64],
        d_design: Option<&[f64]>,
    ) -> Result<ResidualAdjointTangent, CaeError> {
        self.check(current, "state")?;
        self.check(previous, "previous state")?;
        self.check(w, "residual cotangent")?;
        self.check(d_current, "state direction")?;
        self.check(d_previous, "previous-state direction")?;
        let t = self.loading_step(n)?;
        let h = self.history(p)?;
        let wr: Vec<f64> = w.iter().zip(&self.row).map(|(a, b)| a * b).collect();
        let dparams = d_design.map(|d| self.params_direction(d)).transpose()?;
        let rt = h.state_adjoint_tangent(
            t,
            &self.to_physical(previous),
            &self.to_physical(current),
            flux,
            &wr,
            &self.to_physical(d_current),
            &self.to_physical(d_previous),
            dparams.as_deref(),
        )?;
        Ok(ResidualAdjointTangent {
            current: self.to_physical(&rt.current),
            previous: self.to_physical(&rt.previous),
            flux: rt.extra,
            design: self.params_pullback(&rt.params)?,
            time_scale: rt.dt * self.dt_nom,
        })
    }


    pub fn samples(&self, current: &[f64], p: StepParameters<'_>) -> Result<Vec<f64>, CaeError> {
        self.check(current, "state")?;
        let h = self.history(p)?;
        self.observables.values(&h, &self.to_physical(current))
    }


    pub fn samples_vjp(
        &self,
        current: &[f64],
        p: StepParameters<'_>,
        bar: &[f64],
    ) -> Result<(Vec<f64>, Vec<f64>), CaeError> {
        self.check(current, "state")?;
        if bar.iter().all(|v| *v == 0.0) {
            return Ok((vec![0.0; self.state_len], vec![0.0; self.design_size()]));
        }
        let h = self.history(p)?;
        let (xs, ps) = self.observables.vjp(&h, &self.to_physical(current), bar)?;
        Ok((xs.iter().zip(&self.scale).map(|(a, s)| a / s).collect(), self.params_pullback(&ps)?))
    }


    pub fn predict(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> Result<Vec<f64>, CaeError> {
        self.check(previous, "previous state")?;
        let t = self.loading_step(n)?;
        let h = self.history(p)?;
        let x = self.to_physical(previous);
        let l = h.layout;
        let m = self.model;
        let dt = h.step_length(t);
        let quad = if matches!(self.config.scheme, Scheme::Newmark { .. } | Scheme::GeneralizedAlpha { .. }) {
            0.5 * dt * dt
        } else {
            0.0
        };
        let mut y = vec![0.0; m.n_unknowns];
        for i in 0..l.n3 {
            if m.unknown[i] != usize::MAX {
                y[m.unknown[i]] = x[l.u() + i] + dt * x[l.v() + i] + quad * x[l.a() + i];
            }
        }
        for a in 0..l.np {
            y[m.unknown[l.n3 + a]] = x[l.p() + a];
        }
        Ok(self.to_scaled(&h.state_from_unknowns(t, &x, &y, None)?))
    }
}

#[derive(Debug)]
pub struct SoftSolidField<'m> {
    core: SoftStepCore<'m>,
}

impl<'m> SoftSolidField<'m> {
    #[must_use]
    pub fn new(core: SoftStepCore<'m>) -> Self {
        Self { core }
    }

    #[must_use]
    pub fn core(&self) -> &SoftStepCore<'m> {
        &self.core
    }
}

impl FluxDrivenField for SoftSolidField<'_> {
    fn local_elimination_groups(&self) -> CaeResult<Option<Vec<Vec<usize>>>> { let groups=self.core.local_elimination_groups(); Ok(if groups.is_empty(){None}else{Some(groups.to_vec())}) }
    fn admissible_step(&self, _n: usize, current: &[f64], direction: &[f64], previous: &[f64], p: StepParameters<'_>, trial: f64) -> CaeResult<f64> {
        self.core.check(current,"state")?;
        self.core.check(direction,"direction")?;
        self.core.check(previous,"previous state")?;
        let history=self.core.history(p)?;
        let l=history.layout;
        let u=self.core.to_physical(current);
        let du=self.core.to_physical(direction);
        self.core.model.admissible_step(&u[l.u()..l.u()+l.n3],&du[l.u()..l.u()+l.n3],history.params(),trial)
    }

    fn check_trace_path(&self, _n: usize, start: &[f64], end: &[f64], p: StepParameters<'_>) -> CaeResult<()> {
        let n3=3*self.core.model.n();
        if start.len()!=n3 || end.len()!=n3 {return Err(CaeError::contract("invalid solid trace path"));}
        let history=self.core.history(p)?;
        let direction:Vec<f64>=end.iter().zip(start).map(|(a,b)|a-b).collect();
        if self.core.model.admissible_step(start,&direction,history.params(),1.0)?<1.0 {
            return Err(CaeError::contract("solid trace linear surface path is inadmissible"));
        }
        Ok(())
    }

    fn state_size(&self) -> usize {
        self.core.state_len
    }

    fn design_size(&self) -> usize {
        self.core.design_size()
    }

    fn sample_names(&self) -> &[String] {
        &self.core.names
    }

    fn nominal_step_s(&self) -> f64 {
        self.core.dt_nom
    }

    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.initial_state(design)
    }

    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.initial_state_vjp(design, cotangent)
    }

    fn trace_operator(&self) -> &CsrMatrix {
        &self.core.trace
    }

    fn predict(
        &self,
        n: usize,
        previous: &[f64],
        _before_previous: Option<&[f64]>,
        p: StepParameters<'_>,
    ) -> Vec<f64> {

        self.core.predict(n, previous, p).unwrap_or_else(|_| previous.to_vec())
    }

    fn residual(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        self.core.residual(n, current, previous, Some(flux), p)
    }

    fn jacobians(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<FieldJacobians> {
        let j = self.core.jacobians(n, current, previous, Some(flux), p)?;
        Ok(FieldJacobians {
            current: Jacobian::Csr(j.current),
            previous: Jacobian::Csr(j.previous),
            flux: Jacobian::Csr(j.flux),
            design: Jacobian::Csr(j.design),
            time_scale: j.time_scale,
        })
    }

    fn current_jacobian(
        &self,
        n: usize,
        current: &[f64],
        previous: &[f64],
        flux: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.core.current_jacobian(n, current, previous, flux, p)?))
    }

    fn current_flux_jacobians(&self, n: usize, current: &[f64], previous: &[f64], flux: &[f64], p: StepParameters<'_>) -> CaeResult<(Jacobian, Jacobian)> {
        let (current, flux) = self.core.current_flux_jacobians(n, current, previous, flux, p)?;
        Ok((Jacobian::Csr(current), Jacobian::Csr(flux)))
    }

    fn samples(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
    ) -> CaeResult<Vec<f64>> {
        self.core.samples(current, p)
    }

    fn samples_vjp(
        &self,
        _n: usize,
        current: &[f64],
        _previous: &[f64],
        p: StepParameters<'_>,
        sample_bar: &[f64],
    ) -> CaeResult<(Vec<f64>, Vec<f64>, Vec<f64>)> {
        let (c, d) = self.core.samples_vjp(current, p, sample_bar)?;
        Ok((c, vec![0.0; self.core.state_len], d))
    }

    fn second_order(&self) -> Option<&dyn SecondOrderField> {
        Some(self)
    }
}

impl SecondOrderField for SoftSolidField<'_> {
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
        _d_flux: &[f64],
        d_design: Option<&[f64]>,
    ) -> CaeResult<ResidualAdjointTangent> {
        self.core.residual_adjoint_tangent(
            n,
            current,
            previous,
            Some(flux),
            p,
            w,
            d_current,
            d_previous,
            d_design,
        )
    }
}

struct Linearized {
    key: Vec<u64>,
    jac: ScaledJacobians,
    lu: SparseLu,
    bytes: usize,
}

pub const DEFAULT_LINEARIZATION_CACHE_BYTES: usize = 256 << 20;

pub struct SoftSolidStepper<'m> {
    core: SoftStepCore<'m>,
    identity: String,
    linearized: Mutex<Vec<Arc<Linearized>>>,
    cache_bytes: usize,
}

impl std::fmt::Debug for SoftSolidStepper<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoftSolidStepper")
            .field("identity", &self.identity)
            .field("core", &self.core)
            .finish()
    }
}

impl<'m> SoftSolidStepper<'m> {

    pub fn new(core: SoftStepCore<'m>, identity: impl Into<String>) -> Result<Self, CaeError> {
        let identity = identity.into();
        if identity.is_empty() {
            return contract("time steppers need a nonempty identity");
        }
        Ok(Self {
            core,
            identity,
            linearized: Mutex::new(Vec::new()),
            cache_bytes: DEFAULT_LINEARIZATION_CACHE_BYTES,
        })
    }

    #[must_use]
    pub fn with_linearization_cache_bytes(mut self, bytes: usize) -> Self {
        self.cache_bytes = bytes;
        self
    }

    #[must_use]
    pub fn core(&self) -> &SoftStepCore<'m> {
        &self.core
    }

    fn linearize(
        &self,
        n: usize,
        previous: &[f64],
        current: &[f64],
        p: StepParameters<'_>,
    ) -> Result<Arc<Linearized>, CaeError> {
        let key: Vec<u64> = [n as u64, p.time_scale.to_bits()]
            .into_iter()
            .chain(bits(p.design))
            .chain(bits(previous))
            .chain(bits(current))
            .collect();
        if let Ok(mut cache) = self.linearized.lock()
            && let Some(pos) = cache.iter().position(|l| l.key == key)
        {
            let lin = cache.remove(pos);
            cache.push(Arc::clone(&lin));
            return Ok(lin);
        }
        let jac = self.core.jacobians(n, current, previous, None, p)?;
        let csc = jac.current.to_csc();
        let lu = symbolic_for(&csc)
            .and_then(|s| s.factor(&csc, Parallelism::Sequential))
            .map_err(|e| CaeError::convergence(format!("soft-solid step Jacobian: {e}")))?;
        let nnz = jac.current.nnz() + jac.previous.nnz() + jac.design.nnz() + jac.flux.nnz();
        let bytes = 16 * nnz + 8 * (key.len() + jac.time_scale.len()) + lu.nbytes();
        let lin = Arc::new(Linearized { key, jac, lu, bytes });
        if let Ok(mut cache) = self.linearized.lock() {
            cache.push(Arc::clone(&lin));
            while cache.len() > 1 && cache.iter().map(|l| l.bytes).sum::<usize>() > self.cache_bytes {
                cache.remove(0);
            }
        }
        Ok(lin)
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

impl TimeStepper for SoftSolidStepper<'_> {
    fn identity(&self) -> &str {
        &self.identity
    }

    fn state_size(&self) -> usize {
        self.core.state_len
    }

    fn design_size(&self) -> usize {
        self.core.design_size()
    }

    fn sample_names(&self) -> &[String] {
        &self.core.names
    }

    fn nominal_step_s(&self) -> f64 {
        self.core.dt_nom
    }

    fn initial_state(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.initial_state(design)
    }

    fn initial_state_vjp(&self, design: &[f64], cotangent: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.initial_state_vjp(design, cotangent)
    }

    fn advance(&self, n: usize, previous: &[f64], p: StepParameters<'_>) -> CaeResult<StepRecord> {
        self.core.check(previous, "previous state")?;
        let t = self.core.loading_step(n)?;
        let h = self.core.history(p)?;
        let x = self.core.to_physical(previous);
        let sol = h.solve_step(t, &x, None, None)?;
        let lg = h.energy_ledger(t, &x, &sol.state, None)?;
        let values = [
            lg.kinetic_end,
            lg.potential_end,
            lg.external_work,
            lg.support_work,
            lg.damping_dissipation,
            lg.viscous_work,
            lg.quadrature_defect,
            lg.algorithmic_dissipation,
        ];
        let ledger: BTreeMap<String, f64> =
            LEDGER_KEYS.iter().zip(values).map(|(k, v)| ((*k).to_string(), v)).collect();
        let state = self.core.to_scaled(&sol.state);
        let samples = self.core.observables.values(&h, &sol.state)?;
        Ok(StepRecord {
            state,
            samples,
            ledger,
            diagnostics: StepDiagnostics {
                newton_iterations: sol.iterations,
                coupling_iterations: 0,
                krylov_iterations: 0,
                residual_norm: sol.free_residual,
            },
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
        self.core.check(d_previous, "previous-state direction")?;
        let lin = self.linearize(n, previous, current, p)?;
        let err = |e: implexity_linalg::LinalgError| CaeError::contract(format!("soft-solid tangent: {e}"));
        let mut b = lin.jac.previous.matvec(d_previous).map_err(err)?;
        if let Some(dx) = d_design {
            if dx.len() != self.core.design_size() {
                return contract("soft-solid design direction has the wrong length");
            }
            for (a, v) in b.iter_mut().zip(lin.jac.design.matvec(dx).map_err(err)?) {
                *a += v;
            }
        }
        if d_time_scale != 0.0 {
            for (a, v) in b.iter_mut().zip(&lin.jac.time_scale) {
                *a += d_time_scale * v;
            }
        }
        let neg: Vec<f64> = b.iter().map(|v| -v).collect();
        let state = lin
            .lu
            .solve(&neg)
            .map_err(|e| CaeError::convergence(format!("soft-solid tangent solve: {e}")))?;
        let ns = self.core.names.len();
        let mut samples = Vec::with_capacity(ns);
        for k in 0..ns {
            let mut e = vec![0.0; ns];
            e[k] = 1.0;
            let (cs, cx) = self.core.samples_vjp(current, p, &e)?;
            samples.push(dot(&cs, &state) + d_design.map_or(0.0, |dx| dot(&cx, dx)));
        }
        Ok(StepTangent { state, samples })
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
        self.adjoint_many(n, previous, current, p, &[state_bar.to_vec()], &[sample_bar.to_vec()])?
            .pop()
            .ok_or_else(|| CaeError::contract("internal: empty adjoint batch"))
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
        if state_bars.len() != sample_bars.len() {
            return contract("soft-solid adjoint batch has unequal state and sample cotangent counts");
        }
        let lin = self.linearize(n, previous, current, p)?;
        let err = |e: implexity_linalg::LinalgError| CaeError::contract(format!("soft-solid adjoint: {e}"));
        let mut out = Vec::with_capacity(state_bars.len());
        for (wz, ws) in state_bars.iter().zip(sample_bars) {
            self.core.check(wz, "state cotangent")?;
            if ws.len() != self.core.names.len() {
                return contract("soft-solid sample cotangent has the wrong length");
            }
            let (cs, cx) = self.core.samples_vjp(current, p, ws)?;
            let rhs: Vec<f64> = wz.iter().zip(&cs).map(|(a, b)| a + b).collect();
            let lambda = lin
                .lu
                .solve_transpose(&rhs)
                .map_err(|e| CaeError::convergence(format!("soft-solid adjoint solve: {e}")))?;
            let previous_bar: Vec<f64> =
                lin.jac.previous.matvec_transpose(&lambda).map_err(err)?.iter().map(|v| -v).collect();
            let mut design: Vec<f64> =
                lin.jac.design.matvec_transpose(&lambda).map_err(err)?.iter().map(|v| -v).collect();
            for (a, b) in design.iter_mut().zip(&cx) {
                *a += b;
            }
            out.push(StepCotangent {
                previous: previous_bar,
                design,
                time_scale: -dot(&lin.jac.time_scale, &lambda),
            });
        }
        Ok(out)
    }

    fn second_order(&self) -> Option<&dyn SecondOrderStepper> {
        Some(self)
    }
}

impl SecondOrderStepper for SoftSolidStepper<'_> {
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
        self.core.check(state_bar, "state cotangent")?;
        self.core.check(d_previous, "previous-state direction")?;
        if sample_bar.len() != self.core.names.len() {
            return contract("soft-solid sample cotangent has the wrong length");
        }
        if sample_bar.iter().any(|v| *v != 0.0) {
            return contract(
                "soft-solid second-order adjoints need zero sample cotangents (the observables publish first \
                 derivatives only)",
            );
        }
        let lin = self.linearize(n, previous, current, p)?;
        let err = |e: implexity_linalg::LinalgError| {
            CaeError::contract(format!("soft-solid second-order adjoint: {e}"))
        };
        let solve = |rhs: &[f64], transpose: bool| {
            if transpose { lin.lu.solve_transpose(rhs) } else { lin.lu.solve(rhs) }
                .map_err(|e| CaeError::convergence(format!("soft-solid second-order solve: {e}")))
        };
        let mut b = lin.jac.previous.matvec(d_previous).map_err(err)?;
        if let Some(dx) = d_design {
            if dx.len() != self.core.design_size() {
                return contract("soft-solid design direction has the wrong length");
            }
            for (a, v) in b.iter_mut().zip(lin.jac.design.matvec(dx).map_err(err)?) {
                *a += v;
            }
        }
        let d_current: Vec<f64> = solve(&b, false)?.into_iter().map(|v| -v).collect();
        let lambda = solve(state_bar, true)?;
        let rt = self.core.residual_adjoint_tangent(
            n, current, previous, None, p, &lambda, &d_current, d_previous, d_design,
        )?;
        let d_lambda: Vec<f64> = solve(&rt.current, true)?.into_iter().map(|v| -v).collect();
        let jp = lin.jac.previous.matvec_transpose(&d_lambda).map_err(err)?;
        let jx = lin.jac.design.matvec_transpose(&d_lambda).map_err(err)?;
        Ok(StepCotangent {
            previous: rt.previous.iter().zip(&jp).map(|(a, b)| -a - b).collect(),
            design: rt.design.iter().zip(&jx).map(|(a, b)| -a - b).collect(),
            time_scale: -rt.time_scale - dot(&lin.jac.time_scale, &d_lambda),
        })
    }
}
