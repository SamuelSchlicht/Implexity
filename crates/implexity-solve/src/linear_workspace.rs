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



use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use implexity_core::error::{CaeError, CaeResult};
use serde_json::{Value, json};

use crate::exact_matrix::{AdmittedExactMatrix, ExactMatrixIdentity, MatrixInput, admit_exact_matrix};
use crate::factorization::{CertifiedSolve, Factorization};

pub trait CertifiedFactorization: Send + Sync {
    fn matrix(&self) -> &AdmittedExactMatrix;
    fn condition(&self) -> f64;
    fn kind(&self) -> &str;
    fn retained_bytes(&self) -> Option<usize>;


    fn solve_block(&self, b: &[f64], m: usize, transpose: bool) -> CaeResult<CertifiedSolve>;
}

impl CertifiedFactorization for Factorization {
    fn matrix(&self) -> &AdmittedExactMatrix {
        Factorization::matrix(self)
    }
    fn condition(&self) -> f64 {
        Factorization::condition(self)
    }
    fn kind(&self) -> &str {
        Factorization::kind(self)
    }
    fn retained_bytes(&self) -> Option<usize> {
        Some(Factorization::retained_bytes(self))
    }
    fn solve_block(&self, b: &[f64], m: usize, transpose: bool) -> CaeResult<CertifiedSolve> {
        Factorization::solve_block(self, b, m, transpose)
    }
}

#[derive(Clone)]
struct Entry {
    identity: ExactMatrixIdentity,
    factorization: Arc<dyn CertifiedFactorization>,
}

#[derive(Default)]
struct State {
    active: Option<u64>,
    committed: Option<Entry>,
    builds: u64,
    reuses: u64,
    commits: u64,
    rollbacks: u64,
    evictions: u64,
}

static TOKENS: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct ExactFactorizationWorkspace {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for ExactFactorizationWorkspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactFactorizationWorkspace").field("report", &self.report()).finish()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TransactionOptions {
    pub discard_committed_on_miss: bool,
    pub require_committed_match: bool,
}

fn checked_condition_limit(limit: f64) -> CaeResult<f64> {
    if !limit.is_finite() || limit <= 1.0 {
        return Err(CaeError::contract(
            "exact factorization condition limit must be finite and greater than one",
        ));
    }
    Ok(limit)
}

fn admit_factorization(
    factorization: &Arc<dyn CertifiedFactorization>,
    identity: &ExactMatrixIdentity,
    condition_limit: f64,
) -> CaeResult<()> {
    let condition = factorization.condition();

    if !condition.is_finite() || condition > condition_limit {
        return Err(CaeError::convergence(
            "exact factorization does not satisfy the requested condition limit",
        ));
    }
    if factorization.matrix().identity() != identity {
        return Err(CaeError::contract(
            "factorization matrix identity differs from the staged current matrix",
        ));
    }
    Ok(())
}

impl ExactFactorizationWorkspace {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }



    pub fn transaction<F>(
        &self,
        matrix: impl Into<MatrixInput>,
        size: usize,
        condition_limit: f64,
        build: F,
        options: TransactionOptions,
    ) -> CaeResult<ExactFactorizationTransaction>
    where
        F: FnOnce(&AdmittedExactMatrix) -> CaeResult<Arc<dyn CertifiedFactorization>>,
    {
        let admitted = admit_exact_matrix(matrix, Some(size))?;
        let identity = admitted.identity().clone();
        let condition_limit = checked_condition_limit(condition_limit)?;
        let token = TOKENS.fetch_add(1, Ordering::Relaxed);
        let committed = {
            let mut state = self.lock();
            if state.active.is_some() {
                return Err(CaeError::contract(
                    "exact factorization workspace already has an active transaction",
                ));
            }
            state.active = Some(token);
            let matches = state.committed.as_ref().is_some_and(|c| c.identity == identity);
            if options.require_committed_match && !matches {
                if options.discard_committed_on_miss && state.committed.take().is_some() {
                    state.evictions += 1;
                }
                state.active = None;
                return Err(CaeError::contract(
                    "current matrix identity does not match the committed exact factorization",
                ));
            }
            if !matches && options.discard_committed_on_miss && state.committed.take().is_some() {
                state.evictions += 1;
            }
            state.committed.clone()
        };
        let staged = (|| -> CaeResult<(Entry, bool)> {
            if let Some(c) = committed.filter(|c| c.identity == identity) {
                admit_factorization(&c.factorization, &identity, condition_limit)?;
                Ok((c, true))
            } else {
                let built = build(&admitted)?;
                admit_factorization(&built, &identity, condition_limit)?;
                Ok((Entry { identity: identity.clone(), factorization: built }, false))
            }
        })();
        let mut state = self.lock();
        match staged {
            Ok((entry, reused)) => {
                if reused {
                    state.reuses += 1;
                } else {
                    state.builds += 1;
                }
                drop(state);
                Ok(ExactFactorizationTransaction {
                    workspace: self.clone(),
                    token,
                    entry: Some(entry),
                    reused,
                })
            }
            Err(e) => {
                if state.active == Some(token) {
                    state.active = None;
                }
                Err(e)
            }
        }
    }

    fn finish(&self, token: u64, entry: Entry, commit: bool) -> CaeResult<()> {
        let mut state = self.lock();
        if state.active != Some(token) {
            return Err(CaeError::contract("exact factorization transaction ownership was lost"));
        }
        if commit {
            state.committed = Some(entry);
            state.commits += 1;
        } else {
            state.rollbacks += 1;
        }
        state.active = None;
        Ok(())
    }



    pub fn clear(&self) -> CaeResult<()> {
        let mut state = self.lock();
        if state.active.is_some() {
            return Err(CaeError::contract(
                "cannot clear exact factorization workspace during a transaction",
            ));
        }
        state.committed = None;
        Ok(())
    }

    #[must_use]
    pub fn transaction_active(&self) -> bool {
        self.lock().active.is_some()
    }

    #[must_use]
    pub fn committed(&self) -> bool {
        self.lock().committed.is_some()
    }

    #[must_use]
    pub fn report(&self) -> Value {
        let state = self.lock();
        let identity = state.committed.as_ref().map(|c| &c.identity);
        json!({
            "method": "single_committed_exact_matrix_factorization",
            "transaction_active": state.active.is_some(),
            "committed": identity.is_some(),
            "committed_matrix_sha256": identity.map(|i| i.sha256.clone()),
            "committed_matrix_size": identity.map(|i| i.size),
            "factorization_builds": state.builds,
            "factorization_reuses": state.reuses,
            "commits": state.commits,
            "rollbacks": state.rollbacks,
            "identity_miss_evictions": state.evictions,
            "maximum_committed_factorizations": 1,
            "solve_certification": "delegated_to_exact_factorization_per_rhs",
        })
    }
}

pub struct ExactFactorizationTransaction {
    workspace: ExactFactorizationWorkspace,
    token: u64,
    entry: Option<Entry>,
    reused: bool,
}

impl std::fmt::Debug for ExactFactorizationTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExactFactorizationTransaction")
            .field("active", &self.entry.is_some())
            .field("reused", &self.reused)
            .finish_non_exhaustive()
    }
}

impl ExactFactorizationTransaction {
    fn active(&self) -> CaeResult<&Entry> {
        self.entry
            .as_ref()
            .ok_or_else(|| CaeError::contract("exact factorization transaction is no longer active"))
    }

    #[must_use]
    pub fn reused(&self) -> bool {
        self.reused
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.entry.is_some()
    }



    pub fn identity(&self) -> CaeResult<&ExactMatrixIdentity> {
        Ok(&self.active()?.identity)
    }



    pub fn condition(&self) -> CaeResult<f64> {
        Ok(self.active()?.factorization.condition())
    }



    pub fn kind(&self) -> CaeResult<String> {
        Ok(self.active()?.factorization.kind().to_string())
    }



    pub fn retained_bytes(&self) -> CaeResult<Option<usize>> {
        Ok(self.active()?.factorization.retained_bytes())
    }



    pub fn factorization(&self) -> CaeResult<Arc<dyn CertifiedFactorization>> {
        Ok(Arc::clone(&self.active()?.factorization))
    }

    pub fn solve(&self, b: &[f64], transpose: bool) -> CaeResult<(Vec<f64>, f64, f64)> {
        let r = self.active()?.factorization.solve_block(b, 1, transpose)?;
        Ok((r.solution, r.error_norms[0], r.relative[0]))
    }



    pub fn solve_block(&self, b: &[f64], m: usize, transpose: bool) -> CaeResult<CertifiedSolve> {
        self.active()?.factorization.solve_block(b, m, transpose)
    }



    pub fn commit(&mut self) -> CaeResult<()> {
        let entry = self
            .entry
            .take()
            .ok_or_else(|| CaeError::contract("exact factorization transaction is no longer active"))?;
        self.workspace.finish(self.token, entry, true)
    }



    pub fn rollback(&mut self) -> CaeResult<()> {
        match self.entry.take() {
            Some(entry) => self.workspace.finish(self.token, entry, false),
            None => Ok(()),
        }
    }
}

impl Drop for ExactFactorizationTransaction {
    fn drop(&mut self) {
        let _ = self.rollback();
    }
}

