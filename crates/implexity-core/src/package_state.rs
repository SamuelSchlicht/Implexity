// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::sync::Mutex;

use crate::sync::lock;

static SELECTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[must_use]
pub fn selected() -> Vec<String> {
    lock(&SELECTED).clone()
}

pub fn replace(names: &[String]) {
    let mut snapshot: Vec<String> = names.to_vec();
    snapshot.sort();
    *lock(&SELECTED) = snapshot;
}

#[must_use]
pub fn is_selected(package: &str) -> bool {
    lock(&SELECTED).iter().any(|p| p == package)
}
