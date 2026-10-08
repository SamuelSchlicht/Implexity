// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{HashMap, VecDeque};

use sha2::{Digest, Sha256};

use crate::time_stepper::StepParameters;

#[derive(Clone, Debug)]
pub(super) struct CacheEntry {
    pub(super) field_b: Vec<f64>,
    pub(super) trace_end: Vec<f64>,
    pub(super) flux: Vec<f64>,
    pub(super) reference: f64,
}

impl CacheEntry {
    fn bytes(&self) -> u64 {
        ((self.field_b.len() + self.trace_end.len() + self.flux.len()) as u64 + 1) * 8 + 96
    }
}

pub(super) struct StepCache {
    capacity: u64,
    used: u64,
    entries: HashMap<[u8; 32], CacheEntry>,
    order: VecDeque<[u8; 32]>,
}

impl StepCache {
    pub(super) fn new(capacity: u64) -> Self {
        Self { capacity, used: 0, entries: HashMap::new(), order: VecDeque::new() }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used = 0;
    }

    pub(super) fn get(&self, key: &[u8; 32]) -> Option<CacheEntry> {
        self.entries.get(key).cloned()
    }

    pub(super) fn insert(&mut self, key: [u8; 32], entry: CacheEntry) {
        let bytes = entry.bytes();
        if bytes > self.capacity {
            return;
        }
        if let Some(old) = self.entries.remove(&key) {
            self.used -= old.bytes();
            self.order.retain(|k| k != &key);
        }
        while self.used + bytes > self.capacity {
            let Some(oldest) = self.order.pop_front() else { break };
            if let Some(old) = self.entries.remove(&oldest) {
                self.used -= old.bytes();
            }
        }
        self.used += bytes;
        self.entries.insert(key, entry);
        self.order.push_back(key);
    }
}

fn hash_values(hasher: &mut Sha256, values: &[f64]) {
    hasher.update((values.len() as u64).to_le_bytes());
    let mut buffer = Vec::with_capacity(8 * values.len().min(4096));
    for chunk in values.chunks(4096) {
        buffer.clear();
        for v in chunk {
            buffer.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        hasher.update(&buffer);
    }
}

pub(super) fn step_key(n: usize, previous: &[f64], p: StepParameters<'_>) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"implexity-multirate-step/1");
    hasher.update((n as u64).to_le_bytes());
    hasher.update(p.time_scale.to_bits().to_le_bytes());
    hash_values(&mut hasher, p.design);
    hash_values(&mut hasher, previous);
    hasher.finalize().into()
}

pub(super) fn design_key(design: &[f64]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"implexity-multirate-design/1");
    hash_values(&mut hasher, design);
    hasher.finalize().into()
}


