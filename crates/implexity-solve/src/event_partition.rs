// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use implexity_core::error::{CaeError, CaeResult};

#[derive(Clone, Debug, PartialEq)]
pub struct EventPartition {
    pub event_values: Vec<f64>,
    pub event_signs: Vec<i8>,
    pub margin: f64,
}

#[must_use]
pub fn classify_events(values: &[f64], margin: f64) -> EventPartition {
    let signs = values
        .iter()
        .map(|&x| {
            if x.abs() <= margin || x == 0.0 || x.is_nan() {
                0
            } else if x > 0.0 {
                1
            } else {
                -1
            }
        })
        .collect();
    EventPartition { event_values: values.to_vec(), event_signs: signs, margin }
}

#[must_use]
pub fn partition_stable(reference: &EventPartition, candidate_values: &[f64]) -> (bool, EventPartition) {
    let c = classify_events(candidate_values, reference.margin);
    let stable = reference.event_signs.len() == c.event_signs.len()
        && reference.event_signs.iter().zip(&c.event_signs).all(|(a, b)| a == b && *b != 0);
    (stable, c)
}



pub fn require_stable_partition(
    reference: &EventPartition,
    candidate_values: &[f64],
) -> CaeResult<EventPartition> {
    let (ok, c) = partition_stable(reference, candidate_values);
    if !ok {
        return Err(CaeError::convergence(
            "active event partition changed or entered unresolved event layer; relinearization/generalized derivative required",
        ));
    }
    Ok(c)
}

