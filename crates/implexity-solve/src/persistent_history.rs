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

use crate::state_store::{DiskStore, SnapshotStore};
use implexity_core::{CaeError, CaeResult};
use crate::checkpointed_history::HistoryObserver;
use crate::time_stepper::StepRecord;
use std::{collections::BTreeSet, path::Path};
use sha2::{Digest, Sha256};

pub struct PersistentHistoryObserver {
    store: DiskStore,
    identity: String,
    interval: usize,
    period: usize,
    initial: Option<usize>,
    latest: Option<usize>,
    period_end: Option<usize>,
    accepted_step: Option<usize>,
    state_len: Option<usize>,
    accepted_digest: Option<[u8;32]>,
}
impl PersistentHistoryObserver {
    pub fn new(root: &Path, identity: String, interval: usize, period: usize, budget_bytes: u64) -> CaeResult<Self> {
        if identity.is_empty() || interval == 0 || period == 0 {
            return Err(CaeError::contract("persistent history identity and intervals are required"));
        }
        Ok(Self { store: DiskStore::new(root, budget_bytes)?, identity, interval, period, initial: None, latest: None, period_end: None, accepted_step: None, state_len: None, accepted_digest: None })
    }
    pub fn directory(&self) -> &Path { self.store.directory() }
    pub fn latest_checkpoint(&self) -> CaeResult<(usize, Vec<f64>)> {
        self.store.get(self.latest.ok_or_else(|| CaeError::contract("persistent history has no initial state"))?)
    }
    pub fn finish(&mut self, step: usize, state: &[f64]) -> CaeResult<()> {
        if self.accepted_step != Some(step) { return Err(CaeError::contract("persistent final state is not the accepted endpoint")); }
        if self.accepted_digest != Some(state_digest(state)) {
            return Err(CaeError::contract("persistent final state differs from the accepted endpoint"));
        }
        if self.latest == Some(step) {
            let (_, saved) = self.latest_checkpoint()?;
            if saved.len() != state.len() || saved.iter().zip(state).any(|(a,b)| a.to_bits() != b.to_bits()) {
                return Err(CaeError::contract("persistent final state differs from its accepted checkpoint"));
            }
            Ok(())
        } else { self.save(step, state, false) }
    }
    fn save(&mut self, step: usize, state: &[f64], period_end: bool) -> CaeResult<()> {
        if state.is_empty() || state.iter().any(|v| !v.is_finite()) || self.state_len.is_some_and(|len| len != state.len()) {
            return Err(CaeError::contract("persistent history state dimension or values are invalid"));
        }
        if self.store.snapshot_steps().iter().any(|&(slot, _)| slot == step) {
            return Err(CaeError::contract("persistent history would overwrite a retained state"));
        }
        self.store.put(step, step, state)?;
        let initial = self.initial.unwrap_or(step);
        let period = if period_end { Some(step) } else { self.period_end };
        let mut slots = BTreeSet::from([initial, step]);
        if let Some(slot) = period { slots.insert(slot); }
        self.store.persist_selected(&self.identity, &slots.into_iter().collect::<Vec<_>>())?;
        self.initial = Some(initial); self.latest = Some(step); self.period_end = period; self.state_len = Some(state.len());
        Ok(())
    }
}
impl HistoryObserver for PersistentHistoryObserver {
    fn initial(&mut self, step: usize, state: &[f64]) -> CaeResult<()> {
        if self.initial.is_some() { return Err(CaeError::contract("persistent history was already initialized")); }
        self.save(step, state, false)?; self.accepted_step = Some(step); self.accepted_digest = Some(state_digest(state)); Ok(())
    }
    fn accepted(&mut self, step: usize, record: &StepRecord) -> CaeResult<()> {
        let previous = self.accepted_step.ok_or_else(|| CaeError::contract("persistent history needs an initial state"))?;
        if previous.checked_add(1) != Some(step) || record.state.len() != self.state_len.unwrap_or(0)
            || record.state.iter().chain(&record.samples).chain(record.ledger.values()).any(|v| !v.is_finite())
            || !record.diagnostics.residual_norm.is_finite() {
            return Err(CaeError::contract("persistent history record order or values are invalid"));
        }
        let elapsed = step - self.initial.unwrap_or(step);
        let end = elapsed % self.period == 0;
        if end || elapsed % self.interval == 0 { self.save(step, &record.state, end)?; }
        self.accepted_step = Some(step); self.accepted_digest = Some(state_digest(&record.state)); Ok(())
    }
}

fn state_digest(state:&[f64])->[u8;32] {
    let mut hash=Sha256::new();
    hash.update((state.len() as u64).to_le_bytes());
    for value in state { hash.update(value.to_bits().to_le_bytes()); }
    hash.finalize().into()
}
