// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use implexity_core::contracts::ResponseSpec;
use implexity_optim::constraint_admission::{final_monitor_report, response_bound_report};
use serde_json::{Map, Value};

pub const TERMINAL_RESULT_STATES: [&str; 6] =
    ["completed", "stopped", "error", "discarded", "accepted", "superseded"];


pub fn validate_request(payload: &Value) -> Result<Map<String, Value>, String> {
    let allowed = ["job_id", "epoch", "expected_content_id", "expected_document_sha256"];
    let Some(p) = payload.as_object().filter(|p| p.keys().all(|k| allowed.contains(&k.as_str()))) else {
        return Err(
            "Use as working design accepts a job, optional epoch and current document identities.".into()
        );
    };
    let job_id = p.get("job_id").map(implexity_core::pyobj::py_str).unwrap_or_default();
    if !(job_id.len() == 12 && job_id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))) {
        return Err("Choose a recorded optimization job.".into());
    }
    if let Some(epoch) = p.get("epoch")
        && !(epoch.is_i64() && epoch.as_i64().is_some_and(|e| e >= 0) || epoch.is_u64())
    {
        return Err("The epoch must be a nonnegative integer.".into());
    }
    if p.get("expected_content_id").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err("Refresh the current model before using this result.".into());
    }
    if !p.get("expected_document_sha256").and_then(Value::as_str).is_some_and(crate::private::is_sha256) {
        return Err("The current document checksum is required to avoid replacing a newer edit.".into());
    }
    Ok(p.clone())
}

#[must_use]
pub fn assessment(
    responses: &[Value],
    row: &Map<String, Value>,
    schedule: Option<&[Value]>,
    source_status: &str,
    result_authority: &str,
) -> Value {
    let mut notes: Vec<Value> = Vec::new();
    let bounds = (|| {
        let specs: Vec<ResponseSpec> =
            responses.iter().map(ResponseSpec::from_dict).collect::<Result<_, _>>()?;
        let mut points = row.get("operating_points").cloned();
        if let Some(Value::Array(p)) = &points
            && row.get("robust_mode").and_then(Value::as_str) == Some("nominal")
        {
            points = Some(Value::Array(p.iter().take(1).cloned().collect()));
        }
        response_bound_report(&specs, row.get("terms"), points.as_ref().filter(|p| !p.is_null()))
    })();
    let (bounds, target_status) = match bounds {
        Ok(b) => {
            let met = b.get("bounds_satisfied").and_then(Value::as_bool).unwrap_or(false);
            if !met {
                notes.push(Value::String(
                    "Some declared response bounds are not met. They are penalty terms, so you can keep developing this design."
                        .into(),
                ));
            }
            (b, if met { "met" } else { "not_met" })
        }
        Err(e) => {
            notes.push(Value::String(format!("Target assessment is incomplete: {}", e.message())));
            (Value::Null, "not_assessed")
        }
    };
    let (monitors, monitor_status) = match final_monitor_report(row, schedule, false) {
        Ok(m) => {
            let status = if m.get("status").and_then(Value::as_str) == Some("not_declared") {
                "not_declared"
            } else if m.get("satisfied") == Some(&Value::Bool(true)) {
                "met"
            } else {
                notes.push(Value::String("Some engineering checks need further review.".into()));
                "not_met"
            };
            (m, status)
        }
        Err(e) => {
            notes.push(Value::String(format!("Engineering assessment is incomplete: {}", e.message())));
            (Value::Null, "not_assessed")
        }
    };
    if result_authority != "authoritative" {
        notes.push(Value::String(
            "This source run was exploratory. Its numerical limitations remain part of the record.".into(),
        ));
    }
    if source_status != "completed" && source_status != "accepted" {
        notes.push(Value::String(format!(
            "This is a saved checkpoint from a {source_status} run. Its source observations remain available."
        )));
    }
    notes.push(Value::String(
        "This working copy is not engineering approval. Changed geometry needs a fresh physics evaluation."
            .into(),
    ));
    let mut m = Map::new();
    m.insert("schema".into(), Value::String("implexity-working-design-assessment/2".into()));
    m.insert("advisory".into(), Value::Bool(true));
    m.insert("target_status".into(), Value::String(target_status.into()));
    m.insert("response_bounds".into(), bounds);
    m.insert("engineering_status".into(), Value::String(monitor_status.into()));
    m.insert("engineering_checks".into(), monitors);
    m.insert("final_acceptance_performed".into(), Value::Bool(false));
    m.insert("warnings".into(), Value::Array(notes));
    m.insert("observations_apply_to".into(), Value::String("source_snapshot_only".into()));
    Value::Object(m)
}

