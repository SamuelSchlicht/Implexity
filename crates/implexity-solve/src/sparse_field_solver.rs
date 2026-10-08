// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use implexity_ad::{Dual, Scalar};
use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::lu::SparseLu;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::certificate::residual_certificate;
use crate::differentiable::{Argument, DifferentiableResponse, WIDTH, gradient, response_value};
use crate::pyfmt::fmt_g6;

fn field_error(message: impl Into<String>) -> CaeError {
    CaeError::convergence(message)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryKind {
    Dirichlet,
    Neumann,
    Robin,
    Periodic,
}

impl BoundaryKind {


    pub fn parse(text: &str) -> CaeResult<Self> {
        match text {
            "dirichlet" => Ok(Self::Dirichlet),
            "neumann" => Ok(Self::Neumann),
            "robin" => Ok(Self::Robin),
            "periodic" => Ok(Self::Periodic),
            other => Err(CaeError::contract(format!("'{other}' is not a valid BoundaryKind"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldParam {
    Constant(f64),
    Array(Vec<f64>),
    Model(String),
}

impl Default for FieldParam {
    fn default() -> Self {
        Self::Constant(0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoundaryCondition {
    pub kind: BoundaryKind,
    pub value: FieldParam,
    pub coefficient: FieldParam,
}

impl BoundaryCondition {
    #[must_use]
    pub fn constant(kind: BoundaryKind, value: f64, coefficient: f64) -> Self {
        Self { kind, value: FieldParam::Constant(value), coefficient: FieldParam::Constant(coefficient) }
    }
}

pub trait TopologyModels: Send + Sync {
    fn coefficient<S: Scalar>(&self, _rho: &[S]) -> Option<Vec<S>> {
        None
    }
    fn field<S: Scalar>(&self, _name: &str, _rho: &[S]) -> Option<Vec<S>> {
        None
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultModels;

impl TopologyModels for DefaultModels {}

#[derive(Clone, Debug, PartialEq)]
pub struct StructuredGrid {
    shape: Vec<usize>,
    spacing: Vec<f64>,
}

impl StructuredGrid {


    pub fn new(shape: Vec<usize>, spacing: Vec<f64>) -> CaeResult<Self> {
        if !(1..=3).contains(&shape.len()) {
            return Err(CaeError::contract("structured field grid supports one, two or three dimensions"));
        }
        if spacing.len() != shape.len() {
            return Err(CaeError::contract("one spacing is required per grid dimension"));
        }
        if shape.iter().any(|&x| x < 2) || spacing.iter().any(|&x| !x.is_finite() || x <= 0.0) {
            return Err(CaeError::contract(
                "grid extents must contain at least two cells and positive spacing",
            ));
        }
        Ok(Self { shape, spacing })
    }
    #[must_use]
    pub fn ndim(&self) -> usize {
        self.shape.len()
    }
    #[must_use]
    pub fn size(&self) -> usize {
        self.shape.iter().product()
    }
    #[must_use]
    pub fn cell_volume(&self) -> f64 {
        self.spacing.iter().product()
    }
    #[must_use]
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }
    #[must_use]
    pub fn spacing(&self) -> &[f64] {
        &self.spacing
    }

    fn strides(&self) -> Vec<usize> {
        let mut s = vec![1; self.ndim()];
        for a in (0..self.ndim().saturating_sub(1)).rev() {
            s[a] = s[a + 1] * self.shape[a + 1];
        }
        s
    }

    fn unravel(&self, flat: usize) -> Vec<usize> {
        let strides = self.strides();
        strides.iter().zip(&self.shape).map(|(&s, &n)| (flat / s) % n).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseSolveDiagnostics {
    pub residual_norm: f64,
    pub relative_residual: f64,
    pub degrees_of_freedom: usize,
    pub matrix_nonzeros: usize,
    pub matrix_density: f64,
    pub symmetric_relative_error: f64,
    pub topology_coordinate: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SparseFieldSensitivity {
    pub value: f64,
    pub gradient: Vec<f64>,
    pub state: Vec<f64>,
    pub adjoint: Vec<f64>,
    pub primal: SparseSolveDiagnostics,
    pub adjoint_relative_residual: f64,
}

impl SparseFieldSensitivity {
    #[must_use]
    pub fn as_value(&self, shape: &[usize]) -> Value {
        json!({
            "value": self.value,
            "gradient_shape": shape,
            "state_shape": shape,
            "primal": {
                "residual_norm": self.primal.residual_norm,
                "relative_residual": self.primal.relative_residual,
                "degrees_of_freedom": self.primal.degrees_of_freedom,
                "matrix_nonzeros": self.primal.matrix_nonzeros,
                "matrix_density": self.primal.matrix_density,
                "symmetric_relative_error": self.primal.symmetric_relative_error,
                "topology_coordinate": self.primal.topology_coordinate,
            },
            "adjoint_relative_residual": self.adjoint_relative_residual,
            "topology_coordinate": TOPOLOGY_COORDINATE,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    Lower,
    Upper,
}

impl Side {


    pub fn parse(value: &str) -> CaeResult<Self> {
        match value.trim().to_lowercase().as_str() {
            "0" | "low" | "lower" | "-" => Ok(Self::Lower),
            "1" | "high" | "upper" | "+" => Ok(Self::Upper),
            _ => Err(CaeError::contract(format!("unknown boundary side '{value}'"))),
        }
    }
}

pub struct SparseScalarFieldSolver<M: TopologyModels = DefaultModels> {
    grid: StructuredGrid,
    boundaries: BTreeMap<(usize, Side), BoundaryCondition>,
    models: M,
    k_min: f64,
    k_max: f64,
    penal: f64,
    residual_tolerance: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct FieldSolverOptions {
    pub k_min: f64,
    pub k_max: f64,
    pub penal: f64,
    pub residual_tolerance: f64,
}

impl Default for FieldSolverOptions {
    fn default() -> Self {
        Self { k_min: 1e-3, k_max: 1.0, penal: 3.0, residual_tolerance: 1e-9 }
    }
}

fn harmonic<S: Scalar>(a: S, b: S) -> S {
    a * b * 2.0 / (a + b).max_f64(1e-30)
}

fn broadcast<S: Scalar>(v: Vec<S>, n: usize, name: &str) -> CaeResult<Vec<S>> {
    match v.len() {
        1 => Ok(vec![v[0]; n]),
        l if l == n => Ok(v),
        _ => Err(field_error(format!("{name} cannot be broadcast to the grid shape"))),
    }
}

impl<M: TopologyModels> SparseScalarFieldSolver<M> {


    pub fn new(
        grid: StructuredGrid,
        boundaries: Vec<((usize, Side), BoundaryCondition)>,
        models: M,
        options: FieldSolverOptions,
    ) -> CaeResult<Self> {
        if !(options.k_min > 0.0 && options.k_min <= options.k_max) || options.penal <= 0.0 {
            return Err(CaeError::contract(
                "positive ordered coefficient limits and penalisation are required",
            ));
        }
        let mut map = BTreeMap::new();
        for axis in 0..grid.ndim() {
            for side in [Side::Lower, Side::Upper] {
                map.insert((axis, side), BoundaryCondition::constant(BoundaryKind::Neumann, 0.0, 0.0));
            }
        }
        for ((axis, side), bc) in boundaries {
            if axis >= grid.ndim() {
                return Err(CaeError::contract(format!(
                    "boundary axis {axis} is outside grid dimension {}",
                    grid.ndim()
                )));
            }
            map.insert((axis, side), bc);
        }
        for axis in 0..grid.ndim() {
            let lower = map[&(axis, Side::Lower)].kind == BoundaryKind::Periodic;
            let upper = map[&(axis, Side::Upper)].kind == BoundaryKind::Periodic;
            if lower != upper {
                return Err(CaeError::contract(format!(
                    "periodic boundary on axis {axis} must be declared on both sides"
                )));
            }
        }
        Ok(Self {
            grid,
            boundaries: map,
            models,
            k_min: options.k_min,
            k_max: options.k_max,
            penal: options.penal,
            residual_tolerance: options.residual_tolerance,
        })
    }

    #[must_use]
    pub fn grid(&self) -> &StructuredGrid {
        &self.grid
    }

    fn topology_checked(&self, topology: &[f64]) -> CaeResult<()> {
        if topology.len() != self.grid.size() {
            return Err(field_error(format!(
                "topology has {} values; grid requires {}",
                topology.len(),
                self.grid.size()
            )));
        }
        if topology.iter().any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0) {
            return Err(field_error("topology values must be finite and lie in [0, 1]"));
        }
        Ok(())
    }



    pub fn coefficient<S: Scalar>(&self, rho: &[S]) -> CaeResult<Vec<S>> {
        let n = self.grid.size();
        let raw = match self.models.coefficient(rho) {
            Some(v) => v,
            None => rho
                .iter()
                .map(|&r| r.clip(0.0, 1.0).powf(self.penal) * (self.k_max - self.k_min) + self.k_min)
                .collect(),
        };
        broadcast(raw, n, "coefficient model")
            .map_err(|_| field_error("coefficient model cannot be broadcast to the grid shape"))
    }

    fn param_field<S: Scalar>(&self, p: &FieldParam, rho: &[S], name: &str) -> CaeResult<Vec<S>> {
        let n = self.grid.size();
        match p {
            FieldParam::Constant(c) => Ok(vec![S::from_f64(*c); n]),
            FieldParam::Array(a) => broadcast(a.iter().map(|&v| S::from_f64(v)).collect(), n, name),
            FieldParam::Model(m) => {
                let v = self
                    .models
                    .field(m, rho)
                    .ok_or_else(|| CaeError::contract(format!("unknown topology model '{m}'")))?;
                broadcast(v, n, name)
            }
        }
    }

    fn param_scalar<S: Scalar>(&self, p: &FieldParam, rho: &[S], name: &str) -> CaeResult<S> {
        let v = match p {
            FieldParam::Constant(c) => vec![S::from_f64(*c)],
            FieldParam::Array(a) => a.iter().map(|&v| S::from_f64(v)).collect(),
            FieldParam::Model(m) => self
                .models
                .field(m, rho)
                .ok_or_else(|| CaeError::contract(format!("unknown topology model '{m}'")))?,
        };
        if v.len() != 1 {
            return Err(field_error(format!("{name} must be scalar for this boundary substrate")));
        }
        Ok(v[0])
    }

    fn neighbours(&self) -> Vec<(usize, usize, usize)> {

        let strides = self.grid.strides();
        let mut out = Vec::new();
        for flat in 0..self.grid.size() {
            let idx = self.grid.unravel(flat);
            for axis in 0..self.grid.ndim() {
                if idx[axis] + 1 < self.grid.shape[axis] {
                    out.push((flat, flat + strides[axis], axis));
                }
            }
        }
        out
    }

    fn periodic_pairs(&self, axis: usize) -> Vec<(usize, usize)> {
        let strides = self.grid.strides();
        let last = self.grid.shape[axis] - 1;
        (0..self.grid.size())
            .filter(|&f| self.grid.unravel(f)[axis] == 0)
            .map(|f| (f, f + last * strides[axis]))
            .collect()
    }

    fn face_cells(&self, axis: usize, side: Side) -> Vec<usize> {
        let fixed = if side == Side::Lower { 0 } else { self.grid.shape[axis] - 1 };
        (0..self.grid.size()).filter(|&f| self.grid.unravel(f)[axis] == fixed).collect()
    }



    #[allow(clippy::too_many_lines)]
    pub fn matrix_and_rhs(
        &self,
        topology: &[f64],
        source: &FieldParam,
        reaction: &FieldParam,
    ) -> CaeResult<(CsrMatrix, Vec<f64>)> {
        self.topology_checked(topology)?;
        let coefficient = self.coefficient::<f64>(topology)?;
        if coefficient.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(field_error("coefficient model must return finite positive values"));
        }
        let source_np = self.param_field::<f64>(source, topology, "source")?;
        let reaction_np = self.param_field::<f64>(reaction, topology, "reaction")?;
        for (name, v) in [("source", &source_np), ("reaction", &reaction_np)] {
            if v.iter().any(|x| !x.is_finite()) {
                return Err(field_error(format!("{name} contains non-finite values")));
            }
        }
        if reaction_np.iter().any(|v| *v < 0.0) {
            return Err(field_error("reaction coefficient must be non-negative"));
        }
        let n = self.grid.size();
        let mut rows = Vec::new();
        let mut cols = Vec::new();
        let mut data = Vec::new();
        let mut diagonal = reaction_np.clone();
        let mut rhs = source_np;
        let mut add = |i: usize, j: usize, v: f64| {
            rows.push(i);
            cols.push(j);
            data.push(v);
        };
        for (i, j, axis) in self.neighbours() {
            let dx = self.grid.spacing[axis];
            let g = harmonic(coefficient[i], coefficient[j]) / (dx * dx);
            diagonal[i] += g;
            diagonal[j] += g;
            add(i, j, -g);
            add(j, i, -g);
        }
        let mut anchored = reaction_np.iter().any(|v| *v > 0.0);
        for axis in 0..self.grid.ndim() {
            let dx = self.grid.spacing[axis];
            for side in [Side::Lower, Side::Upper] {
                let bc = &self.boundaries[&(axis, side)];
                if bc.kind == BoundaryKind::Periodic {
                    if side == Side::Upper {
                        continue;
                    }
                    for (i, j) in self.periodic_pairs(axis) {
                        let g = harmonic(coefficient[i], coefficient[j]) / (dx * dx);
                        diagonal[i] += g;
                        diagonal[j] += g;
                        add(i, j, -g);
                        add(j, i, -g);
                    }
                    continue;
                }
                let label = if side == Side::Lower { "lower" } else { "upper" };
                let value =
                    self.param_scalar::<f64>(&bc.value, topology, &format!("boundary {axis}:{label} value"))?;
                let transfer = self.param_scalar::<f64>(
                    &bc.coefficient,
                    topology,
                    &format!("boundary {axis}:{label} coefficient"),
                )?;
                for (name, v) in [("value", value), ("coefficient", transfer)] {
                    if !v.is_finite() {
                        return Err(field_error(format!("boundary {axis}:{label} {name} must be finite")));
                    }
                }
                for i in self.face_cells(axis, side) {
                    match bc.kind {
                        BoundaryKind::Dirichlet => {
                            let g = 2.0 * coefficient[i] / (dx * dx);
                            diagonal[i] += g;
                            rhs[i] += g * value;
                            anchored = true;
                        }
                        BoundaryKind::Neumann => rhs[i] -= value / dx,
                        BoundaryKind::Robin => {
                            if transfer < 0.0 {
                                return Err(field_error("Robin coefficient must be non-negative"));
                            }
                            diagonal[i] += transfer / dx;
                            rhs[i] += transfer * value / dx;
                            anchored = anchored || transfer > 0.0;
                        }
                        BoundaryKind::Periodic => {}
                    }
                }
            }
        }
        if !anchored {
            return Err(field_error(
                "unanchored diffusion operator: add a Dirichlet/Robin condition, positive reaction, or an explicit gauge",
            ));
        }
        for (i, v) in diagonal.iter().enumerate() {
            add(i, i, *v);
        }
        let matrix = CsrMatrix::from_triplets(n, n, &rows, &cols, &data)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        Ok((matrix, rhs))
    }



    pub fn residual<S: Scalar>(
        &self,
        rho: &[S],
        u: &[S],
        source: &FieldParam,
        reaction: &FieldParam,
    ) -> CaeResult<Vec<S>> {
        let coefficient = self.coefficient(rho)?;
        let src = self.param_field(source, rho, "source")?;
        let rea = self.param_field(reaction, rho, "reaction")?;
        let mut r: Vec<S> = (0..u.len()).map(|i| rea[i] * u[i] - src[i]).collect();
        for (i, j, axis) in self.neighbours() {
            let dx = self.grid.spacing[axis];
            let flux = harmonic(coefficient[i], coefficient[j]) / (dx * dx) * (u[i] - u[j]);
            r[i] += flux;
            r[j] -= flux;
        }
        for axis in 0..self.grid.ndim() {
            let dx = self.grid.spacing[axis];
            for side in [Side::Lower, Side::Upper] {
                let bc = &self.boundaries[&(axis, side)];
                if bc.kind == BoundaryKind::Periodic {
                    if side == Side::Upper {
                        continue;
                    }
                    for (i, j) in self.periodic_pairs(axis) {
                        let flux = harmonic(coefficient[i], coefficient[j]) / (dx * dx) * (u[i] - u[j]);
                        r[i] += flux;
                        r[j] -= flux;
                    }
                    continue;
                }
                let label = if side == Side::Lower { "lower" } else { "upper" };
                let value = self.param_scalar(&bc.value, rho, &format!("boundary {axis}:{label} value"))?;
                let transfer =
                    self.param_scalar(&bc.coefficient, rho, &format!("boundary {axis}:{label} coefficient"))?;
                for i in self.face_cells(axis, side) {
                    match bc.kind {
                        BoundaryKind::Dirichlet => r[i] += coefficient[i] * 2.0 / (dx * dx) * (u[i] - value),
                        BoundaryKind::Neumann => r[i] += value / dx,
                        BoundaryKind::Robin => r[i] += transfer / dx * (u[i] - value),
                        BoundaryKind::Periodic => {}
                    }
                }
            }
        }
        Ok(r)
    }

    fn solve_checked(matrix: &CsrMatrix, rhs: &[f64], label: &str, transpose: bool) -> CaeResult<Vec<f64>> {
        let lu = SparseLu::new(&matrix.to_csc())
            .map_err(|_| field_error(format!("sparse {label} solve failed")))?;
        let x = if transpose { lu.solve_transpose(rhs) } else { lu.solve(rhs) }
            .map_err(|_| field_error(format!("sparse {label} solve failed")))?;
        if x.iter().any(|v| !v.is_finite()) {
            return Err(field_error(format!("sparse {label} solve returned non-finite values")));
        }
        Ok(x)
    }

    fn diagnostics(
        &self,
        matrix: &CsrMatrix,
        state: &[f64],
        rhs: &[f64],
    ) -> CaeResult<SparseSolveDiagnostics> {
        let ax = matrix.matvec(state).map_err(|e| CaeError::contract(e.to_string()))?;
        let res: Vec<f64> = ax.iter().zip(rhs).map(|(a, b)| a - b).collect();
        let (norm, rel) = residual_certificate(&res, rhs)
            .map_err(|e| field_error(format!("sparse residual certification failed: {e}")))?;
        let diff = matrix
            .add_scaled(1.0, &matrix.transpose(), -1.0)
            .map_err(|e| CaeError::contract(e.to_string()))?;
        let n = self.grid.size();
        Ok(SparseSolveDiagnostics {
            residual_norm: norm,
            relative_residual: rel,
            degrees_of_freedom: n,
            matrix_nonzeros: matrix.nnz(),
            matrix_density: matrix.nnz() as f64 / (n * n) as f64,
            symmetric_relative_error: diff.norm_fro() / matrix.norm_fro().max(1e-30),
            topology_coordinate: TOPOLOGY_COORDINATE,
        })
    }



    pub fn solve(
        &self,
        topology: &[f64],
        source: &FieldParam,
        reaction: &FieldParam,
    ) -> CaeResult<(Vec<f64>, SparseSolveDiagnostics)> {
        let (matrix, rhs) = self.matrix_and_rhs(topology, source, reaction)?;
        let state = Self::solve_checked(&matrix, &rhs, "primal", false)?;
        let diag = self.diagnostics(&matrix, &state, &rhs)?;
        if diag.relative_residual > self.residual_tolerance {
            return Err(field_error(format!(
                "sparse primal residual {} exceeds {}",
                fmt_g6(diag.relative_residual),
                fmt_g6(self.residual_tolerance)
            )));
        }
        Ok((state, diag))
    }



    pub fn residual_design_vjp(
        &self,
        rho: &[f64],
        u: &[f64],
        lam: &[f64],
        source: &FieldParam,
        reaction: &FieldParam,
    ) -> CaeResult<Vec<f64>> {
        let n = rho.len();
        let du: Vec<Dual<WIDTH>> = u.iter().map(|&v| Dual::constant(v)).collect();
        let mut dr: Vec<Dual<WIDTH>> = rho.iter().map(|&v| Dual::constant(v)).collect();
        let mut out = vec![0.0; n];
        for pass in 0..n.div_ceil(WIDTH) {
            let start = pass * WIDTH;
            let end = (start + WIDTH).min(n);
            for (j, s) in dr.iter_mut().enumerate() {
                s.eps = [0.0; WIDTH];
                if (start..end).contains(&j) {
                    s.eps[j - start] = 1.0;
                }
            }
            let r = self.residual(&dr, &du, source, reaction)?;
            for (k, o) in out[start..end].iter_mut().enumerate() {
                *o = r.iter().zip(lam).map(|(ri, li)| li * ri.eps[k]).sum();
            }
        }
        Ok(out)
    }



    pub fn response_and_gradient<J: DifferentiableResponse>(
        &self,
        topology: &[f64],
        source: &FieldParam,
        response: &J,
        reaction: &FieldParam,
    ) -> CaeResult<SparseFieldSensitivity> {
        self.topology_checked(topology)?;
        let (matrix, rhs) = self.matrix_and_rhs(topology, source, reaction)?;
        let state = Self::solve_checked(&matrix, &rhs, "primal", false)?;
        let primal = self.diagnostics(&matrix, &state, &rhs)?;
        if primal.relative_residual > self.residual_tolerance {
            return Err(field_error("primal residual is too large for an exact adjoint"));
        }
        let value = response_value(response, &state, topology);
        let gu = gradient(response, &state, topology, Argument::State);
        let gx = gradient(response, &state, topology, Argument::Design);
        let adjoint = Self::solve_checked(&matrix, &gu, "transpose", true)?;
        let at = matrix.matvec_transpose(&adjoint).map_err(|e| CaeError::contract(e.to_string()))?;
        let ares: Vec<f64> = at.iter().zip(&gu).map(|(a, b)| a - b).collect();
        let (_, arel) = residual_certificate(&ares, &gu)
            .map_err(|e| field_error(format!("sparse residual certification failed: {e}")))?;
        if arel > self.residual_tolerance {
            return Err(field_error("adjoint residual is too large for direct-gradient admission"));
        }
        let product = self.residual_design_vjp(topology, &state, &adjoint, source, reaction)?;
        let grad: Vec<f64> = gx.iter().zip(&product).map(|(a, b)| a - b).collect();
        Ok(SparseFieldSensitivity {
            value,
            gradient: grad,
            state,
            adjoint,
            primal,
            adjoint_relative_residual: arel,
        })
    }
}

#[must_use]
pub fn all_dirichlet(grid: &StructuredGrid, value: f64) -> Vec<((usize, Side), BoundaryCondition)> {
    (0..grid.ndim())
        .flat_map(|axis| {
            [Side::Lower, Side::Upper].into_iter().map(move |side| {
                ((axis, side), BoundaryCondition::constant(BoundaryKind::Dirichlet, value, 0.0))
            })
        })
        .collect()
}

