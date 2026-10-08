// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError, RwLock};

use implexity_ad::revolve::{Action, BinomialSchedule, OnlineSchedule, Tier};
use implexity_core::error::{CaeError, CaeResult};
use implexity_linalg::dense::DenseMatrix;
use serde_json::{Value, json};

use crate::state_store::{AdaptiveStore, SnapshotStore, StoreBudget, TieredStore};
use crate::time_stepper::{
    StepDiagnostics, StepParameters, StepRecord, TimeStepper, check_parameters, check_vector,
};

pub const RECOMPUTATION_DIVERGED: &str = "checkpoint recomputation diverged from the forward sweep";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointPolicy {
    All,
    Binomial {
        ram_snapshots: usize,
        disk_snapshots: usize,
    },
    Online {
        ram_snapshots: usize,
        disk_snapshots: usize,
    },
}

impl CheckpointPolicy {
    #[must_use]
    pub const fn slots(&self) -> Option<(usize, usize)> {
        match *self {
            Self::All => None,
            Self::Binomial { ram_snapshots, disk_snapshots }
            | Self::Online { ram_snapshots, disk_snapshots } => Some((ram_snapshots, disk_snapshots)),
        }
    }

    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Binomial { .. } => "binomial",
            Self::Online { .. } => "online",
        }
    }

    fn validate(&self) -> CaeResult<()> {
        if let Some((ram, disk)) = self.slots()
            && ram.checked_add(disk).is_none_or(|s| s == 0)
        {
            return Err(CaeError::contract(format!(
                "{} checkpointing needs at least one snapshot slot",
                self.name()
            )));
        }
        Ok(())
    }
}

pub trait StopRule: Send + Sync {


    fn stop(&self, n: usize, record: &StepRecord) -> CaeResult<bool>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct HistoryGradient {
    pub design: Vec<f64>,
    pub initial_state: Vec<f64>,
    pub time_scale: f64,
}

#[must_use]
pub fn state_digest(state: &[f64]) -> [u64; 2] {
    const K0: u64 = 0x9E37_79B9_7F4A_7C15;
    const K1: u64 = 0xC2B2_AE3D_27D4_EB4F;
    let mut a: u64 = 0x243F_6A88_85A3_08D3 ^ state.len() as u64;
    let mut b: u64 = 0x1319_8A2E_0370_7344 ^ (state.len() as u64).rotate_left(32);
    for v in state {
        let w = v.to_bits();
        a = (a ^ w).wrapping_mul(K0).rotate_left(29);
        b = (b ^ w.rotate_left(17)).wrapping_mul(K1).rotate_left(31);
    }
    let fin = |mut h: u64| {
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 33;
        h
    };
    [fin(a ^ b.rotate_left(7)), fin(b ^ a.rotate_left(13))]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Location {
    Initial,
    Store(usize),
}

#[derive(Debug)]
struct SlotMap {
    map: BTreeMap<usize, (Location, usize)>,
    free_ram: Vec<usize>,
    free_disk: Vec<usize>,
}

impl SlotMap {
    fn new(ram: usize, disk: usize) -> Self {
        Self {
            map: BTreeMap::new(),
            free_ram: (0..ram).rev().collect(),
            free_disk: (ram..ram + disk).rev().collect(),
        }
    }

    fn free(&mut self, location: Location, store: &mut TieredStore) -> CaeResult<()> {
        if let Location::Store(s) = location {
            store.release(s)?;
            if s < store.ram_slots() {
                self.free_ram.push(s);
            } else {
                self.free_disk.push(s);
            }
        }
        Ok(())
    }

    fn snapshot(
        &mut self,
        slot: usize,
        tier: Tier,
        step: usize,
        state: &[f64],
        store: &mut TieredStore,
    ) -> CaeResult<()> {
        let previous = self.map.remove(&slot);
        if step == 0 {
            if let Some((location, _)) = previous {
                self.free(location, store)?;
            }
            self.map.insert(slot, (Location::Initial, 0));
            return Ok(());
        }
        let reuse = match previous {
            Some((Location::Store(s), _)) if (s < store.ram_slots()) == (tier == Tier::Ram) => Some(s),
            Some((location, _)) => {
                self.free(location, store)?;
                None
            }
            None => None,
        };
        let target = if let Some(s) = reuse {
            s
        } else {
            let pool = if tier == Tier::Ram { &mut self.free_ram } else { &mut self.free_disk };
            pool.pop().ok_or_else(|| {
                CaeError::contract(format!(
                    "checkpoint schedule needs more {} snapshot slots than configured",
                    if tier == Tier::Ram { "RAM" } else { "disk" }
                ))
            })?
        };
        store.put(target, step, state)?;
        self.map.insert(slot, (Location::Store(target), step));
        Ok(())
    }

    fn restore(&self, slot: usize, step: usize, initial: &[f64], store: &TieredStore) -> CaeResult<Vec<f64>> {
        match self.map.get(&slot) {
            Some((Location::Initial, 0)) if step == 0 => Ok(initial.to_vec()),
            Some((Location::Store(s), held)) if *held == step => {
                let (stored_step, state) = store.get(*s)?;
                if stored_step != step {
                    return Err(inconsistent("a snapshot slot holds another step"));
                }
                Ok(state)
            }
            _ => Err(inconsistent("restore of an empty or foreign slot")),
        }
    }

    fn release(&mut self, slot: usize, store: &mut TieredStore) -> CaeResult<()> {
        if let Some((location, _)) = self.map.remove(&slot) {
            self.free(location, store)?;
        }
        Ok(())
    }

    fn clear(&mut self, store: &mut TieredStore) -> CaeResult<()> {
        let slots: Vec<usize> = self.map.keys().copied().collect();
        for slot in slots {
            self.release(slot, store)?;
        }
        Ok(())
    }
}

fn inconsistent(what: &str) -> CaeError {
    CaeError::contract(format!("checkpoint schedule is inconsistent: {what}"))
}

struct Executor<'a> {
    steps: usize,
    initial: &'a [f64],
    last: Option<&'a [f64]>,
    store: &'a mut TieredStore,
    slots: &'a mut SlotMap,
    cursor: Option<(usize, Vec<f64>)>,
    ahead: Option<(usize, Vec<f64>)>,
    evaluations: usize,
}

pub type AdvanceFn<'f> = dyn FnMut(usize, &[f64]) -> CaeResult<Vec<f64>> + 'f;
pub type ReverseFn<'f> = dyn FnMut(usize, &[f64], &[f64]) -> CaeResult<()> + 'f;

impl Executor<'_> {
    fn run(
        &mut self,
        actions: &[Action],
        advance: &mut AdvanceFn<'_>,
        reverse: &mut ReverseFn<'_>,
    ) -> CaeResult<()> {
        for action in actions {
            match *action {
                Action::Snapshot { step, slot, tier } => {
                    let Some((at, state)) = &self.cursor else {
                        return Err(inconsistent("snapshot without a cursor"));
                    };
                    if *at != step {
                        return Err(inconsistent("snapshot of a state the cursor does not hold"));
                    }
                    self.slots.snapshot(slot, tier, step, state, self.store)?;
                }
                Action::Restore { step, slot } => {
                    let state = self.slots.restore(slot, step, self.initial, self.store)?;
                    self.cursor = Some((step, state));
                }
                Action::Release { slot } => self.slots.release(slot, self.store)?,
                Action::Advance { from, to } => {
                    let Some((at, mut state)) = self.cursor.take() else {
                        return Err(inconsistent("advance without a cursor"));
                    };
                    if at != from || to <= from || to > self.steps {
                        return Err(inconsistent("advance from a position the cursor does not hold"));
                    }
                    let mut position = from;
                    for k in from + 1..=to {
                        if k == self.steps {

                            let last = if let Some(z) = self.last {
                                z.to_vec()
                            } else {
                                self.evaluations += 1;
                                advance(k, &state)?
                            };
                            self.ahead = Some((k, last));
                            break;
                        }
                        self.evaluations += 1;
                        state = advance(k, &state)?;
                        position = k;
                    }
                    self.cursor = Some((position, state));
                }
                Action::Reverse { step } => {
                    let (Some((pc, previous)), Some((pa, current))) = (self.cursor.take(), self.ahead.take())
                    else {
                        return Err(inconsistent("reverse without both states"));
                    };
                    if pc + 1 != step || pa != step {
                        return Err(inconsistent("reverse of a step whose states are not held"));
                    }
                    reverse(step, &previous, &current)?;
                    self.ahead = Some((pc, previous));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Storage {
    All(AdaptiveStore),
    Checkpointed {
        store: TieredStore,
        slots: SlotMap,
        ram: usize,
        disk: usize,
        first: Option<(BinomialSchedule, Vec<f64>)>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryRunStats {
    pub adjoint_sweeps: usize,
    pub tangent_sweeps: usize,
    pub recomputed_steps: usize,
    pub peak_snapshot_bytes: (u64, u64),
    pub disk_writes: usize,
}

pub struct HistoryRun {
    identity: String,
    first_step: usize,
    steps: usize,
    design: Vec<f64>,
    time_scale: f64,
    initial_state: Vec<f64>,
    final_state: Vec<f64>,
    samples: DenseMatrix,
    ledger: Vec<BTreeMap<String, f64>>,
    diagnostics: Vec<StepDiagnostics>,
    digests: Vec<[u64; 2]>,
    policy: CheckpointPolicy,
    storage: RwLock<Storage>,
    stats: Mutex<HistoryRunStats>,
}

impl std::fmt::Debug for HistoryRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryRun")
            .field("stepper", &self.identity)
            .field("first_step", &self.first_step)
            .field("steps", &self.steps)
            .field("time_scale", &self.time_scale)
            .field("policy", &self.policy)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl HistoryRun {
    #[must_use]
    pub fn stepper_identity(&self) -> &str {
        &self.identity
    }
    #[must_use]
    pub const fn first_step(&self) -> usize {
        self.first_step
    }
    #[must_use]
    pub const fn steps(&self) -> usize {
        self.steps
    }
    #[must_use]
    pub fn design(&self) -> &[f64] {
        &self.design
    }
    #[must_use]
    pub const fn time_scale(&self) -> f64 {
        self.time_scale
    }
    #[must_use]
    pub fn initial_state(&self) -> &[f64] {
        &self.initial_state
    }
    #[must_use]
    pub fn final_state(&self) -> &[f64] {
        &self.final_state
    }
    #[must_use]
    pub const fn samples(&self) -> &DenseMatrix {
        &self.samples
    }
    #[must_use]
    pub fn ledger(&self) -> &[BTreeMap<String, f64>] {
        &self.ledger
    }
    #[must_use]
    pub fn diagnostics(&self) -> &[StepDiagnostics] {
        &self.diagnostics
    }
    #[must_use]
    pub const fn policy(&self) -> &CheckpointPolicy {
        &self.policy
    }
    #[must_use]
    pub fn stats(&self) -> HistoryRunStats {
        let mut stats = *self.stats.lock().unwrap_or_else(PoisonError::into_inner);
        if let Storage::Checkpointed { store, .. } =
            &*self.storage.read().unwrap_or_else(PoisonError::into_inner)
        {
            stats.peak_snapshot_bytes = store.peak_bytes();
            stats.disk_writes = store.disk_writes();
        }
        stats
    }

    #[must_use]
    pub fn record(&self) -> Value {
        let stats = self.stats();
        let (ram, disk) = self.policy.slots().unwrap_or((0, 0));
        json!({
            "schema": "implexity-history-run/1",
            "stepper": self.identity,
            "first_step": self.first_step,
            "steps": self.steps,
            "time_scale": self.time_scale,
            "checkpoint_policy": self.policy.name(),
            "ram_snapshots": ram,
            "disk_snapshots": disk,
            "newton_iterations": self.diagnostics.iter().map(|d| d.newton_iterations).sum::<usize>(),
            "coupling_iterations": self.diagnostics.iter().map(|d| d.coupling_iterations).sum::<usize>(),
            "krylov_iterations": self.diagnostics.iter().map(|d| d.krylov_iterations).sum::<usize>(),
            "maximum_step_residual": self.diagnostics.iter().map(|d| d.residual_norm).fold(0.0, f64::max),
            "adjoint_sweeps": stats.adjoint_sweeps,
            "tangent_sweeps": stats.tangent_sweeps,
            "recomputed_steps": stats.recomputed_steps,
            "peak_ram_snapshot_bytes": stats.peak_snapshot_bytes.0,
            "peak_disk_snapshot_bytes": stats.peak_snapshot_bytes.1,
            "disk_writes": stats.disk_writes,
            "derivative_scope": "discrete_history_exact",
        })
    }

    fn step_number(&self, k: usize) -> usize {
        self.first_step + k - 1
    }

    fn check_parameters(&self, stepper: &dyn TimeStepper, p: StepParameters<'_>) -> CaeResult<()> {
        if stepper.identity() != self.identity
            || stepper.state_size() != self.initial_state.len()
            || stepper.sample_names().len() != self.samples.ncols
        {
            return Err(CaeError::contract(format!(
                "history run of stepper {} cannot be swept with stepper {}",
                self.identity,
                stepper.identity()
            )));
        }
        let same = p.design.len() == self.design.len()
            && p.design.iter().zip(&self.design).all(|(a, b)| a.to_bits() == b.to_bits())
            && p.time_scale.to_bits() == self.time_scale.to_bits();
        if !same {
            return Err(CaeError::contract(
                "history sweeps must use exactly the design and time scale of the forward run",
            ));
        }
        Ok(())
    }

    fn recompute(
        &self,
        stepper: &dyn TimeStepper,
        p: StepParameters<'_>,
        k: usize,
        previous: &[f64],
    ) -> CaeResult<Vec<f64>> {
        let record = stepper.advance(self.step_number(k), previous, p)?;
        let n_s = self.samples.ncols;
        let row = &self.samples.data[(k - 1) * n_s..k * n_s];
        let same_samples = record.samples.len() == n_s
            && record.samples.iter().zip(row).all(|(a, b)| a.to_bits() == b.to_bits());
        if !same_samples || state_digest(&record.state) != self.digests[k] {
            return Err(CaeError::convergence(RECOMPUTATION_DIVERGED));
        }
        Ok(record.state)
    }

    fn count(&self, f: impl FnOnce(&mut HistoryRunStats)) {
        f(&mut self.stats.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

fn check_record(stepper: &dyn TimeStepper, n: usize, record: &StepRecord) -> CaeResult<()> {
    if record.state.len() != stepper.state_size() || record.state.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!(
            "time stepper {} step {n} produced an invalid state (length {} or non-finite values)",
            stepper.identity(),
            record.state.len()
        )));
    }
    if record.samples.len() != stepper.sample_names().len() || record.samples.iter().any(|v| !v.is_finite()) {
        return Err(CaeError::convergence(format!(
            "time stepper {} step {n} produced invalid samples (length {} or non-finite values)",
            stepper.identity(),
            record.samples.len()
        )));
    }
    Ok(())
}

enum Plan {
    All(AdaptiveStore),
    Binomial { schedule: BinomialSchedule, snapshots: BTreeMap<usize, (usize, Tier)> },
    Online(OnlineSchedule),
}



#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn run_history(
    stepper: &dyn TimeStepper,
    p: StepParameters<'_>,
    initial: &[f64],
    first_step: usize,
    steps: Option<usize>,
    stop: Option<&dyn StopRule>,
    policy: CheckpointPolicy,
    budget: &StoreBudget,
) -> CaeResult<HistoryRun> {
    check_parameters(stepper, p)?;
    check_vector(initial, stepper.state_size(), "history initial state")?;
    policy.validate()?;
    if first_step == 0 {
        return Err(CaeError::contract("history steps are numbered from 1"));
    }
    if steps == Some(0) {
        return Err(CaeError::contract("a history needs at least one step"));
    }
    if steps.is_none() && stop.is_none() {
        return Err(CaeError::contract("a history needs a step count or a stop rule"));
    }
    let nz = stepper.state_size();
    let n_s = stepper.sample_names().len();
    let (mut plan, store) = match &policy {
        CheckpointPolicy::All => (Plan::All(AdaptiveStore::new(budget)), None),
        CheckpointPolicy::Binomial { ram_snapshots, disk_snapshots } => {
            let (Some(n), None) = (steps, stop) else {
                return Err(CaeError::contract(
                    "binomial checkpointing needs a known step count and no stop rule; use online checkpointing with a stop rule",
                ));
            };
            let schedule = BinomialSchedule::new(n, *ram_snapshots, *disk_snapshots)
                .map_err(|e| CaeError::contract(e.to_string()))?;
            let snapshots = schedule
                .forward_actions()
                .iter()
                .filter_map(|a| match *a {
                    Action::Snapshot { step, slot, tier } => Some((step, (slot, tier))),
                    _ => None,
                })
                .collect();
            let store = TieredStore::new(budget, *ram_snapshots, *disk_snapshots, nz)?;
            (Plan::Binomial { schedule, snapshots }, Some(store))
        }
        CheckpointPolicy::Online { ram_snapshots, disk_snapshots } => {
            let online = OnlineSchedule::new(*ram_snapshots, *disk_snapshots)
                .map_err(|e| CaeError::contract(e.to_string()))?;
            let store = TieredStore::new(budget, *ram_snapshots, *disk_snapshots, nz)?;
            (Plan::Online(online), Some(store))
        }
    };
    let (ram, disk) = policy.slots().unwrap_or((0, 0));
    let mut slots = SlotMap::new(ram, disk);
    let mut store = store;
    let mut take = |step: usize, state: &[f64], plan: &mut Plan| -> CaeResult<()> {
        let decision = match plan {
            Plan::All(_) => None,
            Plan::Binomial { snapshots, .. } => snapshots.get(&step).copied(),
            Plan::Online(online) => match online.after_step(step) {
                Some(Action::Snapshot { slot, tier, .. }) => Some((slot, tier)),
                _ => None,
            },
        };
        if let (Some((slot, tier)), Some(store)) = (decision, store.as_mut()) {
            slots.snapshot(slot, tier, step, state, store)?;
        }
        Ok(())
    };
    take(0, initial, &mut plan)?;
    let limit = steps.unwrap_or(usize::MAX);
    let mut cursor = initial.to_vec();
    let mut previous: Vec<f64>;
    let mut samples = Vec::new();
    let mut ledger = Vec::new();
    let mut diagnostics = Vec::new();
    let mut digests = vec![state_digest(initial)];
    let mut k = 0usize;
    loop {
        k += 1;
        let n = first_step + k - 1;
        let record = stepper.advance(n, &cursor, p)?;
        check_record(stepper, n, &record)?;
        let done = k == limit
            || match stop {
                Some(rule) => rule.stop(n, &record)?,
                None => false,
            };
        let StepRecord { state, samples: s, ledger: l, diagnostics: d } = record;
        digests.push(state_digest(&state));
        samples.extend_from_slice(&s);
        ledger.push(l);
        diagnostics.push(d);
        take(k, &state, &mut plan)?;
        if let Plan::All(store) = &mut plan && !done {
            store.put(k - 1, k, &state)?;
        }
        previous = std::mem::replace(&mut cursor, state);
        if done {
            break;
        }
    }
    let steps = k;
    let samples = DenseMatrix::new(steps, n_s, samples).map_err(|e| CaeError::contract(e.to_string()))?;
    let storage = match plan {
        Plan::All(store) => Storage::All(store),
        Plan::Binomial { schedule, .. } => Storage::Checkpointed {
            store: store.ok_or_else(|| inconsistent("binomial run without a store"))?,
            slots,
            ram,
            disk,
            first: Some((schedule, previous)),
        },
        Plan::Online(online) => {
            let schedule = online.reverse_plan(steps).map_err(|e| CaeError::contract(e.to_string()))?;
            Storage::Checkpointed {
                store: store.ok_or_else(|| inconsistent("online run without a store"))?,
                slots,
                ram,
                disk,
                first: Some((schedule, previous)),
            }
        }
    };
    let mut initial_stats = HistoryRunStats::default();
    if let Storage::All(store) = &storage { initial_stats.peak_snapshot_bytes = store.peak_bytes(); initial_stats.disk_writes = store.disk_writes(); }
    Ok(HistoryRun {
        identity: stepper.identity().to_string(),
        first_step,
        steps,
        design: p.design.to_vec(),
        time_scale: p.time_scale,
        initial_state: initial.to_vec(),
        final_state: cursor,
        samples,
        ledger,
        diagnostics,
        digests,
        policy,
        storage: RwLock::new(storage),
        stats: Mutex::new(initial_stats),
    })
}



pub fn history_tangent(
    stepper: &dyn TimeStepper,
    p: StepParameters<'_>,
    run: &HistoryRun,
    d_initial: &[f64],
    d_design: Option<&[f64]>,
    d_time_scale: f64,
) -> CaeResult<(Vec<f64>, DenseMatrix)> {
    run.check_parameters(stepper, p)?;
    check_vector(d_initial, stepper.state_size(), "history initial-state direction")?;
    if let Some(d) = d_design {
        check_vector(d, stepper.design_size(), "history design direction")?;
    }
    if !d_time_scale.is_finite() {
        return Err(CaeError::contract("history time-scale direction must be finite"));
    }
    let n = run.steps;
    let n_s = run.samples.ncols;
    let mut d_samples = DenseMatrix::zeros(n, n_s);
    let mut dz = d_initial.to_vec();
    let guard = run.storage.read().unwrap_or_else(PoisonError::into_inner);
    let stored = match &*guard {
        Storage::All(states) => Some(states),
        Storage::Checkpointed { .. } => None,
    };
    let mut previous = run.initial_state.clone();
    let mut recomputed = 0usize;
    for k in 1..=n {
        let current = if k == n {
            run.final_state.clone()
        } else if let Some(states) = &stored {
            states.get(k - 1)?.1
        } else {
            recomputed += 1;
            run.recompute(stepper, p, k, &previous)?
        };
        let t = stepper.tangent(run.step_number(k), &previous, &current, p, &dz, d_design, d_time_scale)?;
        check_vector(&t.state, stepper.state_size(), "step state tangent")?;
        check_vector(&t.samples, n_s, "step sample tangent")?;
        d_samples.data[(k - 1) * n_s..k * n_s].copy_from_slice(&t.samples);
        dz = t.state;
        previous = current;
    }
    drop(guard);
    run.count(|s| {
        s.tangent_sweeps += 1;
        s.recomputed_steps += recomputed;
    });
    Ok((dz, d_samples))
}



#[allow(clippy::too_many_lines)]
pub fn history_adjoint_many(
    stepper: &dyn TimeStepper,
    p: StepParameters<'_>,
    run: &HistoryRun,
    sample_bars: &[DenseMatrix],
    final_state_bars: &[Vec<f64>],
) -> CaeResult<Vec<HistoryGradient>> {
    run.check_parameters(stepper, p)?;
    if sample_bars.len() != final_state_bars.len() {
        return Err(CaeError::contract(format!(
            "history adjoint received {} sample and {} final-state cotangents",
            sample_bars.len(),
            final_state_bars.len()
        )));
    }
    let m = sample_bars.len();
    if m == 0 {
        return Ok(Vec::new());
    }
    let n = run.steps;
    let n_s = run.samples.ncols;
    let nz = stepper.state_size();
    let nx = stepper.design_size();
    for (bar, fin) in sample_bars.iter().zip(final_state_bars) {
        if bar.nrows != n
            || bar.ncols != n_s
            || bar.data.len() != n * n_s
            || bar.data.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(format!(
                "history sample cotangent must be a finite {n} × {n_s} matrix (got {} × {})",
                bar.nrows, bar.ncols
            )));
        }
        check_vector(fin, nz, "history final-state cotangent")?;
    }
    let mut lambdas: Vec<Vec<f64>> = final_state_bars.to_vec();
    let mut design = vec![vec![0.0; nx]; m];
    let mut time_scale = vec![0.0; m];
    let mut rows = vec![Vec::with_capacity(n_s); m];
    let mut reverse = |k: usize, previous: &[f64], current: &[f64]| -> CaeResult<()> {
        for (row, bar) in rows.iter_mut().zip(sample_bars) {
            row.clear();
            row.extend_from_slice(&bar.data[(k - 1) * n_s..k * n_s]);
        }
        let cotangents = stepper.adjoint_many(run.step_number(k), previous, current, p, &lambdas, &rows)?;
        if cotangents.len() != m {
            return Err(CaeError::contract(
                "time stepper adjoint_many returned a different number of cotangents",
            ));
        }
        for (r, c) in cotangents.into_iter().enumerate() {
            check_vector(&c.previous, nz, "step previous-state cotangent")?;
            check_vector(&c.design, nx, "step design cotangent")?;
            if !c.time_scale.is_finite() {
                return Err(CaeError::contract("step time-scale cotangent is not finite"));
            }
            for (a, b) in design[r].iter_mut().zip(&c.design) {
                *a += b;
            }
            time_scale[r] += c.time_scale;
            lambdas[r] = c.previous;
        }
        Ok(())
    };
    let mut guard = run.storage.write().unwrap_or_else(PoisonError::into_inner);
    let recomputed = match &mut *guard {
        Storage::All(states) => {
            let mut current = run.final_state.clone();
            for k in (1..=n).rev() {
                let previous = if k == 1 { run.initial_state.clone() } else { states.get(k - 2)?.1 };
                reverse(k, &previous, &current)?;
                current = previous;
            }
            0
        }
        Storage::Checkpointed { store, slots, ram, disk, first } => {
            let mut advance = |k: usize, previous: &[f64]| run.recompute(stepper, p, k, previous);
            let (schedule, skip_forward, penultimate) = if let Some((schedule, penultimate)) = first.take() {
                (schedule, true, Some(penultimate))
            } else {

                slots.clear(store)?;
                let schedule =
                    BinomialSchedule::new(n, *ram, *disk).map_err(|e| CaeError::contract(e.to_string()))?;
                (schedule, false, None)
            };
            let mut executor = Executor {
                steps: n,
                initial: &run.initial_state,
                last: Some(&run.final_state),
                store: &mut *store,
                slots: &mut *slots,
                cursor: None,
                ahead: None,
                evaluations: 0,
            };
            let actions = if skip_forward {
                executor.cursor = Some((n - 1, penultimate.unwrap_or_default()));
                executor.ahead = Some((n, run.final_state.clone()));
                schedule.reverse_actions()
            } else {
                executor.cursor = Some((0, run.initial_state.clone()));
                schedule.actions()
            };
            let outcome = executor.run(actions, &mut advance, &mut reverse);
            let evaluations = executor.evaluations;
            if outcome.is_err() {

                let _ = slots.clear(store);
            }
            outcome?;
            evaluations
        }
    };
    drop(guard);
    run.count(|s| {
        s.adjoint_sweeps += 1;
        s.recomputed_steps += recomputed;
    });
    Ok(lambdas
        .into_iter()
        .zip(design)
        .zip(time_scale)
        .map(|((initial_state, design), time_scale)| HistoryGradient { design, initial_state, time_scale })
        .collect())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepStats {
    pub forward_steps: usize,
    pub peak_snapshot_bytes: (u64, u64),
}



pub fn checkpointed_reverse(
    initial: &[f64],
    steps: usize,
    policy: &CheckpointPolicy,
    budget: &StoreBudget,
    advance: &mut AdvanceFn<'_>,
    reverse: &mut ReverseFn<'_>,
) -> CaeResult<(Vec<f64>, SweepStats)> {
    policy.validate()?;
    if steps == 0 {
        return Err(CaeError::contract("a checkpointed sweep needs at least one step"));
    }
    let nz = initial.len();
    match policy.slots() {
        None => {
            let mut states = AdaptiveStore::new(budget);
            let mut previous = initial.to_vec();
            for k in 1..=steps {
                let next = advance(k, &previous)?;
                if next.len() != nz { return Err(CaeError::contract(format!("inner step {k} changed the state length"))); }
                if k < steps { states.put(k - 1, k, &next)?; }
                previous = next;
            }
            let last = previous;
            for k in (1..=steps).rev() {
                let previous = if k == 1 { initial.to_vec() } else { states.get(k - 2)?.1 };
                let current = if k == steps { last.clone() } else { states.get(k - 1)?.1 };
                reverse(k, &previous, &current)?;
            }
            Ok((last, SweepStats { forward_steps: steps, peak_snapshot_bytes: states.peak_bytes() }))
        }
        Some((ram, disk)) => {
            let schedule =
                BinomialSchedule::new(steps, ram, disk).map_err(|e| CaeError::contract(e.to_string()))?;
            let mut store = TieredStore::new(budget, ram, disk, nz)?;
            let mut slots = SlotMap::new(ram, disk);
            let mut last = None;
            let mut checked_advance = |k: usize, previous: &[f64]| -> CaeResult<Vec<f64>> {
                let next = advance(k, previous)?;
                if next.len() != nz {
                    return Err(CaeError::contract(format!("inner step {k} changed the state length")));
                }
                if k == steps {
                    last = Some(next.clone());
                }
                Ok(next)
            };
            let mut executor = Executor {
                steps,
                initial,
                last: None,
                store: &mut store,
                slots: &mut slots,
                cursor: Some((0, initial.to_vec())),
                ahead: None,
                evaluations: 0,
            };
            executor.run(schedule.actions(), &mut checked_advance, reverse)?;
            let forward_steps = executor.evaluations;
            let last = last.ok_or_else(|| inconsistent("the forward phase did not reach the final step"))?;
            Ok((last, SweepStats { forward_steps, peak_snapshot_bytes: store.peak_bytes() }))
        }
    }
}

pub struct ControlHistoryGradient{pub design:Vec<f64>}
pub trait InitialControlPullback{fn design_pullback(&self,stepper:&dyn TimeStepper,p:StepParameters<'_>,previous:&[f64],current:&[f64],state_bars:&[Vec<f64>],sample_bars:&[Vec<f64>])->CaeResult<Vec<Vec<f64>>>;}
pub fn history_control_pullback_many(
    stepper: &dyn TimeStepper,
    p: StepParameters<'_>,
    run: &HistoryRun,
    sample_bars: &[DenseMatrix],
    final_state_bars: &[Vec<f64>],
    initial_control_pullback: &dyn InitialControlPullback,
) -> CaeResult<Vec<ControlHistoryGradient>> {
    run.check_parameters(stepper, p)?;
    if run.first_step()!=1 || run.steps()==0 { return Err(CaeError::contract("restricted control pullback requires complete from-rest history")); }
    if sample_bars.len() != final_state_bars.len() {
        return Err(CaeError::contract(format!(
            "history adjoint received {} sample and {} final-state cotangents",
            sample_bars.len(),
            final_state_bars.len()
        )));
    }
    let m = sample_bars.len();
    if m == 0 {
        return Ok(Vec::new());
    }
    let n = run.steps;
    let n_s = run.samples.ncols;
    let nz = stepper.state_size();
    let nx = stepper.design_size();
    for (bar, fin) in sample_bars.iter().zip(final_state_bars) {
        if bar.nrows != n
            || bar.ncols != n_s
            || bar.data.len() != n * n_s
            || bar.data.iter().any(|v| !v.is_finite())
        {
            return Err(CaeError::contract(format!(
                "history sample cotangent must be a finite {n} × {n_s} matrix (got {} × {})",
                bar.nrows, bar.ncols
            )));
        }
        check_vector(fin, nz, "history final-state cotangent")?;
    }
    let mut lambdas: Vec<Vec<f64>> = final_state_bars.to_vec();
    let mut design = vec![vec![0.0; nx]; m];
    let mut rows = vec![Vec::with_capacity(n_s); m];
    let mut reverse = |k: usize, previous: &[f64], current: &[f64]| -> CaeResult<()> {
        for (row, bar) in rows.iter_mut().zip(sample_bars) {
            row.clear();
            row.extend_from_slice(&bar.data[(k - 1) * n_s..k * n_s]);
        }
        if k==1 {
            let prefix=initial_control_pullback.design_pullback(stepper,p,previous,current,&lambdas,&rows)?;
            if prefix.len()!=m{return Err(CaeError::contract("restricted prefix response count"));}
            for(r,g)in prefix.into_iter().enumerate(){check_vector(&g,nx,"restricted prefix control gradient")?;for(a,b)in design[r].iter_mut().zip(g){*a+=b;}lambdas[r].fill(0.);}
            return Ok(());
        }
        let cotangents = stepper.adjoint_many(run.step_number(k), previous, current, p, &lambdas, &rows)?;
        if cotangents.len() != m {
            return Err(CaeError::contract(
                "time stepper adjoint_many returned a different number of cotangents",
            ));
        }
        for (r, c) in cotangents.into_iter().enumerate() {
            check_vector(&c.previous, nz, "step previous-state cotangent")?;
            check_vector(&c.design, nx, "step design cotangent")?;
            if !c.time_scale.is_finite() {
                return Err(CaeError::contract("step time-scale cotangent is not finite"));
            }
            for (a, b) in design[r].iter_mut().zip(&c.design) {
                *a += b;
            }
            lambdas[r] = c.previous;
        }
        Ok(())
    };
    let mut guard = run.storage.write().unwrap_or_else(PoisonError::into_inner);
    let recomputed = match &mut *guard {
        Storage::All(states) => {
            let mut current = run.final_state.clone();
            for k in (1..=n).rev() {
                let previous = if k == 1 { run.initial_state.clone() } else { states.get(k - 2)?.1 };
                reverse(k, &previous, &current)?;
                current = previous;
            }
            0
        }
        Storage::Checkpointed { store, slots, ram, disk, first } => {
            let mut advance = |k: usize, previous: &[f64]| run.recompute(stepper, p, k, previous);
            let (schedule, skip_forward, penultimate) = if let Some((schedule, penultimate)) = first.take() {
                (schedule, true, Some(penultimate))
            } else {
                slots.clear(store)?;
                let schedule =
                    BinomialSchedule::new(n, *ram, *disk).map_err(|e| CaeError::contract(e.to_string()))?;
                (schedule, false, None)
            };
            let mut executor = Executor {
                steps: n,
                initial: &run.initial_state,
                last: Some(&run.final_state),
                store: &mut *store,
                slots: &mut *slots,
                cursor: None,
                ahead: None,
                evaluations: 0,
            };
            let actions = if skip_forward {
                executor.cursor = Some((n - 1, penultimate.unwrap_or_default()));
                executor.ahead = Some((n, run.final_state.clone()));
                schedule.reverse_actions()
            } else {
                executor.cursor = Some((0, run.initial_state.clone()));
                schedule.actions()
            };
            let outcome = executor.run(actions, &mut advance, &mut reverse);
            let evaluations = executor.evaluations;
            if outcome.is_err() {
                let _ = slots.clear(store);
            }
            outcome?;
            evaluations
        }
    };
    drop(guard);
    run.count(|s| {
        s.adjoint_sweeps += 1;
        s.recomputed_steps += recomputed;
    });
    Ok(lambdas
        .into_iter()
        .zip(design)
        .map(|(_initial_state, design)| ControlHistoryGradient { design })
        .collect())
}
