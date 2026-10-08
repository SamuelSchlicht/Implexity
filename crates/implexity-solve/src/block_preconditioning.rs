// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::time::Instant;

use implexity_core::contracts::TOPOLOGY_COORDINATE;
use implexity_core::error::{CaeError, CaeResult};
use implexity_core::json::canonical_sha256;
use implexity_core::rng::default_rng;
use implexity_linalg::lu::SparseLu;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::json;

use crate::matrix::Jacobian;
use crate::pyfmt::fmt_e;
use crate::sparse_block_residual::{BlockLayout, to_csr_dropping_zeros};

pub const COMPATIBILITY_ONLY: bool = true;
pub const CANONICAL_EXACT_ACCELERATION: bool = false;

fn err(message: impl Into<String>) -> CaeError {
    CaeError::contract(message)
}

#[derive(Clone, Debug, PartialEq)]
pub struct BlockPreconditionerDiagnostics {
    pub provider_name: String,
    pub strategy: String,
    pub primary_block: String,
    pub secondary_block: String,
    pub degrees_of_freedom: usize,
    pub primary_nonzeros: usize,
    pub coupling_nonzeros: usize,
    pub secondary_nonzeros: usize,
    pub schur_nonzeros: usize,
    pub primary_shift: f64,
    pub schur_shift: f64,
    pub build_seconds: f64,
    pub transpose_duality_error: f64,
    pub topology_coordinate: &'static str,
}

pub type BlockKey = (String, String);

pub trait ProviderBlockJacobian: Send + Sync {
    fn name(&self) -> &str;
    fn layout(&self) -> &BlockLayout;
    fn topology_coordinate(&self) -> &str {
        TOPOLOGY_COORDINATE
    }


    fn blocks(&self, state: &[f64], design: &[f64]) -> CaeResult<BTreeMap<BlockKey, Jacobian>>;


    fn schur_approximation(
        &self,
        _state: &[f64],
        _design: &[f64],
        _primary: &str,
        _secondary: &str,
    ) -> CaeResult<Option<Jacobian>> {
        Ok(None)
    }
}

type BlockBuilder = Box<dyn Fn(&[f64], &[f64]) -> CaeResult<BTreeMap<BlockKey, Jacobian>> + Send + Sync>;
type SchurBuilder = Box<dyn Fn(&[f64], &[f64], &str, &str) -> CaeResult<Option<Jacobian>> + Send + Sync>;

pub struct ProviderBlockJacobianContract {
    name: String,
    layout: BlockLayout,
    block_builder: BlockBuilder,
    schur_builder: Option<SchurBuilder>,
}

impl ProviderBlockJacobianContract {


    pub fn new(
        name: &str,
        layout: BlockLayout,
        block_builder: BlockBuilder,
        schur_builder: Option<SchurBuilder>,
    ) -> CaeResult<Self> {
        if name.trim().is_empty() {
            return Err(err("provider preconditioner contract requires a name"));
        }
        if layout.names().len() != 2 {
            return Err(err("Stage-40 Schur contract requires exactly two state blocks"));
        }
        Ok(Self { name: name.to_string(), layout, block_builder, schur_builder })
    }
}

impl ProviderBlockJacobian for ProviderBlockJacobianContract {
    fn name(&self) -> &str {
        &self.name
    }
    fn layout(&self) -> &BlockLayout {
        &self.layout
    }
    fn blocks(&self, state: &[f64], design: &[f64]) -> CaeResult<BTreeMap<BlockKey, Jacobian>> {
        (self.block_builder)(state, design)
    }
    fn schur_approximation(
        &self,
        state: &[f64],
        design: &[f64],
        primary: &str,
        secondary: &str,
    ) -> CaeResult<Option<Jacobian>> {
        match &self.schur_builder {
            Some(f) => f(state, design, primary, secondary),
            None => Ok(None),
        }
    }
}

fn valid_csr(value: Jacobian, shape: (usize, usize), name: &str) -> CaeResult<CsrMatrix> {
    let m = to_csr_dropping_zeros(value)?;
    if m.shape() != shape {
        return Err(err(format!(
            "{name} has shape ({}, {}); expected ({}, {})",
            m.nrows(),
            m.ncols(),
            shape.0,
            shape.1
        )));
    }
    if !m.is_finite() {
        return Err(err(format!("{name} contains non-finite coefficients")));
    }
    Ok(m)
}

fn factor(m: &CsrMatrix, name: &str) -> CaeResult<SparseLu> {
    SparseLu::new(&m.to_csc()).map_err(|_| err(format!("{name} factorisation failed")))
}

fn shifted(m: &CsrMatrix, shift: f64) -> CaeResult<CsrMatrix> {
    if shift == 0.0 {
        return Ok(m.clone());
    }
    m.add_scaled(1.0, &CsrMatrix::identity(m.nrows()), shift).map_err(|e| err(e.to_string()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchurMode {
    DiagonalPrimary,
    SecondaryOnly,
    Provider,
}

impl SchurMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DiagonalPrimary => "diagonal_primary",
            Self::SecondaryOnly => "secondary_only",
            Self::Provider => "provider",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SchurOptions {
    pub schur_mode: SchurMode,
    pub diagonal_floor: f64,
    pub primary_shift: f64,
    pub schur_shift: f64,
    pub verify_transpose: bool,
    pub transpose_tolerance: f64,
}

impl Default for SchurOptions {
    fn default() -> Self {
        Self {
            schur_mode: SchurMode::DiagonalPrimary,
            diagonal_floor: 1e-12,
            primary_shift: 0.0,
            schur_shift: 0.0,
            verify_transpose: true,
            transpose_tolerance: 1e-10,
        }
    }
}

pub struct BlockSchurPreconditioner {
    provider_name: String,
    layout: BlockLayout,
    primary_block: String,
    secondary_block: String,
    mode: SchurMode,
    primary_shift: f64,
    schur_shift: f64,
    a: CsrMatrix,
    b: CsrMatrix,
    c: CsrMatrix,
    d: CsrMatrix,
    s: CsrMatrix,
    a_lu: SparseLu,
    s_lu: SparseLu,
    n1: usize,
    n2: usize,
    build_seconds: f64,
    duality_error: f64,
}

impl std::fmt::Debug for BlockSchurPreconditioner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockSchurPreconditioner")
            .field("diagnostics", &self.diagnostics())
            .finish_non_exhaustive()
    }
}

impl BlockSchurPreconditioner {


    #[allow(clippy::too_many_arguments, clippy::many_single_char_names)]
    pub fn new(
        provider_name: &str,
        layout: BlockLayout,
        primary_block: &str,
        secondary_block: &str,
        a: Jacobian,
        b: Jacobian,
        c: Jacobian,
        d: Jacobian,
        schur: Option<Jacobian>,
        options: SchurOptions,
    ) -> CaeResult<Self> {
        let started = Instant::now();
        if layout.names() != [primary_block.to_string(), secondary_block.to_string()] {
            return Err(err("layout order must match primary and secondary Schur blocks"));
        }
        let (n1, n2) = (layout.sizes()[0], layout.sizes()[1]);
        let mut a = valid_csr(a, (n1, n1), "primary block A")?;
        let b = valid_csr(b, (n1, n2), "coupling block B")?;
        let c = valid_csr(c, (n2, n1), "coupling block C")?;
        let d = valid_csr(d, (n2, n2), "secondary block D")?;
        a = shifted(&a, options.primary_shift)?;
        let a_lu = factor(&a, "primary block")?;
        let (mode, mut s) = match (schur, options.schur_mode) {
            (Some(sm), _) => (SchurMode::Provider, valid_csr(sm, (n2, n2), "provider Schur approximation")?),
            (None, SchurMode::DiagonalPrimary) => {
                let diag = a.diagonal();
                if diag.len() != n1 || diag.iter().any(|v| !v.is_finite()) {
                    return Err(err("primary block diagonal is invalid"));
                }
                let inv: Vec<f64> = diag
                    .iter()
                    .map(|&v| {
                        let sign = if v < 0.0 { -1.0 } else { 1.0 };
                        let safe =
                            if v.abs() >= options.diagonal_floor { v } else { sign * options.diagonal_floor };
                        1.0 / safe
                    })
                    .collect();
                let lin = |e: implexity_linalg::error::LinalgError| err(e.to_string());
                let cdb =
                    c.matmul(&CsrMatrix::diagonal_matrix(&inv)).map_err(lin)?.matmul(&b).map_err(lin)?;
                (SchurMode::DiagonalPrimary, d.add_scaled(1.0, &cdb, -1.0).map_err(lin)?)
            }
            (None, SchurMode::SecondaryOnly) => (SchurMode::SecondaryOnly, d.clone()),
            (None, SchurMode::Provider) => {
                return Err(err(
                    "schur_mode must be 'diagonal_primary', 'secondary_only', or provider supplied",
                ));
            }
        };
        s = shifted(&s, options.schur_shift)?;
        let s_lu = factor(&s, "Schur approximation")?;
        let mut out = Self {
            provider_name: provider_name.to_string(),
            layout,
            primary_block: primary_block.to_string(),
            secondary_block: secondary_block.to_string(),
            mode,
            primary_shift: options.primary_shift,
            schur_shift: options.schur_shift,
            a,
            b,
            c,
            d,
            s,
            a_lu,
            s_lu,
            n1,
            n2,
            build_seconds: started.elapsed().as_secs_f64(),
            duality_error: 0.0,
        };
        if options.verify_transpose {
            out.duality_error = out.verify_transpose_duality(3, 4020)?;
            if out.duality_error > options.transpose_tolerance {
                return Err(err(format!(
                    "preconditioner transpose action violates duality: {} > {}",
                    fmt_e(out.duality_error, 3),
                    fmt_e(options.transpose_tolerance, 3)
                )));
            }
        }
        Ok(out)
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.n1 + self.n2
    }

    #[must_use]
    pub fn signature(&self) -> String {
        canonical_sha256(&json!({
            "provider": self.provider_name,
            "layout": [self.layout.names(), self.layout.sizes()],
            "mode": self.mode.as_str(),
            "nnz": [self.a.nnz(), self.b.nnz(), self.c.nnz(), self.d.nnz(), self.s.nnz()],
            "shifts": [self.primary_shift, self.schur_shift],
        }))
    }



    pub fn apply(&self, vector: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        if vector.len() != self.size() {
            return Err(err(format!(
                "preconditioner vector has {} entries; expected {}",
                vector.len(),
                self.size()
            )));
        }
        if vector.iter().any(|v| !v.is_finite()) {
            return Err(err("preconditioner input contains non-finite values"));
        }
        let (r1, r2) = vector.split_at(self.n1);
        let lin = |e: implexity_linalg::error::LinalgError| err(e.to_string());
        let (x1, x2) = if transpose {
            let y1 = self.a_lu.solve_transpose(r1).map_err(lin)?;
            let bt = self.b.matvec_transpose(&y1).map_err(lin)?;
            let z2: Vec<f64> = r2.iter().zip(&bt).map(|(a, b)| a - b).collect();
            let x2 = self.s_lu.solve_transpose(&z2).map_err(lin)?;
            let ct = self.c.matvec_transpose(&x2).map_err(lin)?;
            let corr = self.a_lu.solve_transpose(&ct).map_err(lin)?;
            (y1.iter().zip(&corr).map(|(a, b)| a - b).collect::<Vec<_>>(), x2)
        } else {
            let y1 = self.a_lu.solve(r1).map_err(lin)?;
            let cy = self.c.matvec(&y1).map_err(lin)?;
            let z2: Vec<f64> = r2.iter().zip(&cy).map(|(a, b)| a - b).collect();
            let x2 = self.s_lu.solve(&z2).map_err(lin)?;
            let bx = self.b.matvec(&x2).map_err(lin)?;
            let corr = self.a_lu.solve(&bx).map_err(lin)?;
            (y1.iter().zip(&corr).map(|(a, b)| a - b).collect::<Vec<_>>(), x2)
        };
        let mut result = x1;
        result.extend(x2);
        if result.iter().any(|v| !v.is_finite()) {
            return Err(err("preconditioner produced non-finite values"));
        }
        Ok(result)
    }



    pub fn verify_transpose_duality(&self, probes: usize, seed: u128) -> CaeResult<f64> {
        let mut generator = default_rng(seed);
        let mut worst: f64 = 0.0;
        for _ in 0..probes.max(1) {
            let left = generator.normal_vec(0.0, 1.0, self.size())?;
            let right = generator.normal_vec(0.0, 1.0, self.size())?;
            let lhs: f64 = self.apply(&left, false)?.iter().zip(&right).map(|(a, b)| a * b).sum();
            let rhs: f64 = left.iter().zip(&self.apply(&right, true)?).map(|(a, b)| a * b).sum();
            let scale = 1.0_f64.max(lhs.abs()).max(rhs.abs());
            worst = worst.max((lhs - rhs).abs() / scale);
        }
        Ok(worst)
    }

    #[must_use]
    pub fn diagnostics(&self) -> BlockPreconditionerDiagnostics {
        BlockPreconditionerDiagnostics {
            provider_name: self.provider_name.clone(),
            strategy: format!("block_ldu_{}", self.mode.as_str()),
            primary_block: self.primary_block.clone(),
            secondary_block: self.secondary_block.clone(),
            degrees_of_freedom: self.size(),
            primary_nonzeros: self.a.nnz(),
            coupling_nonzeros: self.b.nnz() + self.c.nnz(),
            secondary_nonzeros: self.d.nnz(),
            schur_nonzeros: self.s.nnz(),
            primary_shift: self.primary_shift,
            schur_shift: self.schur_shift,
            build_seconds: self.build_seconds,
            transpose_duality_error: self.duality_error,
            topology_coordinate: TOPOLOGY_COORDINATE,
        }
    }
}

pub struct ProviderNativePreconditionerFactory {
    contract: Box<dyn ProviderBlockJacobian>,
    primary_block: String,
    secondary_block: String,
    options: SchurOptions,
}

impl ProviderNativePreconditionerFactory {


    pub fn new(
        contract: Box<dyn ProviderBlockJacobian>,
        primary_block: Option<&str>,
        secondary_block: Option<&str>,
        options: SchurOptions,
    ) -> CaeResult<Self> {
        if contract.topology_coordinate() != TOPOLOGY_COORDINATE {
            return Err(err(format!("provider preconditioner must declare '{TOPOLOGY_COORDINATE}'")));
        }
        let names = contract.layout().names().to_vec();
        if names.len() != 2 {
            return Err(err("Stage-40 Schur contract requires exactly two state blocks"));
        }
        Ok(Self {
            primary_block: primary_block.map_or_else(|| names[0].clone(), str::to_string),
            secondary_block: secondary_block.map_or_else(|| names[1].clone(), str::to_string),
            contract,
            options,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        self.contract.name()
    }



    pub fn build(&self, state: &[f64], design: &[f64]) -> CaeResult<BlockSchurPreconditioner> {
        let layout = self.contract.layout().clone();
        if state.len() != layout.size() {
            return Err(err(format!(
                "provider state has {} entries; layout requires {}",
                state.len(),
                layout.size()
            )));
        }
        if state.iter().chain(design).any(|v| !v.is_finite()) {
            return Err(err("provider state/design must be finite"));
        }
        let mut blocks = self.contract.blocks(state, design)?;
        let (p, s) = (self.primary_block.clone(), self.secondary_block.clone());
        let required =
            [(p.clone(), p.clone()), (p.clone(), s.clone()), (s.clone(), p.clone()), (s.clone(), s.clone())];
        let missing: Vec<String> = required
            .iter()
            .filter(|k| !blocks.contains_key(*k))
            .map(|(a, b)| format!("('{a}', '{b}')"))
            .collect();
        if !missing.is_empty() {
            return Err(err(format!("provider block contract is incomplete: {}", missing.join(", "))));
        }
        let schur = self.contract.schur_approximation(state, design, &p, &s)?;
        let mut take =
            |k: &BlockKey| blocks.remove(k).ok_or_else(|| err("provider block contract is incomplete"));
        let a = take(&required[0])?;
        let b = take(&required[1])?;
        let c = take(&required[2])?;
        let d = take(&required[3])?;
        BlockSchurPreconditioner::new(self.name(), layout, &p, &s, a, b, c, d, schur, self.options)
    }
}

