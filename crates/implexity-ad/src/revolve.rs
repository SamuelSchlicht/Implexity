// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use crate::error::AdError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    Ram,
    Disk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Advance {
        from: usize,
        to: usize,
    },
    Snapshot {
        step: usize,
        slot: usize,
        tier: Tier,
    },
    Restore {
        step: usize,
        slot: usize,
    },
    Reverse {
        step: usize,
    },
    Release {
        slot: usize,
    },
}

const SATURATED: u128 = 1 << 100;

fn beta(s: usize, r: isize) -> u128 {
    let Ok(r) = usize::try_from(r) else { return 0 };
    let mut value: u128 = 1;
    for i in 1..=r {
        value = value.saturating_mul((s + i) as u128) / i as u128;
        if value >= SATURATED {
            return SATURATED;
        }
    }
    value
}

fn signed(r: usize) -> isize {
    isize::try_from(r).unwrap_or(isize::MAX)
}

#[must_use]
pub fn repetitions(steps: usize, snapshots: usize) -> usize {
    if steps <= 1 {
        return 0;
    }
    if snapshots == 0 {
        return usize::MAX;
    }
    let n = steps as u128;
    let mut r = 0usize;
    let mut value: u128 = 1;
    while value < n {
        r += 1;
        value = value.saturating_mul((snapshots + r) as u128) / r as u128;
    }
    r
}

fn chain_advances(length: usize, snapshots: usize) -> u128 {
    if length <= 1 {
        return 0;
    }
    if snapshots == 0 {
        return SATURATED;
    }
    let n = length as u128;
    let mut r = 0u128;
    let mut value: u128 = 1;
    while value < n {
        r += 1;
        value = value.saturating_mul(snapshots as u128 + r) / r;
    }
    r * n - value * r / (snapshots as u128 + 1)
}

fn to_usize(value: u128) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[must_use]
pub fn optimal_forward_steps(steps: usize, snapshots: usize) -> usize {
    match steps {
        0 => 0,
        1 => 1,
        _ if snapshots == 0 => usize::MAX,
        _ => to_usize(chain_advances(steps, snapshots) + 1),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinomialSchedule {
    steps: usize,
    actions: Vec<Action>,
    reverse_start: usize,
    forward_steps: usize,
    peak: usize,
}

struct Builder {
    steps: usize,
    actions: Vec<Action>,
    free: Vec<usize>,
    cursor: Option<usize>,
    in_use: usize,
    peak: usize,
    advanced: usize,
    reverse_start: Option<usize>,
}

impl Builder {
    fn new(steps: usize, total_slots: usize, occupied: &[usize]) -> Self {
        let mut free: Vec<usize> = (0..total_slots).filter(|s| !occupied.contains(s)).collect();
        free.reverse();
        Self {
            steps,
            actions: Vec::new(),
            free,
            cursor: None,
            in_use: occupied.len(),
            peak: occupied.len(),
            advanced: 0,
            reverse_start: None,
        }
    }

    fn take_slot(&mut self) -> Result<usize, AdError> {
        let slot = self
            .free
            .pop()
            .ok_or_else(|| AdError::Invalid("checkpoint schedule ran out of snapshot slots".into()))?;
        self.in_use += 1;
        self.peak = self.peak.max(self.in_use);
        Ok(slot)
    }

    fn release(&mut self, slot: usize) {
        self.actions.push(Action::Release { slot });
        self.in_use = self.in_use.saturating_sub(1);
        self.free.push(slot);

        self.free.sort_unstable_by(|a, b| b.cmp(a));
    }

    fn advance(&mut self, from: usize, to: usize) {
        if to > from {
            self.actions.push(Action::Advance { from, to });
            self.advanced += to - from;
            self.cursor = Some(to);
        }
    }

    fn ensure_cursor(&mut self, position: usize, slot: usize) {
        if self.cursor != Some(position) {
            self.actions.push(Action::Restore { step: position, slot });
            self.cursor = Some(position);
        }
    }

    fn reverse_step(&mut self, step: usize) {
        if self.reverse_start.is_none() {

            self.actions.push(Action::Advance { from: self.steps - 1, to: self.steps });
            self.advanced += 1;
            self.reverse_start = Some(self.actions.len());
        }
        self.actions.push(Action::Reverse { step });
        self.cursor = None;
    }

    fn chain(&mut self, a: usize, length: usize, own: usize, slots: usize) -> Result<(), AdError> {
        let mut length = length;
        loop {
            if length == 0 {
                return Ok(());
            }
            if length == 1 {
                self.ensure_cursor(a, own);
                self.reverse_step(a + 1);
                return Ok(());
            }
            let m = split(length, slots)?;
            self.ensure_cursor(a, own);
            self.advance(a, a + m);
            if length - m > 1 {
                let slot = self.take_slot()?;

                self.actions.push(Action::Snapshot { step: a + m, slot, tier: Tier::Ram });
                self.chain(a + m, length - m, slot, slots - 1)?;
                self.release(slot);
            } else {
                self.reverse_step(a + length);
            }
            length = m;
        }
    }
}

fn split(length: usize, slots: usize) -> Result<usize, AdError> {
    if slots == 0 {
        return Err(AdError::Invalid("a chain of more than one step needs a snapshot slot".into()));
    }
    if slots == 1 {
        return Ok(length - 1);
    }
    let r = signed(repetitions(length, slots));
    let l = length as u128;
    let lo = 1u128.max(beta(slots, r - 2)).max(l.saturating_sub(beta(slots - 1, r)));
    let hi = (l - 1).min(beta(slots, r - 1)).min(l.saturating_sub(beta(slots - 1, r - 1)));
    if lo > hi {
        return Err(AdError::Invalid(format!(
            "no optimal split for a chain of {length} steps with {slots} slots"
        )));
    }
    Ok(to_usize(hi))
}

fn tier_of(slot: usize, disk_levels: usize) -> Tier {
    if slot < disk_levels { Tier::Disk } else { Tier::Ram }
}

impl BinomialSchedule {


    pub fn new(steps: usize, ram_snapshots: usize, disk_snapshots: usize) -> Result<Self, AdError> {
        if steps == 0 {
            return Err(AdError::Invalid("a checkpoint schedule needs at least one step".into()));
        }
        let slots = ram_snapshots
            .checked_add(disk_snapshots)
            .ok_or_else(|| AdError::Invalid("snapshot slot count overflows".into()))?;
        if slots == 0 {
            return Err(AdError::Invalid("a checkpoint schedule needs at least one snapshot slot".into()));
        }
        let mut builder = Builder::new(steps, slots, &[]);
        let own = builder.take_slot()?;
        builder.actions.push(Action::Snapshot { step: 0, slot: own, tier: Tier::Ram });
        builder.cursor = Some(0);
        builder.chain(0, steps, own, slots)?;
        let peak = builder.peak;
        let disk_levels = peak.saturating_sub(ram_snapshots);
        let mut actions = builder.actions;
        for action in &mut actions {
            if let Action::Snapshot { slot, tier, .. } = action {
                *tier = tier_of(*slot, disk_levels);
            }
        }
        let schedule = Self {
            steps,
            actions,
            reverse_start: builder.reverse_start.unwrap_or(0),
            forward_steps: builder.advanced,
            peak,
        };
        debug_assert_eq!(schedule.forward_steps, optimal_forward_steps(steps, slots));
        Ok(schedule)
    }

    #[must_use]
    pub const fn steps(&self) -> usize {
        self.steps
    }

    #[must_use]
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    #[must_use]
    pub const fn reverse_start(&self) -> usize {
        self.reverse_start
    }

    #[must_use]
    pub fn forward_actions(&self) -> &[Action] {
        &self.actions[..self.reverse_start]
    }

    #[must_use]
    pub fn reverse_actions(&self) -> &[Action] {
        &self.actions[self.reverse_start..]
    }

    #[must_use]
    pub const fn forward_steps(&self) -> usize {
        self.forward_steps
    }

    #[must_use]
    pub const fn peak_snapshots(&self) -> usize {
        self.peak
    }

    fn from_checkpoints(
        steps: usize,
        slots: usize,
        ram: usize,
        checkpoints: &[(usize, usize)],
    ) -> Result<Self, AdError> {
        let mut sorted: Vec<(usize, usize)> = checkpoints.to_vec();
        sorted.sort_unstable();
        if sorted.first() != Some(&(0, 0)) {
            return Err(AdError::Invalid("the initial state must be snapshot 0 in slot 0".into()));
        }
        let tier = |slot: usize| if slot < ram { Tier::Ram } else { Tier::Disk };
        let occupied: Vec<usize> = sorted.iter().map(|c| c.1).collect();
        let mut builder = Builder::new(steps, slots, &occupied);

        let useful: Vec<(usize, usize)> =
            sorted.iter().copied().filter(|c| c.0 == 0 || c.0 + 1 < steps).collect();
        let dropped: Vec<usize> =
            sorted.iter().filter(|c| c.0 != 0 && c.0 + 1 >= steps).map(|c| c.1).collect();

        let mut position = 0usize;
        for &(at, slot) in &sorted {
            if at >= steps {
                continue;
            }
            builder.advance(position, at);
            position = at;
            builder.actions.push(Action::Snapshot { step: at, slot, tier: tier(slot) });
        }
        builder.advance(position, steps - 1);
        builder.cursor = Some(steps - 1);
        builder.reverse_step(steps);
        for slot in dropped {
            builder.release(slot);
        }

        let mut end = steps - 1;
        for (j, &(at, slot)) in useful.iter().enumerate().rev() {
            builder.chain(at, end - at, slot, slots - j)?;
            if j > 0 {
                builder.release(slot);
            }
            end = at;
        }
        let mut actions = builder.actions;
        for action in &mut actions {
            if let Action::Snapshot { slot, tier: t, .. } = action {
                *t = tier(*slot);
            }
        }
        Ok(Self {
            steps,
            actions,
            reverse_start: builder.reverse_start.unwrap_or(0),
            forward_steps: builder.advanced,
            peak: builder.peak,
        })
    }
}

#[derive(Clone, Debug)]
pub struct OnlineSchedule {
    ram: usize,
    slots: usize,
    checkpoints: Vec<(usize, usize)>,
    free: Vec<usize>,
    next_step: usize,
    fault: Option<String>,
}

impl OnlineSchedule {


    pub fn new(ram_snapshots: usize, disk_snapshots: usize) -> Result<Self, AdError> {
        let slots = ram_snapshots
            .checked_add(disk_snapshots)
            .ok_or_else(|| AdError::Invalid("snapshot slot count overflows".into()))?;
        if slots == 0 {
            return Err(AdError::Invalid(
                "an online checkpoint schedule needs at least one snapshot slot".into(),
            ));
        }
        let mut free: Vec<usize> = (0..slots).collect();
        free.reverse();
        Ok(Self { ram: ram_snapshots, slots, checkpoints: Vec::new(), free, next_step: 0, fault: None })
    }

    fn tier(&self, slot: usize) -> Tier {
        if slot < self.ram { Tier::Ram } else { Tier::Disk }
    }

    pub fn after_step(&mut self, step: usize) -> Option<Action> {
        if self.fault.is_some() {
            return None;
        }
        if step != self.next_step {
            self.fault = Some(format!(
                "online checkpoint schedule expected step {} but was told about step {step}",
                self.next_step
            ));
            return None;
        }
        self.next_step += 1;
        if let Some(slot) = self.free.pop() {
            self.checkpoints.push((step, slot));
            return Some(Action::Snapshot { step, slot, tier: self.tier(slot) });
        }
        if self.checkpoints.len() < 2 {

            return None;
        }
        let replaced = self.best_replacement(step)?;
        let (_, slot) = self.checkpoints.remove(replaced);
        self.checkpoints.push((step, slot));
        Some(Action::Snapshot { step, slot, tier: self.tier(slot) })
    }

    fn best_replacement(&self, step: usize) -> Option<usize> {
        let c: Vec<usize> = self.checkpoints.iter().map(|x| x.0).collect();
        let k = c.len();
        let s = self.slots;
        let near = step + 1;
        let far = near + near / 2;

        let internal: Vec<u128> = (0..k - 1).map(|j| chain_advances(c[j + 1] - c[j], s - j)).collect();

        let shifted: Vec<u128> = (0..k - 1).map(|j| chain_advances(c[j + 1] - c[j], s - j + 1)).collect();
        let mut prefix = vec![0u128; k];
        for j in 0..k - 1 {
            prefix[j + 1] = prefix[j] + internal[j];
        }
        let mut suffix = vec![0u128; k + 1];
        for j in (0..k - 1).rev() {
            suffix[j] = suffix[j + 1] + shifted[j];
        }
        let tail = |start: usize, index: usize, horizon: usize| -> u128 {
            chain_advances((horizon - 1).saturating_sub(start), s - index)
        };
        let keep = (prefix[k - 1] + tail(c[k - 1], k - 1, near), prefix[k - 1] + tail(c[k - 1], k - 1, far));
        let mut best: (u128, u128) = keep;
        let mut choice = None;
        for drop in 1..k {

            let merged_end = if drop + 1 < k { c[drop + 1] } else { step };
            let merged = chain_advances(merged_end - c[drop - 1], s - (drop - 1));
            let after = if drop + 1 < k {

                suffix[drop + 1] + chain_advances(step - c[k - 1], s - (k - 2))
            } else {
                0
            };
            let base = prefix[drop - 1] + merged + after;
            let score = (base + tail(step, k - 1, near), base + tail(step, k - 1, far));
            if score < best {
                best = score;
                choice = Some(drop);
            }
        }
        choice
    }

    #[must_use]
    pub fn snapshots(&self) -> Vec<(usize, usize, Tier)> {
        self.checkpoints.iter().map(|&(p, s)| (p, s, self.tier(s))).collect()
    }



    pub fn reverse_plan(self, steps: usize) -> Result<BinomialSchedule, AdError> {
        if let Some(fault) = self.fault {
            return Err(AdError::Invalid(fault));
        }
        if steps == 0 {
            return Err(AdError::Invalid("a checkpoint schedule needs at least one step".into()));
        }
        if self.next_step < steps {
            return Err(AdError::Invalid(format!(
                "online checkpoint schedule saw {} states but the history has {steps} steps",
                self.next_step
            )));
        }
        BinomialSchedule::from_checkpoints(steps, self.slots, self.ram, &self.checkpoints)
    }
}

