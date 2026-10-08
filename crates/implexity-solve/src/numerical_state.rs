// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::{Mutex, OnceLock};

pub type ResetFn = fn() -> usize;

fn registry() -> &'static Mutex<Vec<(&'static str, ResetFn)>> {
    static REGISTRY: OnceLock<Mutex<Vec<(&'static str, ResetFn)>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn register_reset(name: &'static str, reset: ResetFn) {
    let mut r = registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(entry) = r.iter_mut().find(|(n, _)| *n == name) {
        entry.1 = reset;
    } else {
        r.push((name, reset));
    }
}

#[must_use]
pub fn reset_all() -> Vec<(String, usize)> {
    let resets: Vec<(&'static str, ResetFn)> =
        registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    resets.into_iter().map(|(name, reset)| (name.to_owned(), reset())).collect()
}

