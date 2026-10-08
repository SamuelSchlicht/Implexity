// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END




use std::sync::Mutex;

use serde_json::{Value, json};

use implexity_core::json::{DumpOptions, dumps, sha256_hex};
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::ilu::{Ilu, IluOptions};
use implexity_linalg::sparse::{CscMatrix, CsrMatrix};
use implexity_solve::exact_matrix::ExactMatrixIdentity;
use implexity_solve::native_history::HistoryPoint;
use implexity_solve::newton_krylov::{ExactKrylovPolicy, KrylovMethod};
use implexity_solve::operation_context::{ContextRequirement, require_operation_context};
use implexity_solve::preconditioner_lease::{
    ExactPreconditionerBinding, ExactPreconditionerLeaseBudget, Preconditioner, ProviderPreconditioner,
    SealedMatrixReadLease,
};

pub const PROFILE_SCHEMA: &str = "implexity-native-unified-history-exact-solver/1";
pub const DIRECT_POLICY: &str = "exact_direct_default";
pub const DIAGONAL_KRYLOV_POLICY: &str = "exact_krylov_bounded_diagonal_v1";
pub const SPARSE_ILU_KRYLOV_POLICY: &str = "exact_krylov_bounded_sparse_ilu_v1";
const DIAGONAL_PRECONDITIONER: &str = "bounded_signed_diagonal_v1";
const SPARSE_ILU_PRECONDITIONER: &str = "bounded_sparse_ilu_v1";
const SPARSE_ILU_DROP_TOLERANCE: f64 = 3.0e-3;
const SPARSE_ILU_TARGET_FILL_FACTOR: f64 = 5.0;
const SPARSE_ILU_MINIMUM_FILL_FACTOR: f64 = 5.0;
const SPARSE_FACTOR_BYTES_PER_NONZERO: usize = 16;
const MATRIX_FREE_BOOTSTRAP_PROBES: usize = 4;
const MATRIX_FREE_BOOTSTRAP_PRECONDITIONER: &str = "matrix_free_row_equilibration_v1";
const MATRIX_FREE_MAC_SIMPLE_PRECONDITIONER: &str = "matrix_free_mac_simple_schur_v1";
const MAC_SIMPLE_PRESSURE_REGULARIZATION: f64 = 1.0e-10;
const MAC_SIMPLE_DROP_TOLERANCE: f64 = 1.0e-3;
const MAC_SIMPLE_TARGET_FILL_FACTOR: f64 = 4.0;
const MAC_SIMPLE_MINIMUM_FILL_FACTOR: f64 = 1.25;
const MAC_SIMPLE_FACTOR_BYTES_PER_NONZERO: usize = 24;

fn contract<T>(message: impl Into<String>) -> CaeResult<T> {
    Err(CaeError::contract(message))
}

fn lease_error(message: &str) -> CaeError {
    CaeError::contract(message)
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnifiedHistoryExactProfile {
    pub solver_policy: String,
    pub method: String,
    pub rtol: f64,
    pub atol: f64,
    pub residual_limit: f64,
    pub maxiter: usize,
    pub restart: usize,
    pub inner_dimension: usize,
    pub recycle_dimension: usize,
    pub condition_safety_fraction: f64,
    pub maximum_workspace_bytes: usize,
    pub maximum_preconditioner_bytes: usize,
}

impl Default for UnifiedHistoryExactProfile {
    fn default() -> Self {
        Self {
            solver_policy: DIRECT_POLICY.into(),
            method: "gcrotmk".into(),
            rtol: 1.0e-9,
            atol: 0.0,
            residual_limit: 1.0e-8,
            maxiter: 200,
            restart: 40,
            inner_dimension: 40,
            recycle_dimension: 20,
            condition_safety_fraction: 0.5,
            maximum_workspace_bytes: 512 * 1024 * 1024,
            maximum_preconditioner_bytes: 1024 * 1024 * 1024,
        }
    }
}

impl UnifiedHistoryExactProfile {
    #[must_use]
    pub fn direct() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn accelerated() -> Self {
        Self {
            solver_policy: SPARSE_ILU_KRYLOV_POLICY.into(),
            maxiter: 2,
            restart: 24,
            inner_dimension: 24,
            recycle_dimension: 8,
            ..Self::default()
        }
    }


    pub fn validated(self) -> CaeResult<Self> {
        if ![DIRECT_POLICY, DIAGONAL_KRYLOV_POLICY, SPARSE_ILU_KRYLOV_POLICY]
            .contains(&self.solver_policy.as_str())
        {
            return contract("native unified-history exact solver policy is unsupported");
        }
        if !["gcrotmk", "gmres"].contains(&self.method.as_str()) {
            return contract("native unified-history exact Krylov method is unsupported");
        }
        for (name, value, positive) in [
            ("rtol", self.rtol, true),
            ("atol", self.atol, false),
            ("residual_limit", self.residual_limit, true),
            ("condition_safety_fraction", self.condition_safety_fraction, true),
        ] {
            if !value.is_finite() || (positive && value <= 0.0) || (!positive && value < 0.0) {
                return contract(format!("native exact {name} has an invalid numeric value"));
            }
        }
        for (name, value) in [
            ("maxiter", self.maxiter),
            ("restart", self.restart),
            ("inner_dimension", self.inner_dimension),
            ("recycle_dimension", self.recycle_dimension),
            ("maximum_workspace_bytes", self.maximum_workspace_bytes),
            ("maximum_preconditioner_bytes", self.maximum_preconditioner_bytes),
        ] {
            if value < 1 {
                return contract(format!("native exact {name} must be a positive integer"));
            }
        }
        self.policy_for(self.enabled()).validate()?;
        Ok(self)
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.solver_policy != DIRECT_POLICY
    }

    #[must_use]
    pub fn uses_sparse_ilu(&self) -> bool {
        self.solver_policy == SPARSE_ILU_KRYLOV_POLICY
    }

    #[must_use]
    pub fn preconditioner_name(&self) -> &'static str {
        if !self.enabled() {
            "none"
        } else if self.uses_sparse_ilu() {
            SPARSE_ILU_PRECONDITIONER
        } else {
            DIAGONAL_PRECONDITIONER
        }
    }

    #[must_use]
    pub fn to_wire(&self) -> Value {
        json!({
            "schema": PROFILE_SCHEMA,
            "solver_policy": self.solver_policy,
            "method": self.method,
            "rtol": self.rtol,
            "atol": self.atol,
            "residual_limit": self.residual_limit,
            "maxiter": self.maxiter,
            "restart": self.restart,
            "inner_dimension": self.inner_dimension,
            "recycle_dimension": self.recycle_dimension,
            "condition_safety_fraction": self.condition_safety_fraction,
            "maximum_workspace_bytes": self.maximum_workspace_bytes,
            "preconditioner": self.preconditioner_name(),
            "maximum_preconditioner_bytes": self.maximum_preconditioner_bytes,
        })
    }

    #[must_use]
    pub fn sha256(&self) -> String {
        sha256_hex(dumps(&self.to_wire(), &DumpOptions::canonical()).as_bytes())
    }


    pub fn from_wire(raw: &Value) -> CaeResult<Self> {
        let keys = [
            "schema",
            "solver_policy",
            "method",
            "rtol",
            "atol",
            "residual_limit",
            "maxiter",
            "restart",
            "inner_dimension",
            "recycle_dimension",
            "condition_safety_fraction",
            "maximum_workspace_bytes",
            "preconditioner",
            "maximum_preconditioner_bytes",
        ];
        let Some(m) =
            raw.as_object().filter(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
        else {
            return contract("native unified-history exact solver profile is not canonical");
        };
        if m["schema"].as_str() != Some(PROFILE_SCHEMA) {
            return contract("native unified-history exact solver profile schema is unsupported");
        }
        let text = |k: &str| m[k].as_str().map(str::to_string);
        let real = |k: &str| -> CaeResult<f64> {
            match &m[k] {
                Value::Number(n) => Ok(n.as_f64().unwrap_or(f64::NAN)),
                _ => contract(format!("native exact {k} must be finite numeric")),
            }
        };
        let int = |k: &str| -> CaeResult<usize> {
            match &m[k] {
                Value::Number(n) if n.is_u64() => usize::try_from(n.as_u64().unwrap_or(0))
                    .map_err(|_| CaeError::contract(format!("native exact {k} must be a positive integer"))),
                _ => contract(format!("native exact {k} must be a positive integer")),
            }
        };
        let (Some(solver_policy), Some(method)) = (text("solver_policy"), text("method")) else {
            return contract("native unified-history exact solver policy is unsupported");
        };
        let profile = Self {
            solver_policy,
            method,
            rtol: real("rtol")?,
            atol: real("atol")?,
            residual_limit: real("residual_limit")?,
            maxiter: int("maxiter")?,
            restart: int("restart")?,
            inner_dimension: int("inner_dimension")?,
            recycle_dimension: int("recycle_dimension")?,
            condition_safety_fraction: real("condition_safety_fraction")?,
            maximum_workspace_bytes: int("maximum_workspace_bytes")?,
            maximum_preconditioner_bytes: int("maximum_preconditioner_bytes")?,
        }
        .validated()?;
        if m["preconditioner"].as_str() != Some(profile.preconditioner_name()) {
            return contract("native unified-history exact preconditioner profile is inconsistent");
        }
        let exact = implexity_core::parity::JsonCompare {
            rtol: 0.0,
            atol: 0.0,
            ignore_keys: Vec::new(),
            int_float_equal: true,
        };
        if implexity_core::parity::json_equal(&profile.to_wire(), raw, &exact).is_err() {
            return contract("native unified-history exact solver profile is noncanonical");
        }
        Ok(profile)
    }

    fn policy_for(&self, enabled: bool) -> ExactKrylovPolicy {
        ExactKrylovPolicy {
            enabled,
            method: if self.method == "gmres" { KrylovMethod::Gmres } else { KrylovMethod::Gcrotmk },
            rtol: self.rtol,
            atol: self.atol,
            residual_limit: self.residual_limit,
            maxiter: self.maxiter,
            restart: self.restart,
            inner_dimension: self.inner_dimension,
            recycle_dimension: self.recycle_dimension,
            condition_safety_fraction: self.condition_safety_fraction,
            maximum_workspace_bytes: self.maximum_workspace_bytes,
        }
    }

    #[must_use]
    pub fn krylov_policy(&self) -> Option<ExactKrylovPolicy> {
        self.enabled().then(|| self.policy_for(true))
    }


    pub fn lease_budget(&self, state_size: usize) -> CaeResult<Option<ExactPreconditionerLeaseBudget>> {
        if !self.enabled() {
            return Ok(None);
        }
        if state_size < 1 {
            return contract("native exact preconditioner state size must be a positive integer");
        }
        let diagonal_bytes = state_size * 8;
        if diagonal_bytes > self.maximum_preconditioner_bytes {
            return contract("native exact preconditioner exceeds its retained-byte limit");
        }
        if self.uses_sparse_ilu() {
            return Ok(Some(ExactPreconditionerLeaseBudget::new(
                state_size.max(self.maximum_preconditioner_bytes / 4),
                self.maximum_preconditioner_bytes,
            )?));
        }
        Ok(Some(ExactPreconditionerLeaseBudget::new(state_size, diagonal_bytes)?))
    }
}

pub struct PreparedSignedDiagonal {
    matrix_identity: ExactMatrixIdentity,
    scope_digest: String,
    shape: (usize, usize),
    inverse: Mutex<Option<Vec<f64>>>,
    pub regularized_entries: usize,
    pub retained_bytes: usize,
    pub exact_profile_sha256: String,
    pub ilu_fallback_reason: Option<String>,
}

impl PreparedSignedDiagonal {
    #[must_use]
    pub fn preconditioner_kind(&self) -> &'static str {
        DIAGONAL_PRECONDITIONER
    }
    fn act(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        let guard = self
            .inverse
            .lock()
            .map_err(|_| lease_error("native exact diagonal preconditioner is released"))?;
        let inverse =
            guard.as_ref().ok_or_else(|| lease_error("native exact diagonal preconditioner is released"))?;
        Ok(inverse.iter().zip(value).map(|(i, v)| i * v).collect())
    }
}

impl Preconditioner for PreparedSignedDiagonal {
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value)
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value)
    }
    fn release(&self) -> CaeResult<()> {
        if let Ok(mut g) = self.inverse.lock() {
            *g = None;
        }
        Ok(())
    }
}

impl ProviderPreconditioner for PreparedSignedDiagonal {
    fn matrix_identity(&self) -> ExactMatrixIdentity {
        self.matrix_identity.clone()
    }
    fn scope_digest(&self) -> String {
        self.scope_digest.clone()
    }
}

pub struct PreparedSparseIlu {
    matrix_identity: ExactMatrixIdentity,
    scope_digest: String,
    shape: (usize, usize),
    factor: Mutex<Option<Ilu>>,
    pub retained_bytes: usize,
    pub factor_nonzeros: usize,
    pub matrix_nonzeros: usize,
    pub effective_fill_factor: f64,
    pub drop_tolerance: f64,
    pub exact_profile_sha256: String,
}

impl PreparedSparseIlu {
    #[must_use]
    pub fn preconditioner_kind(&self) -> &'static str {
        SPARSE_ILU_PRECONDITIONER
    }
    fn act(&self, value: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
        let guard = self
            .factor
            .lock()
            .map_err(|_| lease_error("native exact sparse ILU preconditioner is released"))?;
        let f = guard
            .as_ref()
            .ok_or_else(|| lease_error("native exact sparse ILU preconditioner is released"))?;
        f.solve(value, transpose).map_err(|e| CaeError::contract(e.to_string()))
    }
}

impl Preconditioner for PreparedSparseIlu {
    fn shape(&self) -> (usize, usize) {
        self.shape
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value, false)
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value, true)
    }
    fn release(&self) -> CaeResult<()> {
        if let Ok(mut g) = self.factor.lock() {
            *g = None;
        }
        Ok(())
    }
}

impl ProviderPreconditioner for PreparedSparseIlu {
    fn matrix_identity(&self) -> ExactMatrixIdentity {
        self.matrix_identity.clone()
    }
    fn scope_digest(&self) -> String {
        self.scope_digest.clone()
    }
}

fn checked_action(value: &[f64], size: usize, label: &str) -> CaeResult<()> {
    if value.len() != size || value.iter().any(|v| !v.is_finite()) {
        return contract(format!("{label} must be a finite real vector with shape ({size},)"));
    }
    Ok(())
}

pub struct PreparedMatrixFreeRowEquilibration {
    inverse_scale: Mutex<Option<Vec<f64>>>,
    size: usize,
    pub retained_bytes: usize,
    pub probe_count: usize,
    pub regularized_entries: usize,
}

impl PreparedMatrixFreeRowEquilibration {
    #[must_use]
    pub fn preconditioner_kind(&self) -> &'static str {
        MATRIX_FREE_BOOTSTRAP_PRECONDITIONER
    }
    fn act(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        let guard = self
            .inverse_scale
            .lock()
            .map_err(|_| lease_error("native exact matrix-free bootstrap preconditioner is released"))?;
        let s = guard
            .as_ref()
            .ok_or_else(|| lease_error("native exact matrix-free bootstrap preconditioner is released"))?;
        if value.len() != self.size || value.iter().any(|v| !v.is_finite()) {
            return contract("native exact matrix-free bootstrap input is invalid");
        }
        let out: Vec<f64> = s.iter().zip(value).map(|(a, b)| a * b).collect();
        if out.iter().any(|v| !v.is_finite()) {
            return contract("native exact matrix-free bootstrap action is nonfinite");
        }
        Ok(out)
    }
}

impl Preconditioner for PreparedMatrixFreeRowEquilibration {
    fn shape(&self) -> (usize, usize) {
        (self.size, self.size)
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value)
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        self.act(value)
    }
    fn release(&self) -> CaeResult<()> {
        if let Ok(mut g) = self.inverse_scale.lock() {
            *g = None;
        }
        Ok(())
    }
}

#[must_use]
pub fn deterministic_rademacher(size: usize, probe: usize) -> Vec<f64> {
    let seed = ((probe as u64) + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    (0..size as u64)
        .map(|i| {
            let mut v = i.wrapping_add(seed);
            v ^= v >> 30;
            v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            v ^= v >> 27;
            v = v.wrapping_mul(0x94D0_49BB_1331_11EB);
            v ^= v >> 31;
            if v & 1 == 1 { 1.0 } else { -1.0 }
        })
        .collect()
}

pub type Matvec<'a> = dyn Fn(&[f64]) -> CaeResult<Vec<f64>> + 'a;

fn estimate_row_norms(
    matvec: &Matvec<'_>,
    state_size: usize,
    maximum_preconditioner_bytes: usize,
    probe_count: usize,
) -> CaeResult<(Vec<f64>, f64, usize, usize)> {
    if state_size < 1 {
        return contract("native exact matrix-free bootstrap state size must be a positive integer");
    }
    if probe_count < 1 {
        return contract("native exact matrix-free bootstrap probe count must be a positive integer");
    }
    if state_size * 8 > maximum_preconditioner_bytes {
        return contract("native exact matrix-free bootstrap exceeds its retained-byte limit");
    }
    let mut energy = vec![0.0; state_size];
    for probe in 0..probe_count {
        let image = matvec(&deterministic_rademacher(state_size, probe))
            .map_err(|_| lease_error("native exact matrix-free bootstrap JVP failed"))?;
        if image.len() != state_size || image.iter().any(|v| !v.is_finite()) {
            return contract("native exact matrix-free bootstrap JVP is invalid");
        }
        for (e, v) in energy.iter_mut().zip(&image) {
            *e += v * v;
        }
    }
    for e in &mut energy {
        *e = (*e / probe_count as f64).sqrt();
    }
    let reference = energy.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !reference.is_finite() || reference <= 0.0 {
        return contract("native exact matrix-free bootstrap has no finite nonzero scale");
    }
    let floor = f64::MIN_POSITIVE.max(reference * f64::EPSILON.sqrt());
    let mut regularized = 0;
    for e in &mut energy {
        if *e < floor {
            *e = reference;
            regularized += 1;
        }
    }
    Ok((energy, reference, probe_count, regularized))
}


pub fn prepare_matrix_free_row_equilibration(
    matvec: &Matvec<'_>,
    state_size: usize,
    maximum_preconditioner_bytes: usize,
) -> CaeResult<PreparedMatrixFreeRowEquilibration> {
    let (mut rows, reference, probe_count, regularized) =
        estimate_row_norms(matvec, state_size, maximum_preconditioner_bytes, MATRIX_FREE_BOOTSTRAP_PROBES)?;
    for r in &mut rows {
        *r = reference / *r;
    }
    if rows.iter().any(|v| !v.is_finite()) {
        return contract("native exact matrix-free bootstrap produced invalid coefficients");
    }
    Ok(PreparedMatrixFreeRowEquilibration {
        size: state_size,
        retained_bytes: state_size * 8,
        inverse_scale: Mutex::new(Some(rows)),
        probe_count,
        regularized_entries: regularized,
    })
}


pub fn build_scaled_mac_pressure_coupling(
    grid: [usize; 3],
    face_maps: &[Vec<i64>; 3],
    cell_edges_m: [f64; 3],
    length_scale_m: f64,
) -> CaeResult<(CsrMatrix, CscMatrix)> {
    if grid.contains(&0) {
        return contract("native exact MAC/SIMPLE grid must contain three positive integers");
    }
    if cell_edges_m.iter().any(|e| !e.is_finite() || *e <= 0.0) {
        return contract("native exact MAC/SIMPLE cell edges must be three positive lengths");
    }
    if !length_scale_m.is_finite() || length_scale_m <= 0.0 {
        return contract("native exact MAC/SIMPLE length scale has an invalid numeric value");
    }
    let mut ids = Vec::new();
    for (axis, map) in face_maps.iter().enumerate() {
        let mut shape = grid;
        shape[axis] += 1;
        if map.len() != shape.iter().product::<usize>() || map.iter().any(|v| *v < -1) {
            return contract("native exact MAC/SIMPLE face map is invalid");
        }
        ids.extend(map.iter().copied().filter(|v| *v >= 0));
    }
    if ids.is_empty() {
        return contract("native exact MAC/SIMPLE face maps have no retained velocities");
    }
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != ids.len()
        || sorted.iter().enumerate().any(|(i, v)| usize::try_from(*v).ok() != Some(i))
    {
        return contract("native exact MAC/SIMPLE retained face IDs must be unique and contiguous");
    }
    let nv = sorted.len();
    let nc: usize = grid.iter().product();
    let volume = cell_edges_m[0] * cell_edges_m[1] * cell_edges_m[2];
    let (mut rr, mut cc, mut vv) = (Vec::new(), Vec::new(), Vec::new());
    for (axis, map) in face_maps.iter().enumerate() {
        let mut shape = grid;
        shape[axis] += 1;
        let scale = (volume / cell_edges_m[axis]) / (length_scale_m * length_scale_m);
        for (offset, sign) in [(0usize, -1.0), (1, 1.0)] {
            for c in 0..nc {
                let cell = [c / (grid[1] * grid[2]), (c / grid[2]) % grid[1], c % grid[2]];
                let mut face = cell;
                face[axis] += offset;
                let id = map[(face[0] * shape[1] + face[1]) * shape[2] + face[2]];
                if let Ok(v) = usize::try_from(id) {
                    rr.push(c);
                    cc.push(v);
                    vv.push(sign * scale);
                }
            }
        }
    }
    let d = CsrMatrix::from_triplets(nc, nv, &rr, &cc, &vv).map_err(|e| CaeError::contract(e.to_string()))?;
    let d = implexity_solve::matrix::eliminate_zeros(&d);
    if (0..nc).any(|i| d.indptr()[i + 1] == d.indptr()[i]) {
        return contract("native exact MAC/SIMPLE face maps leave an uncoupled cell");
    }
    let neg = CsrMatrix::from_triplets(nc, nv, &rr, &cc, &vv.iter().map(|v| -v).collect::<Vec<_>>())
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let gradient = neg.into_transpose().to_csr().to_csc();
    Ok((d, gradient))
}

pub struct PreparedMatrixFreeMacSimpleSchur {
    inner: Mutex<Option<SimpleParts>>,
    size: usize,
    pub retained_bytes: usize,
    pub probe_count: usize,
    pub regularized_entries: usize,
    pub pressure_size: usize,
    pub pressure_schur_nonzeros: usize,
    pub pressure_factor_nonzeros: usize,
    pub pressure_regularization: f64,
    pub effective_fill_factor: f64,
    pub drop_tolerance: f64,
}

struct SimpleParts {
    inverse_scale: Vec<f64>,
    velocity: Vec<usize>,
    pressure: Vec<usize>,
    divergence: CsrMatrix,
    gradient: CscMatrix,
    factor: Ilu,
}

impl PreparedMatrixFreeMacSimpleSchur {
    #[must_use]
    pub fn preconditioner_kind(&self) -> &'static str {
        MATRIX_FREE_MAC_SIMPLE_PRECONDITIONER
    }
    fn with<T>(&self, f: impl FnOnce(&SimpleParts) -> CaeResult<T>) -> CaeResult<T> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| lease_error("native exact matrix-free MAC/SIMPLE preconditioner is released"))?;
        let parts = guard
            .as_ref()
            .ok_or_else(|| lease_error("native exact matrix-free MAC/SIMPLE preconditioner is released"))?;
        f(parts)
    }
}

fn pressure_solve(p: &SimpleParts, value: &[f64], transpose: bool) -> CaeResult<Vec<f64>> {
    let r = p
        .factor
        .solve(value, transpose)
        .map_err(|_| lease_error("native exact matrix-free MAC/SIMPLE pressure action failed"))?;
    if r.len() != p.pressure.len() || r.iter().any(|v| !v.is_finite()) {
        return contract("native exact matrix-free MAC/SIMPLE pressure action is invalid");
    }
    Ok(r)
}

impl Preconditioner for PreparedMatrixFreeMacSimpleSchur {
    fn shape(&self) -> (usize, usize) {
        (self.size, self.size)
    }
    fn apply(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        checked_action(value, self.size, "native exact matrix-free MAC/SIMPLE input")?;
        self.with(|p| {
            let iv: Vec<f64> = p.velocity.iter().map(|i| p.inverse_scale[*i]).collect();
            let y: Vec<f64> = p.velocity.iter().zip(&iv).map(|(i, s)| s * value[*i]).collect();
            let dy = p.divergence.matvec(&y).map_err(|e| CaeError::contract(e.to_string()))?;
            let rhs: Vec<f64> = dy.iter().zip(&p.pressure).map(|(d, i)| d - value[*i]).collect();
            let pressure = pressure_solve(p, &rhs, false)?;
            let gp = p.gradient.matvec(&pressure).map_err(|e| CaeError::contract(e.to_string()))?;
            let mut out: Vec<f64> = p.inverse_scale.iter().zip(value).map(|(s, v)| s * v).collect();
            for (k, i) in p.velocity.iter().enumerate() {
                out[*i] = y[k] - iv[k] * gp[k];
            }
            for (k, i) in p.pressure.iter().enumerate() {
                out[*i] = pressure[k];
            }
            if out.iter().any(|v| !v.is_finite()) {
                return contract("native exact matrix-free MAC/SIMPLE action is nonfinite");
            }
            Ok(out)
        })
    }
    fn apply_transpose(&self, value: &[f64]) -> CaeResult<Vec<f64>> {
        checked_action(value, self.size, "native exact matrix-free MAC/SIMPLE transpose input")?;
        self.with(|p| {
            let iv: Vec<f64> = p.velocity.iter().map(|i| p.inverse_scale[*i]).collect();
            let va: Vec<f64> = p.velocity.iter().map(|i| value[*i]).collect();
            let weighted: Vec<f64> = iv.iter().zip(&va).map(|(s, v)| s * v).collect();
            let gt = p.gradient.matvec_transpose(&weighted).map_err(|e| CaeError::contract(e.to_string()))?;
            let rhs: Vec<f64> = p.pressure.iter().zip(&gt).map(|(i, g)| value[*i] - g).collect();
            let pa = pressure_solve(p, &rhs, true)?;
            let dt = p.divergence.matvec_transpose(&pa).map_err(|e| CaeError::contract(e.to_string()))?;
            let mut out: Vec<f64> = p.inverse_scale.iter().zip(value).map(|(s, v)| s * v).collect();
            for (k, i) in p.velocity.iter().enumerate() {
                out[*i] = iv[k] * (va[k] + dt[k]);
            }
            for (k, i) in p.pressure.iter().enumerate() {
                out[*i] = -pa[k];
            }
            if out.iter().any(|v| !v.is_finite()) {
                return contract("native exact matrix-free MAC/SIMPLE transpose action is nonfinite");
            }
            Ok(out)
        })
    }
    fn release(&self) -> CaeResult<()> {
        if let Ok(mut g) = self.inner.lock() {
            *g = None;
        }
        Ok(())
    }
}

fn checked_partition(indices: &[usize], state_size: usize, label: &str) -> CaeResult<()> {
    let mut sorted = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if indices.is_empty() || sorted.len() != indices.len() || indices.iter().any(|i| *i >= state_size) {
        return contract(format!("{label} must contain unique in-range state indices"));
    }
    Ok(())
}

fn csr_bytes(m: &CsrMatrix) -> usize {
    m.nnz() * 12 + (m.nrows() + 1) * 4
}


#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn prepare_matrix_free_mac_simple_schur(
    matvec: &Matvec<'_>,
    state_size: usize,
    velocity_indices: &[usize],
    pressure_indices: &[usize],
    divergence: &CsrMatrix,
    gradient: &CscMatrix,
    maximum_preconditioner_bytes: usize,
) -> CaeResult<PreparedMatrixFreeMacSimpleSchur> {
    let relative = MAC_SIMPLE_PRESSURE_REGULARIZATION;
    checked_partition(velocity_indices, state_size, "native exact matrix-free MAC/SIMPLE velocity indices")?;
    checked_partition(pressure_indices, state_size, "native exact matrix-free MAC/SIMPLE pressure indices")?;
    if velocity_indices.iter().any(|v| pressure_indices.contains(v)) {
        return contract("native exact matrix-free MAC/SIMPLE partitions overlap");
    }
    let (nv, np) = (velocity_indices.len(), pressure_indices.len());
    if divergence.shape() != (np, nv) || divergence.nnz() < 1 || !divergence.is_finite() {
        return contract(format!(
            "native exact matrix-free MAC/SIMPLE divergence must be a finite nonempty sparse operator with shape ({np}, {nv})"
        ));
    }
    if gradient.shape() != (nv, np) || gradient.nnz() < 1 || !gradient.is_finite() {
        return contract(format!(
            "native exact matrix-free MAC/SIMPLE gradient must be a finite nonempty sparse operator with shape ({nv}, {np})"
        ));
    }
    if (0..np).any(|i| divergence.indptr()[i + 1] == divergence.indptr()[i])
        || (0..np).any(|j| gradient.indptr()[j + 1] == gradient.indptr()[j])
    {
        return contract("native exact matrix-free MAC/SIMPLE has an uncoupled pressure row");
    }
    let (mut rows, _reference, probe_count, regularized) =
        estimate_row_norms(matvec, state_size, maximum_preconditioner_bytes, MATRIX_FREE_BOOTSTRAP_PROBES)?;
    for r in &mut rows {
        *r = 1.0 / *r;
    }
    if rows.iter().any(|v| !v.is_finite()) {
        return contract("native exact matrix-free MAC/SIMPLE produced invalid row scales");
    }
    let iv: Vec<f64> = velocity_indices.iter().map(|i| rows[*i]).collect();
    let weighted = divergence.scaled(&vec![1.0; np], &iv).map_err(|e| CaeError::contract(e.to_string()))?;
    let schur = weighted.matmul(&gradient.to_csr()).map_err(|e| CaeError::contract(e.to_string()))?;
    let schur = implexity_solve::matrix::eliminate_zeros(&schur);
    if schur.shape() != (np, np) || schur.nnz() < 1 || !schur.is_finite() {
        return contract("native exact matrix-free MAC/SIMPLE pressure Schur is invalid");
    }
    let diagonal = schur.diagonal();
    let scale = diagonal.iter().map(|d| d.abs()).fold(0.0, f64::max);
    if !scale.is_finite() || scale <= 0.0 {
        return contract("native exact matrix-free MAC/SIMPLE pressure Schur has no diagonal scale");
    }
    let mut nonzero: Vec<f64> = diagonal.iter().copied().filter(|d| d.abs() > scale * f64::EPSILON).collect();
    nonzero.sort_by(f64::total_cmp);
    let median = if nonzero.is_empty() {
        0.0
    } else if nonzero.len() % 2 == 1 {
        nonzero[nonzero.len() / 2]
    } else {
        0.5 * (nonzero[nonzero.len() / 2 - 1] + nonzero[nonzero.len() / 2])
    };
    let sign = if nonzero.is_empty() || median >= 0.0 { 1.0 } else { -1.0 };
    let regularization = sign * relative * scale;
    let schur = schur
        .add_scaled(1.0, &CsrMatrix::diagonal_matrix(&vec![regularization; np]), 1.0)
        .map_err(|e| CaeError::contract(e.to_string()))?;
    let schur = implexity_solve::matrix::eliminate_zeros(&schur);
    let base_bytes =
        rows.len() * 8 + (nv + np) * 8 + csr_bytes(divergence) + gradient.nnz() * 12 + (np + 1) * 4;
    let schur_bytes = csr_bytes(&schur);
    let fixed = 4 * np * 4;
    let available = maximum_preconditioner_bytes.saturating_sub(base_bytes + schur_bytes + fixed);
    let max_nnz = available / MAC_SIMPLE_FACTOR_BYTES_PER_NONZERO;
    let effective = MAC_SIMPLE_TARGET_FILL_FACTOR.min(max_nnz as f64 / schur.nnz().max(1) as f64);
    if effective < MAC_SIMPLE_MINIMUM_FILL_FACTOR {
        return contract("native exact matrix-free MAC/SIMPLE byte limit cannot hold its pressure factor");
    }
    let factor = Ilu::new(
        &schur,
        &IluOptions { drop_tol: MAC_SIMPLE_DROP_TOLERANCE, fill_factor: effective, ..IluOptions::default() },
    )
    .map_err(|_| lease_error("native exact matrix-free MAC/SIMPLE pressure factorization failed"))?;
    let factor_bytes = factor.nbytes();
    let retained = base_bytes + factor_bytes;
    if retained > maximum_preconditioner_bytes || retained + schur_bytes > maximum_preconditioner_bytes {
        return contract("native exact matrix-free MAC/SIMPLE measured storage exceeds its byte limit");
    }
    Ok(PreparedMatrixFreeMacSimpleSchur {
        size: state_size,
        retained_bytes: retained,
        probe_count,
        regularized_entries: regularized,
        pressure_size: np,
        pressure_schur_nonzeros: schur.nnz(),
        pressure_factor_nonzeros: factor.nnz(),
        pressure_regularization: regularization,
        effective_fill_factor: effective,
        drop_tolerance: MAC_SIMPLE_DROP_TOLERANCE,
        inner: Mutex::new(Some(SimpleParts {
            inverse_scale: rows,
            velocity: velocity_indices.to_vec(),
            pressure: pressure_indices.to_vec(),
            divergence: divergence.clone(),
            gradient: gradient.clone(),
            factor,
        })),
    })
}

#[derive(Debug, Clone, Copy)]
pub struct PreparationFacts<'a> {
    pub expected_state_size: usize,
    pub expected_design_size: usize,
    pub history_steps: usize,
    pub exact_profile_sha256: &'a str,
    pub maximum_preconditioner_bytes: usize,
}

fn checked_preparation(
    lease: &SealedMatrixReadLease,
    binding: &ExactPreconditionerBinding,
    point: &HistoryPoint<'_>,
    facts: &PreparationFacts<'_>,
) -> CaeResult<()> {
    if point.history_step < 1 || point.history_step > facts.history_steps {
        return contract("native exact history step is outside its operation");
    }
    let context = require_operation_context(
        point.execution_context,
        ContextRequirement::default(),
        "native exact preconditioner operation context",
    )?;
    if context.scope_digest() != binding.scope_digest {
        return contract("native exact preconditioner scope disagrees with its matrix binding");
    }
    if *lease.identity()? != binding.matrix_identity
        || lease.shape()? != binding.shape
        || binding.shape != (facts.expected_state_size, facts.expected_state_size)
    {
        return contract("native exact preconditioner matrix identity or state shape drifted");
    }
    for (value, size, label) in [
        (point.state, facts.expected_state_size, "native exact current state"),
        (point.previous_state, facts.expected_state_size, "native exact previous-time state"),
        (point.design, facts.expected_design_size, "native exact design"),
    ] {
        checked_action(value, size, label)?;
    }
    Ok(())
}

fn signed_diagonal(
    binding: &ExactPreconditionerBinding,
    mut diagonal: Vec<f64>,
    facts: &PreparationFacts<'_>,
    fallback: Option<String>,
) -> CaeResult<PreparedSignedDiagonal> {
    if diagonal.len() * 8 > facts.maximum_preconditioner_bytes {
        return contract("native exact diagonal preconditioner exceeds its retained-byte limit");
    }
    let lo = diagonal.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = diagonal.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let reference = lo.abs().max(hi.abs());
    if !reference.is_finite() || reference <= 0.0 {
        return contract("native exact diagonal preconditioner has no finite nonzero scale");
    }
    let floor = f64::MIN_POSITIVE.max(reference * f64::EPSILON.sqrt());
    let mut regularized = 0;
    for d in &mut diagonal {
        let negative = *d < 0.0;
        let mut a = d.abs();
        if a < floor {
            a = reference;
            regularized += 1;
        }
        *d = if negative { -1.0 / a } else { 1.0 / a };
    }
    if diagonal.iter().any(|v| !v.is_finite()) {
        return contract("native exact diagonal preconditioner produced invalid coefficients");
    }
    Ok(PreparedSignedDiagonal {
        matrix_identity: binding.matrix_identity.clone(),
        scope_digest: binding.scope_digest.clone(),
        shape: binding.shape,
        retained_bytes: diagonal.len() * 8,
        inverse: Mutex::new(Some(diagonal)),
        regularized_entries: regularized,
        exact_profile_sha256: facts.exact_profile_sha256.to_string(),
        ilu_fallback_reason: fallback,
    })
}


pub fn prepare_bounded_diagonal(
    lease: &SealedMatrixReadLease,
    binding: &ExactPreconditionerBinding,
    point: &HistoryPoint<'_>,
    facts: &PreparationFacts<'_>,
) -> CaeResult<Box<dyn ProviderPreconditioner>> {
    checked_preparation(lease, binding, point, facts)?;
    let diagonal = lease.diagonal_copy()?;
    if diagonal.len() != facts.expected_state_size || diagonal.iter().any(|v| !v.is_finite()) {
        return contract("native exact diagonal preconditioner received invalid coefficients");
    }
    Ok(Box::new(signed_diagonal(binding, diagonal, facts, None)?))
}


pub fn prepare_bounded_sparse_ilu(
    lease: &SealedMatrixReadLease,
    binding: &ExactPreconditionerBinding,
    point: &HistoryPoint<'_>,
    facts: &PreparationFacts<'_>,
) -> CaeResult<Box<dyn ProviderPreconditioner>> {
    checked_preparation(lease, binding, point, facts)?;
    let matrix = lease.sparse_csc_copy()?;
    let size = facts.expected_state_size;
    if matrix.shape() != (size, size) {
        return contract("native exact sparse ILU matrix shape drifted");
    }
    let matrix_bytes = matrix.nnz() * 12 + (size + 1) * 4;
    let fixed = size * 8 + 4 * size * 4;
    let available = facts.maximum_preconditioner_bytes.saturating_sub(matrix_bytes + fixed);
    let max_nnz = available / SPARSE_FACTOR_BYTES_PER_NONZERO;
    let effective = SPARSE_ILU_TARGET_FILL_FACTOR.min(max_nnz as f64 / matrix.nnz().max(1) as f64);
    let csr = matrix.to_csr();
    let fallback = if effective >= SPARSE_ILU_MINIMUM_FILL_FACTOR {
        match Ilu::new(
            &csr,
            &IluOptions {
                drop_tol: SPARSE_ILU_DROP_TOLERANCE,
                fill_factor: effective,
                ..IluOptions::default()
            },
        ) {
            Ok(factor) => {
                let retained = factor.nnz() * SPARSE_FACTOR_BYTES_PER_NONZERO + fixed;
                if retained <= facts.maximum_preconditioner_bytes.saturating_sub(matrix_bytes) {
                    return Ok(Box::new(PreparedSparseIlu {
                        matrix_identity: binding.matrix_identity.clone(),
                        scope_digest: binding.scope_digest.clone(),
                        shape: binding.shape,
                        retained_bytes: retained,
                        factor_nonzeros: factor.nnz(),
                        matrix_nonzeros: matrix.nnz(),
                        effective_fill_factor: effective,
                        drop_tolerance: SPARSE_ILU_DROP_TOLERANCE,
                        exact_profile_sha256: facts.exact_profile_sha256.to_string(),
                        factor: Mutex::new(Some(factor)),
                    }));
                }
                "sparse_ilu_measured_factor_exceeds_memory_cap".to_string()
            }
            Err(e) => format!("sparse_ilu_numerical_failure:{e}"),
        }
    } else {
        "sparse_ilu_memory_cap_too_small".to_string()
    };
    let diagonal = csr.diagonal();
    Ok(Box::new(signed_diagonal(binding, diagonal, facts, Some(fallback))?))
}
