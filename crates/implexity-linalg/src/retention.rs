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




use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use serde::Serialize;

use crate::error::LinalgError;

pub const DEFAULT_RETAINED_FACTORIZATION_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub const RETAINED_FACTORIZATION_BUDGET_ENV: &str = "IMPLEXITY_RETAINED_FACTORIZATION_BUDGET_BYTES";

pub trait RetainedFactorizationOwner: Send + Sync {
    fn release_retained_factorization(&self, slot: i64) -> bool;
}

struct Entry {
    owner: Weak<dyn RetainedFactorizationOwner>,
    nbytes: u64,
    sequence: u64,
}

#[derive(Default)]
struct State {
    budget: u64,
    entries: BTreeMap<(usize, i64), Entry>,
    sequence: u64,
    admissions: u64,
    refusals: u64,
    evictions: u64,
}

impl State {
    fn purge_dead(&mut self) {
        self.entries.retain(|_, e| e.owner.strong_count() > 0);
    }

    fn used(&self) -> u64 {
        self.entries.values().map(|e| e.nbytes).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetentionReport {
    pub method: &'static str,
    pub budget_bytes: u64,
    pub retained_bytes: u64,
    pub retained_entries: usize,
    pub admissions: u64,
    pub refusals: u64,
    pub evictions: u64,
}

pub struct RetentionLedger {
    state: Mutex<State>,
}

impl core::fmt::Debug for RetentionLedger {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RetentionLedger").field("report", &self.report()).finish()
    }
}

fn owner_key(owner: &Arc<dyn RetainedFactorizationOwner>) -> usize {
    Arc::as_ptr(owner).cast::<()>() as usize
}

impl RetentionLedger {
    #[must_use]
    pub fn new(budget_bytes: u64) -> Self {
        Self { state: Mutex::new(State { budget: budget_bytes, ..State::default() }) }
    }



    pub fn from_env() -> Result<Self, LinalgError> {
        Ok(Self::new(budget_from_env_value(
            std::env::var(RETAINED_FACTORIZATION_BUDGET_ENV).ok().as_deref(),
        )?))
    }

    fn lock(&self) -> MutexGuard<'_, State> {

        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn budget_bytes(&self) -> u64 {
        self.lock().budget
    }

    pub fn set_budget(&self, budget_bytes: u64) {
        self.lock().budget = budget_bytes;
        self.evict_until(0, None);
    }

    fn evict_until(&self, incoming: u64, protected: Option<(usize, i64)>) -> bool {
        let mut tried: Vec<(usize, i64, u64)> = Vec::new();
        loop {
            let candidate = {
                let mut st = self.lock();
                st.purge_dead();
                if st.used() + incoming <= st.budget {
                    return true;
                }
                let mut order: Vec<(&(usize, i64), &Entry)> = st.entries.iter().collect();
                order.sort_by_key(|(_, e)| e.sequence);
                let next = order.into_iter().find(|(k, e)| {
                    Some(**k) != protected && !tried.iter().any(|t| (t.0, t.1) == **k && t.2 == e.sequence)
                });
                match next {
                    None => return false,
                    Some((k, e)) => (*k, e.sequence, e.owner.clone()),
                }
            };
            let (key, sequence, owner) = candidate;
            tried.push((key.0, key.1, sequence));
            let released = owner.upgrade().is_some_and(|o| o.release_retained_factorization(key.1));
            if released {
                let mut st = self.lock();
                if st.entries.get(&key).is_some_and(|e| e.sequence == sequence) {
                    st.entries.remove(&key);
                    st.evictions += 1;
                }
            }
        }
    }

    pub fn admit(
        &self,
        owner: &Arc<dyn RetainedFactorizationOwner>,
        slot: i64,
        nbytes: Option<u64>,
        guaranteed: bool,
    ) -> bool {
        let key = (owner_key(owner), slot);
        {
            let mut st = self.lock();
            st.entries.remove(&key);
            if nbytes.is_none() && !guaranteed {
                st.refusals += 1;
                return false;
            }
        }
        let nbytes = nbytes.unwrap_or(0);
        let fits = self.evict_until(nbytes, Some(key));
        let mut st = self.lock();
        if !fits && !guaranteed {
            st.refusals += 1;
            return false;
        }
        st.sequence += 1;
        let sequence = st.sequence;
        st.entries.insert(key, Entry { owner: Arc::downgrade(owner), nbytes, sequence });
        st.admissions += 1;
        true
    }

    pub fn release(&self, owner: &Arc<dyn RetainedFactorizationOwner>, slot: Option<i64>) {
        let marker = owner_key(owner);
        let mut st = self.lock();
        match slot {
            Some(s) => {
                st.entries.remove(&(marker, s));
            }
            None => st.entries.retain(|k, _| k.0 != marker),
        }
    }

    #[must_use]
    pub fn report(&self) -> RetentionReport {
        let mut st = self.lock();
        st.purge_dead();
        RetentionReport {
            method: "process_wide_oldest_first_byte_bounded_retention",
            budget_bytes: st.budget,
            retained_bytes: st.used(),
            retained_entries: st.entries.len(),
            admissions: st.admissions,
            refusals: st.refusals,
            evictions: st.evictions,
        }
    }
}



pub fn budget_from_env_value(raw: Option<&str>) -> Result<u64, LinalgError> {
    let Some(raw) = raw else { return Ok(DEFAULT_RETAINED_FACTORIZATION_BUDGET_BYTES) };
    let text = raw.trim();
    if text.is_empty() {
        return Ok(DEFAULT_RETAINED_FACTORIZATION_BUDGET_BYTES);
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(LinalgError::Invalid(format!(
            "{RETAINED_FACTORIZATION_BUDGET_ENV} must be a non-negative integer byte count, got '{text}'"
        )));
    }
    text.parse::<u64>().map_err(|_| {
        LinalgError::Invalid(format!("{RETAINED_FACTORIZATION_BUDGET_ENV} is out of range, got '{text}'"))
    })
}



pub fn global_ledger() -> Result<&'static RetentionLedger, LinalgError> {
    static LEDGER: OnceLock<Result<RetentionLedger, LinalgError>> = OnceLock::new();
    LEDGER.get_or_init(RetentionLedger::from_env).as_ref().map_err(Clone::clone)
}

