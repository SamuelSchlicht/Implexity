// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Arc;

use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Value, json};

use crate::coupled_history::CoupledHistoryAssembly;
use crate::local_assembly::Kind;
use crate::matrix::{Jacobian, eliminate_zeros};
use crate::native_history::{
    HistoryAdjoint, HistoryOptions, HistoryProblem, HistorySolution, HistorySolveOptions, NativeHistorySystem,
};
use crate::operation_context::OperationExecutionContext;

pub trait FullHistoryAssembly: Send + Sync {
    fn state_size(&self) -> usize;
    fn design_size(&self) -> usize;


    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>>;


    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix>;


    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>>;
}

impl FullHistoryAssembly for CoupledHistoryAssembly {
    fn state_size(&self) -> usize {
        CoupledHistoryAssembly::state_size(self)
    }
    fn design_size(&self) -> usize {
        CoupledHistoryAssembly::design_size(self)
    }
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        CoupledHistoryAssembly::residual(self, n, z, old, x)
    }
    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        CoupledHistoryAssembly::jacobian(self, kind, n, z, old, x)
    }
    fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        CoupledHistoryAssembly::current_action(self, n, z, old, x, v, transpose)
    }
}

pub type InitialStateFactory = Arc<dyn Fn(&[f64]) -> CaeResult<Vec<f64>> + Send + Sync>;
pub type InitialStatePullback = Arc<dyn Fn(&[f64], &DenseMatrix) -> CaeResult<DenseMatrix> + Send + Sync>;

struct Reduced {
    assembly: Arc<dyn FullHistoryAssembly>,
    p: CsrMatrix,
    w: CsrMatrix,
    offsets: Vec<Vec<f64>>,
    state_size: usize,
    structural: std::sync::OnceLock<CsrMatrix>,
}

fn with_structural_entries(a: &CsrMatrix, pattern: &CsrMatrix) -> CaeResult<CsrMatrix> {
    let (rows, cols) = a.shape();
    if pattern.shape() != (rows, cols) {
        return Err(CaeError::contract("structural state pattern has another shape"));
    }
    let mut indptr = Vec::with_capacity(rows + 1);
    let mut indices = Vec::with_capacity(a.nnz() + pattern.nnz());
    let mut data = Vec::with_capacity(a.nnz() + pattern.nnz());
    indptr.push(0);
    for i in 0..rows {
        let ((ai, av), (pi, _)) = (a.row(i), pattern.row(i));
        let (mut x, mut y) = (0, 0);
        while x < ai.len() || y < pi.len() {
            if y == pi.len() || (x < ai.len() && ai[x] <= pi[y]) {
                if y < pi.len() && ai[x] == pi[y] {
                    y += 1;
                }
                indices.push(ai[x]);
                data.push(av[x]);
                x += 1;
            } else {
                indices.push(pi[y]);
                data.push(0.0);
                y += 1;
            }
        }
        indptr.push(indices.len());
    }
    CsrMatrix::try_new(rows, cols, indptr, indices, data).map_err(|e| CaeError::contract(e.to_string()))
}

impl Reduced {
    fn expand(&self, n: usize, z: &[f64]) -> CaeResult<Vec<f64>> {
        if n >= self.offsets.len() || z.len() != self.state_size || z.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("invalid shared state or time index"));
        }
        let pz = self.p.matvec(z).map_err(|e| CaeError::contract(e.to_string()))?;
        Ok(pz.iter().zip(&self.offsets[n]).map(|(a, b)| a + b).collect())
    }

    fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        if n < 1 {
            return Err(CaeError::contract("invalid history derivative request"));
        }
        let a = self.assembly.jacobian(kind, n, &self.expand(n, z)?, &self.expand(n - 1, old)?, x)?;
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        let left = self.w.matmul(&a).map_err(lin)?;
        let result = if kind == Kind::Design { left } else { left.matmul(&self.p).map_err(lin)? };
        let result = eliminate_zeros(&result);
        match (kind, self.structural.get()) {
            (Kind::Current, Some(pattern)) => with_structural_entries(&result, pattern),
            _ => Ok(result),
        }
    }
}

impl HistoryProblem for Reduced {
    fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        if n < 1 {
            return Err(CaeError::contract("history residual requires n>=1"));
        }
        let r = self.assembly.residual(n, &self.expand(n, z)?, &self.expand(n - 1, old)?, x)?;
        self.w.matvec(&r).map_err(|e| CaeError::contract(e.to_string()))
    }
    fn state_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csc(self.jacobian(Kind::Current, n, z, old, x)?.to_csc()))
    }
    fn previous_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.jacobian(Kind::Previous, n, z, old, x)?))
    }
    fn design_jacobian(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Jacobian> {
        Ok(Jacobian::Csr(self.jacobian(Kind::Design, n, z, old, x)?))
    }
}

pub struct AffineHistoryReduction {
    core: Arc<Reduced>,
    initial: Vec<f64>,
    design_size: usize,
    initial_state_factory: Option<InitialStateFactory>,
    initial_state_pullback: Option<InitialStatePullback>,
    system: NativeHistorySystem,
}

impl std::fmt::Debug for AffineHistoryReduction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AffineHistoryReduction").field("report", &self.report()).finish()
    }
}

impl AffineHistoryReduction {


    #[allow(clippy::too_many_arguments)]
    pub fn new(
        assembly: Arc<dyn FullHistoryAssembly>,
        state_map: CsrMatrix,
        residual_map: CsrMatrix,
        offsets: Vec<Vec<f64>>,
        initial: Vec<f64>,
        options: HistoryOptions,
        initial_state_factory: Option<InitialStateFactory>,
        initial_state_pullback: Option<InitialStatePullback>,
    ) -> CaeResult<Self> {
        if initial_state_factory.is_some() != initial_state_pullback.is_some() {
            return Err(CaeError::contract(
                "design-dependent initialization requires both state and exact pullback callbacks",
            ));
        }
        let full = assembly.state_size();
        let reduced = state_map.ncols();
        if state_map.nrows() != full || residual_map.shape() != (reduced, full) {
            return Err(CaeError::contract("shared-history map dimensions do not match field assembly"));
        }
        if reduced < 1 || offsets.len() < 2 || offsets.iter().any(|o| o.len() != full) {
            return Err(CaeError::contract("explicit affine offset for every history increment required"));
        }
        if initial.len() != reduced
            || !state_map.is_finite()
            || !residual_map.is_finite()
            || offsets.iter().flatten().any(|v| !v.is_finite())
            || initial.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract("nonfinite/invalid shared-state reduction"));
        }
        let p_csc = state_map.to_csc();
        let empty_column = p_csc.indptr().windows(2).any(|w| w[0] == w[1]);
        let empty_row = residual_map.indptr().windows(2).any(|w| w[0] == w[1]);
        if empty_column || empty_row {
            return Err(CaeError::contract(
                "shared-state reduction contains unused unknown or empty equation",
            ));
        }
        if let Some(c) = &options.criterion
            && let Some(bound) = c.state_size()
            && bound != reduced
        {
            return Err(CaeError::contract(format!(
                "reduction convergence criterion must be authored in the reduced frame (state_size mismatch: criterion {bound}, reduced frame {reduced}, full frame {full})"
            )));
        }
        if let Some(p) = &options.residual_partition
            && p.state_size() != reduced
        {
            return Err(CaeError::contract("residual diagnostic partition frame mismatch"));
        }
        let design_size = assembly.design_size();
        let core = Arc::new(Reduced {
            assembly,
            p: state_map,
            w: residual_map,
            offsets,
            state_size: reduced,
            structural: std::sync::OnceLock::new(),
        });
        let problem: Arc<dyn HistoryProblem> = core.clone();
        let system = NativeHistorySystem::new(problem, options)?;
        Ok(Self { core, initial, design_size, initial_state_factory, initial_state_pullback, system })
    }

    #[must_use]
    pub fn design_size(&self) -> usize {
        self.design_size
    }



    pub fn set_structural_state_pattern(&self, pattern: CsrMatrix) -> CaeResult<()> {
        let n = self.core.state_size;
        if pattern.shape() != (n, n) {
            return Err(CaeError::contract("structural state pattern must be state_size × state_size"));
        }
        self.core
            .structural
            .set(pattern)
            .map_err(|_| CaeError::contract("structural state pattern is declared once"))
    }

    #[must_use]
    pub fn system(&self) -> &NativeHistorySystem {
        &self.system
    }



    pub fn expand(&self, n: usize, z: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.expand(n, z)
    }



    pub fn residual(&self, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<Vec<f64>> {
        self.core.residual(n, z, old, x)
    }



    pub fn jacobian(&self, kind: Kind, n: usize, z: &[f64], old: &[f64], x: &[f64]) -> CaeResult<CsrMatrix> {
        self.core.jacobian(kind, n, z, old, x)
    }



    pub fn current_action(
        &self,
        n: usize,
        z: &[f64],
        old: &[f64],
        x: &[f64],
        v: &[f64],
        transpose: bool,
    ) -> CaeResult<Vec<f64>> {
        if v.len() != self.core.state_size || v.iter().any(|x| !x.is_finite()) {
            return Err(CaeError::contract("reduced current-action operand is invalid"));
        }
        if n < 1 {
            return Err(CaeError::contract("invalid shared state or time index"));
        }
        let full = self.core.expand(n, z)?;
        let previous = self.core.expand(n - 1, old)?;
        let lin = |e: implexity_linalg::error::LinalgError| CaeError::contract(e.to_string());
        if transpose {
            let cot = self.core.w.matvec_transpose(v).map_err(lin)?;
            let a = self.core.assembly.current_action(n, &full, &previous, x, &cot, true)?;
            self.core.p.matvec_transpose(&a).map_err(lin)
        } else {
            let t = self.core.p.matvec(v).map_err(lin)?;
            let a = self.core.assembly.current_action(n, &full, &previous, x, &t, false)?;
            self.core.w.matvec(&a).map_err(lin)
        }
    }



    pub fn initial_pullback(&self,design:&[f64],covectors:&DenseMatrix)->CaeResult<DenseMatrix> {
        if design.len()!=self.design_size || covectors.nrows!=self.core.state_size || design.iter().chain(&covectors.data).any(|v|!v.is_finite()) {return Err(CaeError::contract("initial-state pullback shape or values invalid"));}
        let value=match &self.initial_state_pullback {Some(owner)=>owner(design,covectors)?,None if self.initial_state_factory.is_none()=>DenseMatrix::zeros(self.design_size,covectors.ncols),None=>return Err(CaeError::contract("design-dependent initial state lacks its owner pullback"))};
        if value.nrows!=self.design_size || value.ncols!=covectors.ncols || value.data.iter().any(|v|!v.is_finite()) {return Err(CaeError::contract("invalid complete initial-state pullback"));}
        Ok(value)
    }

    pub fn initial_for(&self, design: &[f64]) -> CaeResult<Vec<f64>> {
        let value = match &self.initial_state_factory {
            None => self.initial.clone(),
            Some(f) => f(design)?,
        };
        if value.len() != self.core.state_size || value.iter().any(|v| !v.is_finite()) {
            return Err(CaeError::contract("invalid design-dependent initial state"));
        }
        Ok(value)
    }



    pub fn solve(
        &self,
        design: &[f64],
        steps: usize,
        options: &HistorySolveOptions<'_>,
    ) -> CaeResult<HistorySolution> {
        self.system.solve(design, &self.initial_for(design)?, steps, options)
    }



    pub fn adjoint_many(
        &self,
        design: &[f64],
        solution: &HistorySolution,
        gu: &[DenseMatrix],
        grad: &DenseMatrix,
        initial_design_jacobian: Option<Jacobian>,
        execution_context: Option<&OperationExecutionContext>,
    ) -> CaeResult<HistoryAdjoint> {
        if self.initial_state_factory.is_some() {
            if initial_design_jacobian.is_some() {
                return Err(CaeError::contract("initial derivative specified twice"));
            }
            let expected = self.initial_for(design)?;
            let same = solution.states.first().is_some_and(|s| {
                s.iter().zip(&expected).all(|(a, b)| a.to_bits() == b.to_bits()) && s.len() == expected.len()
            });
            if !same {
                return Err(CaeError::contract(
                    "history initial state does not match the design-dependent initializer",
                ));
            }
        }
        let mut out = self.system.adjoint_many(
            design,
            solution,
            gu,
            grad,
            initial_design_jacobian,
            execution_context,
        )?;
        if let Some(pullback) = &self.initial_state_pullback {
            let addition = pullback(design, &out.initial_state_covectors)?;
            if (addition.nrows, addition.ncols) != (out.gradients.nrows, out.gradients.ncols)
                || addition.data.iter().any(|v| !v.is_finite())
            {
                return Err(CaeError::contract("invalid complete initial-state pullback"));
            }
            let complete: Vec<f64> =
                out.gradients.data.iter().zip(&addition.data).map(|(a, b)| a + b).collect();
            if complete.iter().any(|v| !v.is_finite()) {
                return Err(CaeError::contract("nonfinite complete initial-state design gradient"));
            }
            out.gradients.data = complete;
            out.initial_state_design_derivative = Some("exact_owner_pullback_included");
            out.initial_state_design_gradient_norm = Some(crate::certificate::norm2(&addition.data));
        }
        Ok(out)
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let full = self.core.assembly.state_size();
        let mut out = json!({
            "state_unknowns": self.core.state_size,
            "unreduced_state_unknowns": full,
            "eliminated_duplicate_states": full.saturating_sub(self.core.state_size),
            "state_map_nnz": self.core.p.nnz(),
            "residual_map_nnz": self.core.w.nnz(),
            "state_jacobian_sparse_format": "csc_without_reduced_csr_copy",
            "maps": "explicit_constant_sparse_affine",
            "prescribed_offsets": "time_dependent_design_independent",
            "coupling": "same_Newton_and_all_history_adjoint",
            "global_dense_jacobian_allocated": false,
        });
        if let Value::Object(map) = &mut out {
            if self.initial_state_factory.is_some() {
                map.insert("initial_state_design_dependence".into(), json!(true));
            }
            if let Some(c) = &self.system.options().criterion {
                map.insert("convergence_criterion".into(), c.describe());
            }
        }
        out
    }
}
