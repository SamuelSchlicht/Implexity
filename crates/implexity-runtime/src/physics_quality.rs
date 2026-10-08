// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};

use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_optim::numeric::{float_value, format_g6};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QualityLevel {
    Sufficient = 0,
    FidelityInsufficient = 10,
    UnderResolved = 20,
    OutsideValidity = 30,
    EvidenceMissing = 40,
    DeclarationInvalid = 50,
}

impl QualityLevel {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sufficient => "sufficient",
            Self::FidelityInsufficient => "fidelity_insufficient",
            Self::UnderResolved => "under_resolved",
            Self::OutsideValidity => "outside_validity",
            Self::EvidenceMissing => "evidence_missing",
            Self::DeclarationInvalid => "declaration_invalid",
        }
    }

    #[must_use]
    pub const fn rank(self) -> i64 {
        self as i64
    }
}

fn fidelity_rank(level: &str) -> Option<i64> {
    match level {
        "screening" => Some(0),
        "intermediate" => Some(1),
        "resolved" => Some(2),
        "qualification" => Some(3),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidityBound {
    pub quantity: String,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub inclusive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscretisationRequirement {
    pub quantity: String,
    pub characteristic_size: f64,
    pub minimum_cells: f64,
    pub realised_spacing: Option<Value>,
    pub maximum_time_step: Option<f64>,
    pub realised_time_step: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AddinQualityDeclaration {
    pub addin_id: String,
    pub validity: Vec<ValidityBound>,
    pub discretisation: Vec<DiscretisationRequirement>,
    pub qualification_level: String,
    pub uncertainty_complete: bool,
    pub notes: Vec<String>,
}

fn keyword_error(class: &str, key: &str) -> CaeError {
    CaeError::contract(format!("{class}.__init__() got an unexpected keyword argument {}", repr_str(key)))
}

fn check_keys(m: &Map<String, Value>, class: &str, allowed: &[&str], required: &[&str]) -> CaeResult<()> {
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(keyword_error(class, k));
    }
    if let Some(k) = required.iter().find(|k| !m.contains_key(**k)) {
        return Err(CaeError::contract(format!(
            "{class}.__init__() missing 1 required positional argument: {}",
            repr_str(k)
        )));
    }
    Ok(())
}

fn as_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn opt_float(v: Option<&Value>) -> CaeResult<Option<f64>> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(x) => {
            as_float(x).map(Some).ok_or_else(|| CaeError::contract("could not convert value to float"))
        }
    }
}

impl AddinQualityDeclaration {

    pub fn from_value(value: &Value) -> CaeResult<Self> {
        let m =
            value.as_object().ok_or_else(|| CaeError::contract("quality declaration must be a mapping"))?;
        check_keys(
            m,
            "AddinQualityDeclaration",
            &[
                "addin_id",
                "validity",
                "discretisation",
                "qualification_level",
                "uncertainty_complete",
                "notes",
            ],
            &["addin_id"],
        )?;
        let mut validity = Vec::new();
        for row in m.get("validity").and_then(Value::as_array).into_iter().flatten() {
            let r = row.as_object().ok_or_else(|| CaeError::contract("validity bound must be a mapping"))?;
            check_keys(r, "ValidityBound", &["quantity", "lower", "upper", "inclusive"], &["quantity"])?;
            validity.push(ValidityBound {
                quantity: crate::pyval::py_str(&r["quantity"]),
                lower: opt_float(r.get("lower"))?,
                upper: opt_float(r.get("upper"))?,
                inclusive: r.get("inclusive").is_none_or(implexity_core::pyobj::truthy),
            });
        }
        let mut discretisation = Vec::new();
        for row in m.get("discretisation").and_then(Value::as_array).into_iter().flatten() {
            let r = row
                .as_object()
                .ok_or_else(|| CaeError::contract("discretisation requirement must be a mapping"))?;
            check_keys(
                r,
                "DiscretisationRequirement",
                &[
                    "quantity",
                    "characteristic_size",
                    "minimum_cells",
                    "realised_spacing",
                    "maximum_time_step",
                    "realised_time_step",
                ],
                &["quantity", "characteristic_size", "minimum_cells"],
            )?;
            discretisation.push(DiscretisationRequirement {
                quantity: crate::pyval::py_str(&r["quantity"]),
                characteristic_size: as_float(&r["characteristic_size"]).unwrap_or(f64::NAN),
                minimum_cells: as_float(&r["minimum_cells"]).unwrap_or(f64::NAN),
                realised_spacing: r.get("realised_spacing").filter(|v| !v.is_null()).cloned(),
                maximum_time_step: opt_float(r.get("maximum_time_step"))?,
                realised_time_step: r.get("realised_time_step").filter(|v| !v.is_null()).cloned(),
            });
        }
        Ok(Self {
            addin_id: crate::pyval::py_str(&m["addin_id"]),
            validity,
            discretisation,
            qualification_level: m
                .get("qualification_level")
                .map_or_else(|| "screening".to_string(), |v| crate::pyval::py_str(v).to_lowercase()),
            uncertainty_complete: m.get("uncertainty_complete").is_none_or(implexity_core::pyobj::truthy),
            notes: m
                .get("notes")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(crate::pyval::py_str).collect())
                .unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QualityIssue {
    pub level: QualityLevel,
    pub code: String,
    pub addin_id: String,
    pub quantity: String,
    pub message: String,
    pub evidence: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResponseQuality {
    pub response: String,
    pub level: QualityLevel,
    pub requested_level: String,
    pub inherited_addins: Vec<String>,
    pub issues: Vec<QualityIssue>,
}

impl ResponseQuality {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.level == QualityLevel::Sufficient
    }

    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "response": self.response,
            "level": self.level.name(),
            "level_rank": self.level.rank(),
            "requested_level": self.requested_level,
            "inherited_addins": self.inherited_addins,
            "issues": self.issues.iter().map(|i| json!({
                "level": i.level.name(),
                "code": i.code,
                "addin_id": i.addin_id,
                "quantity": i.quantity,
                "message": i.message,
                "evidence": i.evidence,
                "level_rank": i.level.rank(),
            })).collect::<Vec<_>>(),
        })
    }
}

fn closure(seeds: &[String], dependencies: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    fn visit(
        node: &str,
        deps: &BTreeMap<String, Vec<String>>,
        seen: &mut BTreeSet<String>,
        visiting: &mut BTreeSet<String>,
    ) {
        if seen.contains(node) || visiting.contains(node) {
            return;
        }
        visiting.insert(node.to_string());
        for upstream in deps.get(node).into_iter().flatten() {
            visit(upstream, deps, seen, visiting);
        }
        visiting.remove(node);
        seen.insert(node.to_string());
    }
    let mut seen = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    for s in seeds {
        visit(s, dependencies, &mut seen, &mut visiting);
    }
    seen.into_iter().collect()
}

fn range_of(value: Option<&Value>) -> Option<(f64, f64)> {
    fn flatten(v: &Value, out: &mut Vec<f64>) -> bool {
        match v {
            Value::Array(items) => items.iter().all(|i| flatten(i, out)),
            other => as_float(other).is_some_and(|f| {
                out.push(f);
                true
            }),
        }
    }
    let mut values = Vec::new();
    if !flatten(value?, &mut values) || values.is_empty() || values.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some((lo, hi))
}

fn opt_value(v: Option<f64>) -> Value {
    v.map_or(Value::Null, float_value)
}

fn issue(
    level: QualityLevel,
    code: &str,
    addin: &str,
    quantity: &str,
    message: String,
    evidence: Value,
) -> QualityIssue {
    QualityIssue {
        level,
        code: code.into(),
        addin_id: addin.into(),
        quantity: quantity.into(),
        message,
        evidence,
    }
}

#[allow(clippy::too_many_lines)]
#[must_use]
pub fn propagate_response_quality(
    response_dependencies: &[(String, Vec<String>)],
    addin_dependencies: &BTreeMap<String, Vec<String>>,
    declarations: &BTreeMap<String, AddinQualityDeclaration>,
    realised_values: &Map<String, Value>,
    requested_level: &str,
) -> Vec<(String, ResponseQuality)> {
    let requested = requested_level.to_lowercase();
    let requested_rank = fidelity_rank(&requested);
    let mut result = Vec::new();
    for (response, seeds) in response_dependencies {
        let inherited = closure(seeds, addin_dependencies);
        let mut issues = Vec::new();
        if requested_rank.is_none() {
            issues.push(issue(
                QualityLevel::DeclarationInvalid,
                "UNKNOWN_REQUESTED_QUALITY_LEVEL",
                "",
                "",
                format!("unknown requested quality level {}", repr_str(&requested)),
                json!({"requested_level": requested}),
            ));
        }
        for addin_id in &inherited {
            let Some(declaration) = declarations.get(addin_id) else {
                issues.push(issue(
                    QualityLevel::EvidenceMissing,
                    "QUALITY_DECLARATION_MISSING",
                    addin_id,
                    "",
                    format!("upstream add-in {} has no quality declaration", repr_str(addin_id)),
                    json!({}),
                ));
                continue;
            };
            match fidelity_rank(&declaration.qualification_level) {
                None => issues.push(issue(
                    QualityLevel::DeclarationInvalid,
                    "UNKNOWN_DECLARED_QUALITY_LEVEL",
                    addin_id,
                    "",
                    format!(
                        "add-in {} declares unknown level {}",
                        repr_str(addin_id),
                        repr_str(&declaration.qualification_level)
                    ),
                    json!({"declared_level": declaration.qualification_level}),
                )),
                Some(rank) => {
                    if let Some(req) = requested_rank
                        && rank < req
                    {
                        issues.push(issue(
                            QualityLevel::FidelityInsufficient,
                            "REQUESTED_FIDELITY_NOT_MET",
                            addin_id,
                            "",
                            format!(
                                "add-in {} is {}, below requested {}",
                                repr_str(addin_id),
                                repr_str(&declaration.qualification_level),
                                repr_str(&requested)
                            ),
                            json!({"declared_rank": rank, "requested_rank": req}),
                        ));
                    }
                }
            }
            if !declaration.uncertainty_complete {
                issues.push(issue(
                    QualityLevel::EvidenceMissing,
                    "UNCERTAINTY_DECLARATION_INCOMPLETE",
                    addin_id,
                    "",
                    format!("add-in {} has incomplete uncertainty evidence", repr_str(addin_id)),
                    json!({}),
                ));
            }
            for bound in &declaration.validity {
                let Some((lo, hi)) = range_of(realised_values.get(&bound.quantity)) else {
                    issues.push(issue(
                        QualityLevel::EvidenceMissing,
                        "VALIDITY_EVIDENCE_MISSING",
                        addin_id,
                        &bound.quantity,
                        format!(
                            "no finite realised value is available for validity quantity {}",
                            repr_str(&bound.quantity)
                        ),
                        json!({"lower": opt_value(bound.lower), "upper": opt_value(bound.upper)}),
                    ));
                    continue;
                };
                let lower_bad = bound.lower.is_some_and(|l| if bound.inclusive { lo < l } else { lo <= l });
                let upper_bad = bound.upper.is_some_and(|u| if bound.inclusive { hi > u } else { hi >= u });
                if lower_bad || upper_bad {
                    issues.push(issue(
                        QualityLevel::OutsideValidity,
                        "CONSTITUTIVE_VALIDITY_EXCEEDED",
                        addin_id,
                        &bound.quantity,
                        format!(
                            "realised range [{}, {}] lies outside the declared validity interval",
                            format_g6(lo),
                            format_g6(hi)
                        ),
                        json!({"realised_min": float_value(lo), "realised_max": float_value(hi),
                               "lower": opt_value(bound.lower), "upper": opt_value(bound.upper),
                               "inclusive": bound.inclusive}),
                    ));
                }
            }
            for requirement in &declaration.discretisation {
                let size = requirement.characteristic_size;
                let minimum = requirement.minimum_cells;
                let spacing = requirement
                    .realised_spacing
                    .as_ref()
                    .or_else(|| realised_values.get(&format!("spacing:{}", requirement.quantity)));
                let spacing_value = spacing.and_then(as_float).unwrap_or(f64::NAN);
                if !(size.is_finite() && size > 0.0 && minimum.is_finite() && minimum > 0.0) {
                    issues.push(issue(
                        QualityLevel::DeclarationInvalid,
                        "DISCRETISATION_REQUIREMENT_INVALID",
                        addin_id,
                        &requirement.quantity,
                        "characteristic size and minimum cell count must be finite and positive".into(),
                        json!({"characteristic_size": float_value(size), "minimum_cells": float_value(minimum)}),
                    ));
                } else if !(spacing_value.is_finite() && spacing_value > 0.0) {
                    issues.push(issue(
                        QualityLevel::EvidenceMissing,
                        "SPATIAL_RESOLUTION_EVIDENCE_MISSING",
                        addin_id,
                        &requirement.quantity,
                        format!(
                            "no finite positive spacing is available for {}",
                            repr_str(&requirement.quantity)
                        ),
                        json!({}),
                    ));
                } else {
                    let cells = size / spacing_value;
                    if cells < minimum {
                        issues.push(issue(
                            QualityLevel::UnderResolved,
                            "SPATIAL_DISCRETISATION_INSUFFICIENT",
                            addin_id,
                            &requirement.quantity,
                            format!(
                                "{} cells span the characteristic scale; at least {} are required",
                                format_g6(cells),
                                format_g6(minimum)
                            ),
                            json!({"characteristic_size": float_value(size),
                                   "realised_spacing": float_value(spacing_value),
                                   "realised_cells": float_value(cells),
                                   "minimum_cells": float_value(minimum)}),
                        ));
                    }
                }
                if let Some(maximum) = requirement.maximum_time_step {
                    let dt = requirement
                        .realised_time_step
                        .as_ref()
                        .or_else(|| realised_values.get(&format!("time_step:{}", requirement.quantity)));
                    let dt_value = dt.and_then(as_float).unwrap_or(f64::NAN);
                    if !(dt_value.is_finite() && dt_value > 0.0) {
                        issues.push(issue(
                            QualityLevel::EvidenceMissing,
                            "TEMPORAL_RESOLUTION_EVIDENCE_MISSING",
                            addin_id,
                            &requirement.quantity,
                            format!(
                                "no finite positive time step is available for {}",
                                repr_str(&requirement.quantity)
                            ),
                            json!({}),
                        ));
                    } else if dt_value > maximum {
                        issues.push(issue(
                            QualityLevel::UnderResolved,
                            "TEMPORAL_DISCRETISATION_INSUFFICIENT",
                            addin_id,
                            &requirement.quantity,
                            format!("realised time step {} exceeds {}", format_g6(dt_value), format_g6(maximum)),
                            json!({"realised_time_step": float_value(dt_value), "maximum_time_step": float_value(maximum)}),
                        ));
                    }
                }
            }
        }
        let level = issues.iter().map(|i| i.level).max().unwrap_or(QualityLevel::Sufficient);
        result.push((
            response.clone(),
            ResponseQuality {
                response: response.clone(),
                level,
                requested_level: requested.clone(),
                inherited_addins: inherited,
                issues,
            },
        ));
    }
    result
}
