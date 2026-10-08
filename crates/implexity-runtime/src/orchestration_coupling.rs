// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use implexity_core::coupling_graph::validate_provider_couplings;
use implexity_core::orchestration::{ExecutionKind, OrchestrationPlan, PlanStatus, RegisteredAddIn};
use implexity_core::{CaeError, CaeResult};
use serde_json::{Map, Value, json};

use crate::addin::{JsonMap, addin_operations};

pub const REPORT_SCHEMA: &str = "implexity-physics-coupling-report/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvidence {
    Residual(Vec<(String, String, String, String)>),
    Algebraic,
}

fn report(
    provider: &str,
    ok: bool,
    admission: &str,
    errors: Vec<Value>,
    warnings: Vec<Value>,
    extra: Vec<(&str, Value)>,
) -> JsonMap {
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(ok));
    out.insert("schema".into(), Value::String(REPORT_SCHEMA.into()));
    out.insert("provider".into(), Value::String(provider.into()));
    out.insert("activePhysics".into(), json!([]));
    out.insert("requiredEdges".into(), json!([]));
    out.insert("declaredEdges".into(), json!([]));
    out.insert("closedLoops".into(), json!([]));
    out.insert("errors".into(), Value::Array(errors));
    out.insert("warnings".into(), Value::Array(warnings));
    if !admission.is_empty() {
        out.insert("admission".into(), Value::String(admission.into()));
    }
    for (k, v) in extra {
        out.insert(k.into(), v);
    }
    out
}

fn error_row(code: &str, message: &str) -> Value {
    json!({"code": code, "message": message})
}


pub fn validated_report(raw: &Value, aid: &str) -> CaeResult<JsonMap> {
    let Some(r) = raw.as_object() else {
        return Err(CaeError::contract(format!("{aid}: coupling_validation must return a mapping")));
    };
    let mut report = r.clone();
    if report.get("schema").and_then(Value::as_str) != Some(REPORT_SCHEMA) {
        return Err(CaeError::contract(format!("{aid}: coupling report has an invalid or missing schema")));
    }
    if report.get("provider").and_then(Value::as_str) != Some(aid) {
        return Err(CaeError::contract(format!(
            "{aid}: coupling report provider identity does not match its selected add-in"
        )));
    }
    for key in ["errors", "warnings"] {
        let rows = report.get(key).cloned().unwrap_or(Value::Array(Vec::new()));
        match rows {
            Value::Array(items) if items.iter().all(Value::is_object) => {
                report.insert(key.into(), Value::Array(items));
            }
            _ => {
                return Err(CaeError::contract(format!(
                    "{aid}: coupling report {key} must contain mappings"
                )));
            }
        }
    }
    let Some(ok) = report.get("ok").and_then(Value::as_bool) else {
        return Err(CaeError::contract(format!("{aid}: coupling report requires an explicit boolean ok")));
    };
    let has_errors = report["errors"].as_array().is_some_and(|e| !e.is_empty());
    if ok == has_errors {
        return Err(CaeError::contract(format!(
            "{aid}: coupling report ok flag contradicts its error evidence"
        )));
    }
    let active_ok = report
        .get("activePhysics")
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())));
    if !active_ok {
        return Err(CaeError::contract(format!(
            "{aid}: coupling report activePhysics must contain non-empty strings"
        )));
    }
    for key in ["requiredEdges", "declaredEdges"] {
        let ok = report.get(key).and_then(Value::as_array).is_some_and(|a| a.iter().all(Value::is_object));
        if !ok {
            return Err(CaeError::contract(format!("{aid}: coupling report {key} must contain mappings")));
        }
    }
    let loops_ok = report.get("closedLoops").and_then(Value::as_array).is_some_and(|a| {
        a.iter().all(|row| {
            row.as_array().is_some_and(|r| r.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())))
        })
    });
    if !loops_ok {
        return Err(CaeError::contract(format!(
            "{aid}: coupling report closedLoops must contain string sequences"
        )));
    }
    Ok(report)
}

fn strict_native_admission(
    aid: &str,
    entry: &RegisteredAddIn,
    plan: &OrchestrationPlan,
    runtime: Option<&RuntimeEvidence>,
) -> JsonMap {
    match entry.contract.execution_kind {
        Some(ExecutionKind::Residual) => {
            let incident: Vec<_> =
                plan.coupling_edges.iter().filter(|e| e.source == aid || e.target == aid).collect();
            if !incident.is_empty() && runtime.is_none() {
                return report(
                    aid,
                    false,
                    "",
                    vec![error_row(
                        "NUMERICAL_COUPLING_EVIDENCE_REQUIRED",
                        "strict residual coupling must be admitted by its instantiated composite runtime",
                    )],
                    vec![],
                    vec![],
                );
            }
            let bound: BTreeSet<(String, String, String, String)> = match runtime {
                Some(RuntimeEvidence::Residual(rows)) => rows.iter().cloned().collect(),
                _ => BTreeSet::new(),
            };
            let missing = incident.iter().any(|e| {
                !bound.contains(&(
                    e.source.clone(),
                    e.target.clone(),
                    e.source_port_id.clone(),
                    e.target_port_id.clone(),
                ))
            });
            if missing {
                return report(
                    aid,
                    false,
                    "",
                    vec![error_row(
                        "NUMERICAL_COUPLING_BINDING_MISSING",
                        "a semantic residual edge has no exact forward/transpose numerical binding",
                    )],
                    vec![],
                    vec![],
                );
            }
            report(aid, true, "residual_runtime_numerical_bindings", vec![], vec![], vec![])
        }
        Some(ExecutionKind::Algebraic) => {
            if runtime.is_none() {
                return report(
                    aid,
                    false,
                    "",
                    vec![error_row(
                        "ALGEBRAIC_EXECUTION_EVIDENCE_REQUIRED",
                        "strict algebraic coupling must be admitted by its instantiated typed runtime",
                    )],
                    vec![],
                    vec![],
                );
            }
            report(aid, true, "typed_algebraic_execution", vec![], vec![], vec![])
        }
        Some(ExecutionKind::Operation | ExecutionKind::LifecycleExtension) => {
            if !plan.coupling_edges.is_empty() {
                return report(
                    aid,
                    false,
                    "",
                    vec![error_row(
                        "OPERATION_COUPLING_NOT_NUMERICAL",
                        "operation add-ins cannot qualify numerical coupling edges",
                    )],
                    vec![],
                    vec![],
                );
            }
            report(aid, true, "declared_operation_without_numerical_edges", vec![], vec![], vec![])
        }
        other => {
            let kind = other.map_or("undeclared", |k| k.as_str());
            report(
                aid,
                false,
                "",
                vec![error_row(
                    "CHILD_COUPLING_VALIDATION_REQUIRED",
                    &format!("strict {kind} execution requires explicit coupling validation"),
                )],
                vec![],
                vec![],
            )
        }
    }
}


#[allow(clippy::too_many_lines)]
pub fn validate_orchestration_couplings(
    provider_id: &str,
    plan: &OrchestrationPlan,
    entries: &BTreeMap<String, Arc<RegisteredAddIn>>,
    ctx: &JsonMap,
    for_optimization: bool,
    runtime: Option<&RuntimeEvidence>,
) -> CaeResult<JsonMap> {
    if plan.status != PlanStatus::Ready {
        let reasons = if !plan.blocked_reasons.is_empty() {
            &plan.blocked_reasons
        } else if !plan.missing_physics.is_empty() {
            &plan.missing_physics
        } else {
            &plan.missing_authoring
        };
        let errors = reasons.iter().map(|r| error_row("ORCHESTRATION_PLAN_NOT_READY", r)).collect();
        return Ok(report(
            provider_id,
            false,
            "",
            errors,
            vec![],
            vec![("orchestrationPlan", plan.as_dict()), ("childReports", json!({}))],
        ));
    }
    let provider_problems = crate::pyval::mapping_or_empty(ctx.get("provider_problems"));
    let mut child_reports = Map::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut warnings: Vec<Value> = Vec::new();
    let mut active: BTreeSet<String> = BTreeSet::new();
    for aid in &plan.selected_addins {
        let Some(entry) = entries.get(aid) else {
            errors.push(json!({
                "code": "ORCHESTRATED_ADDIN_MISSING",
                "message": format!("selected add-in {} is no longer registered", implexity_core::py_repr::repr_str(aid)),
                "addin": aid,
            }));
            continue;
        };
        let compat = entry.compatibility_mode || entry.contract.compatibility_mode;
        let adapter = entry.adapter.as_deref();
        let provider = adapter.and_then(implexity_core::orchestration::AddInAdapter::provider);
        let adapter_ops = adapter.and_then(addin_operations).filter(|o| o.provides("coupling_validation"));
        let raw_problem = provider_problems
            .get(aid)
            .or_else(|| ctx.get("problem"))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let report_row = if let Some(provider) = &provider {
            let child_problem = provider.normalise_problem(&raw_problem)?;
            match provider.coupling_validation(Some(&child_problem), for_optimization) {
                Some(raw) => {
                    let raw = raw.map_err(CaeError::contract)?;
                    validated_report(&raw, aid)?
                }
                None if compat => {
                    let extensions = &implexity_core::registries::global().extensions;
                    let raw = validate_provider_couplings(
                        provider.as_ref(),
                        Some(&child_problem),
                        extensions,
                        for_optimization,
                    );
                    let mut r = validated_report(&raw, aid)?;
                    r.entry("admission")
                        .or_insert(Value::String("legacy_application_ontology_compatibility".into()));
                    r
                }
                None => report(
                    aid,
                    false,
                    "",
                    vec![error_row(
                        "CHILD_COUPLING_VALIDATION_REQUIRED",
                        "strict provider add-in has no physics-owned coupling_validation callback",
                    )],
                    vec![],
                    vec![],
                ),
            }
        } else if let Some(ops) = adapter_ops {
            let child_problem = match ops.normalise_problem(&raw_problem) {
                Some(p) => p?,
                None => raw_problem.clone(),
            };
            let raw = ops
                .coupling_validation(&child_problem, for_optimization)
                .ok_or_else(|| CaeError::contract(format!("{aid}: coupling_validation disappeared")))??;
            validated_report(&raw, aid)?
        } else if compat {
            report(
                aid,
                true,
                "legacy_semantic_ports_noncanonical",
                vec![],
                vec![json!({
                    "code": "LEGACY_SEMANTIC_COUPLING_ONLY",
                    "message": "compatibility add-in has semantic port admission only",
                })],
                vec![],
            )
        } else {
            strict_native_admission(aid, entry, plan, runtime)
        };
        for v in report_row.get("activePhysics").and_then(Value::as_array).into_iter().flatten() {
            active.insert(crate::pyval::py_str(v));
        }
        for row in report_row.get("errors").and_then(Value::as_array).into_iter().flatten() {
            let mut item = row.as_object().cloned().unwrap_or_default();
            item.entry("addin").or_insert(Value::String(aid.clone()));
            errors.push(Value::Object(item));
        }
        for row in report_row.get("warnings").and_then(Value::as_array).into_iter().flatten() {
            let mut item = row.as_object().cloned().unwrap_or_default();
            item.entry("addin").or_insert(Value::String(aid.clone()));
            warnings.push(Value::Object(item));
        }
        child_reports.insert(aid.clone(), Value::Object(report_row));
    }
    let edges: Vec<Value> = plan
        .coupling_edges
        .iter()
        .map(|e| {
            json!({
                "source": e.source,
                "target": e.target,
                "quantity": e.quantity,
                "source_port_id": e.source_port_id,
                "target_port_id": e.target_port_id,
            })
        })
        .collect();
    let ok = errors.is_empty();
    Ok(report(
        provider_id,
        ok,
        "",
        errors,
        warnings,
        vec![
            ("activePhysics", Value::Array(active.into_iter().map(Value::String).collect())),
            ("requiredEdges", Value::Array(edges.clone())),
            ("declaredEdges", Value::Array(edges)),
            ("orchestrationPlan", plan.as_dict()),
            ("childReports", Value::Object(child_reports)),
        ],
    ))
}
