// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_ad::forward::gradient;
use implexity_ad::{Dual, Scalar};

use crate::model_errors::PhysicsError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FieldSolverError(pub String);

impl From<FieldSolverError> for PhysicsError {
    fn from(e: FieldSolverError) -> Self {
        PhysicsError::Value(e.0)
    }
}

type Result<T> = std::result::Result<T, FieldSolverError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(FieldSolverError(message.into()))
}

pub trait Response {
    fn eval<S: Scalar>(&self, state: &[S]) -> S;
}

pub trait ElectrostaticResponse {
    fn uses_derived_fields(&self) -> bool;
    fn eval<S: Scalar>(&self, potential: &[S], current: &[S], joule: &[S]) -> S;
}

pub trait HelmholtzResponse {
    fn eval<S: Scalar>(&self, real: &[S], imag: &[S]) -> S;
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub state: Vec<f64>,
}

fn check_finite(values: &[f64], name: &str) -> Result<()> {
    if values.iter().all(Scalar::is_finite) { Ok(()) } else { fail(format!("{name} must be finite")) }
}

fn vector<S: Scalar>(value: &[S], n: usize, name: &str) -> Result<Vec<S>> {
    let primal: Vec<f64> = value.iter().map(Scalar::value).collect();
    check_finite(&primal, name)?;
    match value.len() {
        1 if n != 1 => Ok(vec![value[0]; n]),
        len if len == n => Ok(value.to_vec()),
        len => fail(format!("{name} must have shape ({n},), got ({len},)")),
    }
}

fn check_topology<S: Scalar>(value: &[S], n: usize) -> Result<Vec<S>> {
    let rho = vector(value, n, "topology")?;
    if rho.iter().any(|r| !(0.0..=1.0).contains(&r.value())) {
        return fail("topology must be finite and lie in [0, 1]; no implicit clipping");
    }
    Ok(rho)
}

fn interpolate<S: Scalar>(rho: &[S], minimum: f64, maximum: f64, penal: f64) -> Vec<S> {
    rho.iter().map(|&r| r.powf(penal) * (maximum - minimum) + minimum).collect()
}

fn interpolate_numpy(rho: &[f64], minimum: f64, maximum: f64, penal: f64) -> (Vec<f64>, Vec<f64>) {
    let value = rho.iter().map(|r| minimum + (maximum - minimum) * r.powf(penal)).collect();
    #[allow(clippy::float_cmp)]
    let at_zero = if penal == 1.0 { 1.0 } else { 0.0 };
    let derivative = rho
        .iter()
        .map(|r| (maximum - minimum) * penal * if *r > 0.0 { r.powf(penal - 1.0) } else { at_zero })
        .collect();
    (value, derivative)
}

fn harmonic_faces<S: Scalar>(values: &[S]) -> Vec<S> {
    values
        .windows(2)
        .map(|w| {
            let (l, r) = (w[0].value(), w[1].value());
            let scale = l.max(r);
            let (a, b) = (l / scale, r / scale);
            let face = l.min(r) / (0.5 + 0.5 * a.min(b));
            let den = a + b;
            S::chain2(w[0], w[1], face, 2.0 * (b / den).powi(2), 2.0 * (a / den).powi(2), 0.0, 0.0, 0.0)
        })
        .collect()
}

fn harmonic_numpy(values: &[f64]) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut faces = Vec::new();
    let mut dl = Vec::new();
    let mut dr = Vec::new();
    for w in values.windows(2) {
        let scale = w[0].max(w[1]);
        let (a, b) = (w[0] / scale, w[1] / scale);
        faces.push(w[0].min(w[1]) / (0.5 + 0.5 * a.min(b)));
        dl.push(2.0 * (b / (a + b)).powi(2));
        dr.push(2.0 * (a / (a + b)).powi(2));
    }
    (faces, dl, dr)
}

fn solve<S: Scalar>(matrix: &[S], rhs: &[S], n: usize) -> Result<Vec<S>> {
    implexity_ad::small::solve(matrix, rhs, n).map_err(|e| FieldSolverError(e.to_string()))
}

fn transpose(matrix: &[f64], n: usize) -> Vec<f64> {
    (0..n * n).map(|ij| matrix[(ij % n) * n + ij / n]).collect()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn matvec<S: Scalar>(m: &[S], x: &[S], n: usize) -> Vec<S> {
    (0..m.len() / n)
        .map(|i| {
            let mut acc = S::zero();
            for j in 0..n {
                acc += m[i * n + j] * x[j];
            }
            acc
        })
        .collect()
}

fn scalar_value(value: f64, name: &str) -> Result<f64> {
    if value.is_finite() { Ok(value) } else { fail(format!("{name} must be a finite real scalar")) }
}

fn response_value_gradient<R: Response>(response: &R, state: &[f64]) -> Result<(f64, Vec<f64>)> {
    gradient::<8, _>(|x: &[Dual<8>]| response.eval(x), state).map_err(|e| FieldSolverError(e.to_string()))
}

fn diffusion_matrix<S: Scalar>(
    coefficient: &[S],
    reaction: &[S],
    dx: f64,
    left: f64,
    right: f64,
    source: &[S],
) -> Result<(Vec<S>, Vec<S>)> {
    let n = coefficient.len();
    let left = scalar_value(left, "left boundary")?;
    let right = scalar_value(right, "right boundary")?;
    let q = vector(source, n, "source")?;
    let c = vector(reaction, n, "reaction")?;
    let faces = harmonic_faces(coefficient);
    let mut matrix = vec![S::zero(); n * n];
    let mut rhs = q;
    matrix[0] = S::one();
    matrix[n * n - 1] = S::one();
    rhs[0] = S::from_f64(left);
    rhs[n - 1] = S::from_f64(right);
    let inv_dx2 = 1.0 / dx.powi(2);
    for i in 1..n - 1 {
        let lower = faces[i - 1] * inv_dx2;
        let upper = faces[i] * inv_dx2;
        matrix[i * n + i - 1] = -lower;
        matrix[i * n + i] = lower + upper + c[i];
        matrix[i * n + i + 1] = -upper;
    }
    Ok((matrix, rhs))
}

fn lift<S: Scalar>(x: &[f64]) -> Vec<S> {
    x.iter().map(|&v| S::from_f64(v)).collect()
}

fn check_scalars(values: &[(&str, f64)], label: &str) -> Result<()> {
    for (name, v) in values {
        if !v.is_finite() {
            return fail(format!("{label} {name} must be a finite real scalar"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub struct SteadyScalarDiffusionSolver {
    pub n: usize,
    pub length: f64,
    pub k_min: f64,
    pub k_max: f64,
    pub penal: f64,
    pub dx: f64,
}

impl SteadyScalarDiffusionSolver {

    pub fn new(n: usize, length: f64, k_min: f64, k_max: f64, penal: f64) -> Result<Self> {
        check_scalars(
            &[("length", length), ("k_min", k_min), ("k_max", k_max), ("penal", penal)],
            "scalar diffusion",
        )?;
        if n < 3 || penal < 1.0 || length <= 0.0 || !(0.0 < k_min && k_min <= k_max) {
            return fail("invalid scalar diffusion discretisation");
        }
        Ok(Self { n, length, k_min, k_max, penal, dx: length / (n - 1) as f64 })
    }


    pub fn with_defaults(n: usize) -> Result<Self> {
        Self::new(n, 1.0, 1e-3, 1.0, 3.0)
    }


    pub fn coefficient<S: Scalar>(&self, topology: &[S]) -> Result<Vec<S>> {
        Ok(interpolate(&check_topology(topology, self.n)?, self.k_min, self.k_max, self.penal))
    }


    pub fn system<S: Scalar>(
        &self,
        topology: &[S],
        source: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
    ) -> Result<(Vec<S>, Vec<S>)> {
        diffusion_matrix(&self.coefficient(topology)?, reaction, self.dx, left, right, source)
    }


    pub fn solve<S: Scalar>(
        &self,
        topology: &[S],
        source: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
    ) -> Result<Vec<S>> {
        let (m, r) = self.system(topology, source, left, right, reaction)?;
        solve(&m, &r, self.n)
    }


    pub fn residual<S: Scalar>(
        &self,
        topology: &[S],
        state: &[S],
        source: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
    ) -> Result<Vec<S>> {
        let (m, r) = self.system(topology, source, left, right, reaction)?;
        let u = vector(state, self.n, "state")?;
        Ok(matvec(&m, &u, self.n).into_iter().zip(r).map(|(a, b)| a - b).collect())
    }


    pub fn response_and_gradient<R: Response>(
        &self,
        topology: &[f64],
        source: &[f64],
        response: &R,
        left: f64,
        right: f64,
        reaction: &[f64],
    ) -> Result<(f64, Vec<f64>)> {
        let n = self.n;
        let rho = topology_vec(topology, n)?;
        let (coefficient, dcoef) = interpolate_numpy(&rho, self.k_min, self.k_max, self.penal);
        let (_, dl, dr) = harmonic_numpy(&coefficient);
        let (matrix, rhs) = diffusion_matrix(
            &coefficient,
            &broadcast(reaction, n)?,
            self.dx,
            left,
            right,
            &broadcast(source, n)?,
        )?;
        let state = solve(&matrix, &rhs, n)?;
        let (value, response_state) = response_value_gradient(response, &state)?;
        let adjoint = solve(&transpose(&matrix, n), &response_state, n)?;
        let inv_dx2 = 1.0 / self.dx.powi(2);
        let interior = |row: usize| 0 < row && row < n - 1;
        let mut grad = vec![0.0; n];
        for j in 0..n {
            let mut rd = vec![0.0; n];
            if j > 0 {
                let df = dr[j - 1] * dcoef[j];
                if interior(j) {
                    rd[j] += df * inv_dx2 * (state[j] - state[j - 1]);
                }
                if interior(j - 1) {
                    rd[j - 1] += df * inv_dx2 * (state[j - 1] - state[j]);
                }
            }
            if j < n - 1 {
                let df = dl[j] * dcoef[j];
                if interior(j) {
                    rd[j] += df * inv_dx2 * (state[j] - state[j + 1]);
                }
                if interior(j + 1) {
                    rd[j + 1] += df * inv_dx2 * (state[j + 1] - state[j]);
                }
            }
            grad[j] = -dot(&adjoint, &rd);
        }
        Ok((value, grad))
    }
}

fn topology_vec(topology: &[f64], n: usize) -> Result<Vec<f64>> {
    let rho = check_topology(topology, n)?;
    if topology.len() != n {
        return fail(format!("topology must have shape ({n},), got ({},)", topology.len()));
    }
    Ok(rho)
}

fn broadcast(values: &[f64], n: usize) -> Result<Vec<f64>> {
    match values.len() {
        1 => Ok(vec![values[0]; n]),
        len if len == n => Ok(values.to_vec()),
        len => fail(format!("operands could not be broadcast together with shapes ({len},) ({n},)")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TransientScalarDiffusionSolver {
    pub steady: SteadyScalarDiffusionSolver,
    pub time_step: f64,
    pub steps: usize,
    pub storage_min: f64,
    pub storage_max: f64,
    pub storage_penal: f64,
}

impl TransientScalarDiffusionSolver {

    pub fn new(
        steady: SteadyScalarDiffusionSolver,
        time_step: f64,
        steps: usize,
        storage_min: f64,
        storage_max: f64,
        storage_penal: f64,
    ) -> Result<Self> {
        let finite = [time_step, storage_min, storage_max, storage_penal].iter().all(Scalar::is_finite);
        if !finite
            || storage_penal < 1.0
            || time_step <= 0.0
            || steps == 0
            || !(0.0 < storage_min && storage_min <= storage_max)
        {
            return fail("invalid transient diffusion settings");
        }
        Ok(Self { steady, time_step, steps, storage_min, storage_max, storage_penal })
    }


    pub fn storage<S: Scalar>(&self, topology: &[S]) -> Result<Vec<S>> {
        Ok(interpolate(
            &check_topology(topology, self.steady.n)?,
            self.storage_min,
            self.storage_max,
            self.storage_penal,
        ))
    }


    pub fn step_system<S: Scalar>(
        &self,
        topology: &[S],
        previous: &[S],
        source: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
    ) -> Result<(Vec<S>, Vec<S>)> {
        let n = self.steady.n;
        let coefficient = self.steady.coefficient(topology)?;
        let storage = self.storage(topology)?;
        let previous = vector(previous, n, "previous state")?;
        let (mut matrix, mut rhs) =
            diffusion_matrix(&coefficient, reaction, self.steady.dx, left, right, source)?;
        for i in 1..n - 1 {
            let mass = storage[i] / self.time_step;
            matrix[i * n + i] += mass;
            rhs[i] += mass * previous[i];
        }
        Ok((matrix, rhs))
    }

    fn sources<S: Scalar>(&self, source: &[S]) -> Result<Vec<Vec<S>>> {
        let n = self.steady.n;
        if source.len() == n {
            let v = vector(source, n, "source")?;
            return Ok(vec![v; self.steps]);
        }
        if source.len() != self.steps * n {
            return fail(format!(
                "source history must have shape ({}, {n}), got ({},)",
                self.steps,
                source.len()
            ));
        }
        let primal: Vec<f64> = source.iter().map(Scalar::value).collect();
        check_finite(&primal, "source history")?;
        Ok(source.chunks(n).map(<[S]>::to_vec).collect())
    }


    pub fn solve<S: Scalar>(
        &self,
        topology: &[S],
        source_history: &[S],
        initial: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
    ) -> Result<Vec<S>> {
        let n = self.steady.n;
        let left = scalar_value(left, "left boundary")?;
        let right = scalar_value(right, "right boundary")?;
        let sources = self.sources(source_history)?;
        let mut previous = vector(initial, n, "initial state")?;
        previous[0] = S::from_f64(left);
        previous[n - 1] = S::from_f64(right);
        let mut history = previous.clone();
        for src in &sources {
            let (m, r) = self.step_system(topology, &previous, src, left, right, reaction)?;
            previous = solve(&m, &r, n)?;
            history.extend_from_slice(&previous);
        }
        Ok(history)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn residual<S: Scalar>(
        &self,
        topology: &[S],
        history: &[S],
        source: &[S],
        left: f64,
        right: f64,
        reaction: &[S],
        initial: &[S],
    ) -> Result<Vec<S>> {
        let n = self.steady.n;
        let left = scalar_value(left, "left boundary")?;
        let right = scalar_value(right, "right boundary")?;
        let primal: Vec<f64> = history.iter().map(Scalar::value).collect();
        check_finite(&primal, "state history")?;
        if history.len() != (self.steps + 1) * n {
            return fail(format!(
                "state history must have shape ({}, {n}), got ({},)",
                self.steps + 1,
                history.len()
            ));
        }
        let sources = self.sources(source)?;
        let mut expected = vector(initial, n, "initial state")?;
        expected[0] = S::from_f64(left);
        expected[n - 1] = S::from_f64(right);
        let mut rows: Vec<S> = history[..n].iter().zip(&expected).map(|(a, b)| *a - *b).collect();
        for (k, src) in sources.iter().enumerate() {
            let (m, r) =
                self.step_system(topology, &history[k * n..(k + 1) * n], src, left, right, reaction)?;
            let applied = matvec(&m, &history[(k + 1) * n..(k + 2) * n], n);
            rows.extend(applied.into_iter().zip(r).map(|(a, b)| a - b));
        }
        Ok(rows)
    }


    #[allow(clippy::too_many_arguments)]
    pub fn response_and_gradient<R: Response>(
        &self,
        topology: &[f64],
        source_history: &[f64],
        response: &R,
        initial: &[f64],
        left: f64,
        right: f64,
        reaction: &[f64],
    ) -> Result<(f64, Vec<f64>)> {
        let rho = check_topology(topology, self.steady.n)?;
        let failure = std::cell::RefCell::new(None);
        let objective = |x: &[Dual<8>]| -> Dual<8> {
            match self.solve(x, &lift(source_history), &lift(initial), left, right, &lift(reaction)) {
                Ok(history) => response.eval(&history),
                Err(e) => {
                    *failure.borrow_mut() = Some(e);
                    Dual::constant(f64::NAN)
                }
            }
        };
        let (value, grad) = gradient::<8, _>(objective, &rho).map_err(|e| FieldSolverError(e.to_string()))?;
        if let Some(e) = failure.into_inner() {
            return Err(e);
        }
        Ok((value, grad))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConservativeAdvectionDiffusionReactionSolver {
    pub n: usize,
    pub length: f64,
    pub d_min: f64,
    pub d_max: f64,
    pub penal: f64,
    pub dx: f64,
}

impl ConservativeAdvectionDiffusionReactionSolver {

    pub fn new(n: usize, length: f64, d_min: f64, d_max: f64, penal: f64) -> Result<Self> {
        let finite = [length, d_min, d_max, penal].iter().all(Scalar::is_finite);
        if n < 2 || !finite || penal < 1.0 || length <= 0.0 || !(0.0 < d_min && d_min <= d_max) {
            return fail("invalid transport discretisation");
        }
        Ok(Self { n, length, d_min, d_max, penal, dx: length / n as f64 })
    }


    pub fn diffusivity<S: Scalar>(&self, topology: &[S]) -> Result<Vec<S>> {
        Ok(interpolate(&check_topology(topology, self.n)?, self.d_min, self.d_max, self.penal))
    }

    fn inputs<S: Scalar>(
        &self,
        source: &[S],
        reaction: &[S],
        velocity: f64,
        inlet: f64,
    ) -> Result<(Vec<S>, Vec<S>)> {
        let q = vector(source, self.n, "source")
            .map_err(|_| FieldSolverError("transport source must be finite".into()))?;
        let c = vector(reaction, self.n, "reaction")
            .map_err(|_| FieldSolverError("transport reaction must be finite".into()))?;
        if !velocity.is_finite() || !inlet.is_finite() {
            return fail("transport velocity and inlet must be finite");
        }
        Ok((q, c))
    }

    fn assemble<S: Scalar>(
        &self,
        diffusion: &[S],
        faces: &[S],
        q: Vec<S>,
        c: &[S],
        velocity: f64,
        inlet: f64,
    ) -> (Vec<S>, Vec<S>) {
        let n = self.n;
        let mut m = vec![S::zero(); n * n];
        for i in 0..n {
            m[i * n + i] = c[i];
        }
        let mut rhs = q;
        let inv_dx = 1.0 / self.dx;
        let inv_dx2 = inv_dx.powi(2);
        for i in 0..n - 1 {
            let g = faces[i] * inv_dx2;
            m[i * n + i] += g;
            m[i * n + i + 1] -= g;
            m[(i + 1) * n + i] -= g;
            m[(i + 1) * n + i + 1] += g;
            if velocity >= 0.0 {
                m[i * n + i] += S::from_f64(velocity * inv_dx);
                m[(i + 1) * n + i] += S::from_f64(-velocity * inv_dx);
            } else {
                m[i * n + i + 1] += S::from_f64(velocity * inv_dx);
                m[(i + 1) * n + i + 1] += S::from_f64(-velocity * inv_dx);
            }
        }
        if velocity >= 0.0 {
            m[0] += diffusion[0] * 2.0 * inv_dx2;
            rhs[0] += (diffusion[0] * 2.0 * inv_dx2 + velocity * inv_dx) * inlet;
            m[n * n - 1] += S::from_f64(velocity * inv_dx);
        } else {
            m[0] += S::from_f64(-velocity * inv_dx);
            m[n * n - 1] += diffusion[n - 1] * 2.0 * inv_dx2;
            rhs[n - 1] += (diffusion[n - 1] * 2.0 * inv_dx2 - velocity * inv_dx) * inlet;
        }
        (m, rhs)
    }


    pub fn system<S: Scalar>(
        &self,
        topology: &[S],
        source: &[S],
        velocity: f64,
        reaction: &[S],
        inlet: f64,
    ) -> Result<(Vec<S>, Vec<S>)> {
        let diffusion = self.diffusivity(topology)?;
        let faces = harmonic_faces(&diffusion);
        let (q, c) = self.inputs(source, reaction, velocity, inlet)?;
        Ok(self.assemble(&diffusion, &faces, q, &c, velocity, inlet))
    }


    pub fn solve<S: Scalar>(
        &self,
        topology: &[S],
        source: &[S],
        velocity: f64,
        reaction: &[S],
        inlet: f64,
    ) -> Result<Vec<S>> {
        let (m, r) = self.system(topology, source, velocity, reaction, inlet)?;
        solve(&m, &r, self.n)
    }


    pub fn residual<S: Scalar>(
        &self,
        topology: &[S],
        state: &[S],
        source: &[S],
        velocity: f64,
        reaction: &[S],
        inlet: f64,
    ) -> Result<Vec<S>> {
        let (m, r) = self.system(topology, source, velocity, reaction, inlet)?;
        let u = vector(state, self.n, "state")?;
        Ok(matvec(&m, &u, self.n).into_iter().zip(r).map(|(a, b)| a - b).collect())
    }


    pub fn response_and_gradient<R: Response>(
        &self,
        topology: &[f64],
        source: &[f64],
        response: &R,
        velocity: f64,
        reaction: &[f64],
        inlet: f64,
    ) -> Result<(f64, Vec<f64>)> {
        let n = self.n;
        let rho = topology_vec(topology, n)?;
        let (diffusion, ddiff) = interpolate_numpy(&rho, self.d_min, self.d_max, self.penal);
        let (faces, dl, dr) = harmonic_numpy(&diffusion);
        let (q, c) = self.inputs(source, reaction, velocity, inlet)?;
        let (matrix, rhs) = self.assemble(&diffusion, &faces, q, &c, velocity, inlet);
        let state = solve(&matrix, &rhs, n)?;
        if !state.iter().all(Scalar::is_finite) {
            return fail("transport solve returned a nonfinite state");
        }
        let (value, rs) = response_value_gradient(response, &state)?;
        if !value.is_finite() || !rs.iter().all(Scalar::is_finite) {
            return fail("transport response or state derivative is nonfinite");
        }
        let adjoint = solve(&transpose(&matrix, n), &rs, n)?;
        let inv_dx2 = 1.0 / self.dx.powi(2);
        let mut grad = vec![0.0; n];
        for j in 0..n {
            let mut rd = vec![0.0; n];
            if j > 0 {
                let dg = dr[j - 1] * ddiff[j] * inv_dx2;
                let diff = state[j] - state[j - 1];
                rd[j] += dg * diff;
                rd[j - 1] -= dg * diff;
            }
            if j < n - 1 {
                let dg = dl[j] * ddiff[j] * inv_dx2;
                let diff = state[j] - state[j + 1];
                rd[j] += dg * diff;
                rd[j + 1] -= dg * diff;
            }
            if velocity >= 0.0 && j == 0 {
                rd[0] += 2.0 * ddiff[0] * inv_dx2 * (state[0] - inlet);
            }
            if velocity < 0.0 && j == n - 1 {
                rd[n - 1] += 2.0 * ddiff[n - 1] * inv_dx2 * (state[n - 1] - inlet);
            }
            grad[j] = -dot(&adjoint, &rd);
        }
        if !grad.iter().all(Scalar::is_finite) {
            return fail("transport adjoint returned a nonfinite gradient");
        }
        Ok((value, grad))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElectrostaticPotentialSolver {
    pub diffusion: SteadyScalarDiffusionSolver,
}

impl ElectrostaticPotentialSolver {

    pub fn derived_fields<S: Scalar>(&self, topology: &[S], potential: &[S]) -> Result<(Vec<S>, Vec<S>)> {
        let faces = harmonic_faces(&self.diffusion.coefficient(topology)?);
        let dx = self.diffusion.dx;
        let e: Vec<S> = potential.windows(2).map(|w| -(w[1] - w[0]) / dx).collect();
        let current: Vec<S> = faces.iter().zip(&e).map(|(f, e)| *f * *e).collect();
        let joule: Vec<S> = current.iter().zip(&e).map(|(c, e)| *c * *e).collect();
        Ok((current, joule))
    }


    pub fn current_and_joule<S: Scalar>(
        &self,
        topology: &[S],
        charge: &[S],
        left_voltage: f64,
        right_voltage: f64,
    ) -> Result<(Vec<S>, Vec<S>, Vec<S>)> {
        let rho = check_topology(topology, self.diffusion.n)?;
        let potential = self.diffusion.solve(&rho, charge, left_voltage, right_voltage, &[S::zero()])?;
        let (current, joule) = self.derived_fields(&rho, &potential)?;
        Ok((potential, current, joule))
    }


    pub fn response_and_gradient<R: ElectrostaticResponse>(
        &self,
        topology: &[f64],
        charge: &[f64],
        response: &R,
        left_voltage: f64,
        right_voltage: f64,
    ) -> Result<(f64, Vec<f64>)> {
        let n = self.diffusion.n;
        let rho = check_topology(topology, n)?;
        let (matrix, rhs) = self.diffusion.system(&rho, charge, left_voltage, right_voltage, &[0.0])?;
        let potential = solve(&matrix, &rhs, n)?;
        let objective = |p: &[Dual<8>], r: &[Dual<8>]| -> Dual<8> {
            match self.derived_fields(r, p) {
                Ok((current, joule)) => response.eval(p, &current, &joule),
                Err(_) => Dual::constant(f64::NAN),
            }
        };
        let rho_c: Vec<Dual<8>> = lift(&rho);
        let (value, response_state) = gradient::<8, _>(|p| objective(p, &rho_c), &potential)
            .map_err(|e| FieldSolverError(e.to_string()))?;
        let pot_c: Vec<Dual<8>> = lift(&potential);
        let (_, direct) =
            gradient::<8, _>(|r| objective(&pot_c, r), &rho).map_err(|e| FieldSolverError(e.to_string()))?;
        let adjoint = solve(&transpose(&matrix, n), &response_state, n)?;
        let adj: Vec<Dual<8>> = lift(&adjoint);
        let (_, residual_product) = gradient::<8, _>(
            |r| match self.diffusion.residual(
                r,
                &pot_c,
                &lift(charge),
                left_voltage,
                right_voltage,
                &[Dual::constant(0.0)],
            ) {
                Ok(res) => res.iter().zip(&adj).fold(Dual::constant(0.0), |acc, (a, b)| acc + *a * *b),
                Err(_) => Dual::constant(f64::NAN),
            },
            &rho,
        )
        .map_err(|e| FieldSolverError(e.to_string()))?;
        Ok((value, direct.iter().zip(&residual_product).map(|(a, b)| a - b).collect()))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AcousticHelmholtzSolver {
    pub n: usize,
    pub length: f64,
    pub stiffness: (f64, f64),
    pub mass: (f64, f64),
    pub penal: f64,
    pub wave_speed: f64,
    pub damping: f64,
    pub dx: f64,
}

impl AcousticHelmholtzSolver {

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        n: usize,
        length: f64,
        stiffness: (f64, f64),
        mass: (f64, f64),
        penal: f64,
        wave_speed: f64,
        damping: f64,
    ) -> Result<Self> {
        let all = [length, stiffness.0, stiffness.1, mass.0, mass.1, penal, wave_speed, damping];
        if n < 3
            || !all.iter().all(Scalar::is_finite)
            || penal < 1.0
            || !(0.0 < stiffness.0 && stiffness.0 <= stiffness.1)
            || !(0.0 < mass.0 && mass.0 <= mass.1)
            || length <= 0.0
            || wave_speed <= 0.0
            || damping < 0.0
        {
            return fail("invalid Helmholtz discretisation");
        }
        Ok(Self { n, length, stiffness, mass, penal, wave_speed, damping, dx: length / (n - 1) as f64 })
    }

    fn wavenumber(&self, omega: f64) -> Result<f64> {
        if !omega.is_finite() {
            return fail("angular frequency must be a finite real scalar");
        }
        Ok(omega / self.wave_speed)
    }

    fn source<S: Scalar>(&self, source: &[S]) -> Result<Vec<S>> {
        vector(source, self.n, "source").map_err(|e| {
            if e.0.ends_with("must be finite") {
                FieldSolverError("Helmholtz source must be finite".into())
            } else {
                e
            }
        })
    }


    pub fn system<S: Scalar>(&self, topology: &[S], source: &[S], omega: f64) -> Result<(Vec<S>, Vec<S>)> {
        let n = self.n;
        let rho = check_topology(topology, n)?;
        let stiffness = interpolate(&rho, self.stiffness.0, self.stiffness.1, self.penal);
        let mass = interpolate(&rho, self.mass.0, self.mass.1, 1.0);
        let faces = harmonic_faces(&stiffness);
        let kappa = self.wavenumber(omega)?;
        let mut real = vec![S::zero(); n * n];
        let mut imag = vec![S::zero(); n * n];
        real[0] = S::one();
        real[n * n - 1] = S::one();
        let inv_dx2 = 1.0 / self.dx.powi(2);
        for i in 1..n - 1 {
            let lower = faces[i - 1] * inv_dx2;
            let upper = faces[i] * inv_dx2;
            real[i * n + i - 1] = -lower;
            real[i * n + i] = lower + upper - mass[i] * kappa.powi(2);
            real[i * n + i + 1] = -upper;
            imag[i * n + i] = mass[i] * (self.damping * kappa);
        }
        let m = 2 * n;
        let mut block = vec![S::zero(); m * m];
        for i in 0..n {
            for j in 0..n {
                block[i * m + j] = real[i * n + j];
                block[i * m + n + j] = -imag[i * n + j];
                block[(n + i) * m + j] = imag[i * n + j];
                block[(n + i) * m + n + j] = real[i * n + j];
            }
        }
        let mut rhs = self.source(source)?;
        rhs[0] = S::zero();
        rhs[n - 1] = S::zero();
        rhs.extend(std::iter::repeat_n(S::zero(), n));
        Ok((block, rhs))
    }


    pub fn solve<S: Scalar>(&self, topology: &[S], source: &[S], omega: f64) -> Result<(Vec<S>, Vec<S>)> {
        let (m, r) = self.system(topology, source, omega)?;
        let mut x = solve(&m, &r, 2 * self.n)?;
        let imag = x.split_off(self.n);
        Ok((x, imag))
    }


    pub fn residual<S: Scalar>(
        &self,
        topology: &[S],
        real: &[S],
        imag: &[S],
        source: &[S],
        omega: f64,
    ) -> Result<Vec<S>> {
        let (m, r) = self.system(topology, source, omega)?;
        let mut state = vector(real, self.n, "real pressure")?;
        state.extend(vector(imag, self.n, "imaginary pressure")?);
        Ok(matvec(&m, &state, 2 * self.n).into_iter().zip(r).map(|(a, b)| a - b).collect())
    }


    pub fn response_and_gradient<R: HelmholtzResponse>(
        &self,
        topology: &[f64],
        source: &[f64],
        response: &R,
        omega: f64,
    ) -> Result<(f64, Vec<f64>)> {
        let n = self.n;
        let m = 2 * n;
        let rho = topology_vec(topology, n)?;
        let (_, dstiff) = interpolate_numpy(&rho, self.stiffness.0, self.stiffness.1, self.penal);
        let (_, dmass) = interpolate_numpy(&rho, self.mass.0, self.mass.1, 1.0);
        let (stiff, _) = interpolate_numpy(&rho, self.stiffness.0, self.stiffness.1, self.penal);
        let (_, dl, dr) = harmonic_numpy(&stiff);
        let kappa = self.wavenumber(omega)?;
        let (matrix, rhs) = self.system(&rho, source, omega)?;
        let state = solve(&matrix, &rhs, m)?;
        let (value, rs) = gradient::<8, _>(|x: &[Dual<8>]| response.eval(&x[..n], &x[n..]), &state)
            .map_err(|e| FieldSolverError(e.to_string()))?;
        let adjoint = solve(&transpose(&matrix, m), &rs, m)?;
        let inv_dx2 = 1.0 / self.dx.powi(2);
        let interior = |row: usize| 0 < row && row < n - 1;
        let mut grad = vec![0.0; n];
        for j in 0..n {
            let mut dreal = vec![0.0; n * n];
            let mut dimag = vec![0.0; n * n];
            if j > 0 {
                let df = dr[j - 1] * dstiff[j] * inv_dx2;
                if interior(j) {
                    dreal[j * n + j] += df;
                    dreal[j * n + j - 1] -= df;
                }
                if interior(j - 1) {
                    dreal[(j - 1) * n + j - 1] += df;
                    dreal[(j - 1) * n + j] -= df;
                }
            }
            if j < n - 1 {
                let df = dl[j] * dstiff[j] * inv_dx2;
                if interior(j) {
                    dreal[j * n + j] += df;
                    dreal[j * n + j + 1] -= df;
                }
                if interior(j + 1) {
                    dreal[(j + 1) * n + j + 1] += df;
                    dreal[(j + 1) * n + j] -= df;
                }
            }
            if interior(j) {
                dreal[j * n + j] -= kappa.powi(2) * dmass[j];
                dimag[j * n + j] += self.damping * kappa * dmass[j];
            }
            let mut dstate = vec![0.0; m];
            for i in 0..n {
                for k in 0..n {
                    dstate[i] += dreal[i * n + k] * state[k] - dimag[i * n + k] * state[n + k];
                    dstate[n + i] += dimag[i * n + k] * state[k] + dreal[i * n + k] * state[n + k];
                }
            }
            grad[j] = -dot(&adjoint, &dstate);
        }
        Ok((value, grad))
    }
}

