// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use serde_json::{Value, json};

use implexity_core::CaeResult;
use implexity_physics_solid::history::ageing::{observe_temperature_history, prepare_cell_observer};


pub fn admit_initial(
    specification: &Value,
    shape: [usize; 3],
    times: &[f64],
    indices: Option<&[i64]>,
    initial: &[f64],
) -> CaeResult<()> {
    let rows: Vec<&[f64]> = times.iter().map(|_| initial).collect();
    observe(specification, shape, times, indices, &rows).map(|_| ())
}


pub fn observe(
    specification: &Value,
    shape: [usize; 3],
    times: &[f64],
    indices: Option<&[i64]>,
    temperature: &[&[f64]],
) -> CaeResult<Value> {
    let (settings, context, composition) = prepare_cell_observer(specification, &shape, times, indices)?;
    observe_temperature_history(&settings, &context, &json!(temperature), &json!(composition))
}
