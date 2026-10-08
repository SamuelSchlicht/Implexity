// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;
use std::fmt::Debug;
use std::sync::Arc;

use implexity_core::error::{CaeError, CaeResult};
use implexity_core::py_repr::repr_str;
use implexity_linalg::sparse::CsrMatrix;
use serde_json::{Map, Value, json};

use crate::certificate::norm2;

pub const GENERIC_SCHEMA_ID: &str = "implexity-coupled-convergence/2";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldPolicy {
    pub scale: f64,
    pub tolerance: f64,
}

impl FieldPolicy {


    pub fn new(scale: f64, tolerance: f64) -> CaeResult<Self> {
        for (label, value) in [("scale", scale), ("tolerance", tolerance)] {
            if !value.is_finite() || value <= 0.0 {
                return Err(CaeError::contract(format!(
                    "FieldPolicy.{label} must be a finite positive real number"
                )));
            }
        }
        Ok(Self { scale, tolerance })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CoupledConvergencePolicy {
    fields: Vec<(String, FieldPolicy)>,
}

impl CoupledConvergencePolicy {


    pub fn new(fields: Vec<(String, FieldPolicy)>) -> CaeResult<Self> {
        let mut seen = BTreeSet::new();
        for (name, _) in &fields {
            if name.is_empty() {
                return Err(CaeError::contract(
                    "CoupledConvergencePolicy field names must be non-empty strings",
                ));
            }
            if !seen.insert(name.as_str()) {
                return Err(CaeError::contract("CoupledConvergencePolicy field names must be unique"));
            }
        }
        Ok(Self { fields })
    }

    #[must_use]
    pub fn fields(&self) -> &[(String, FieldPolicy)] {
        &self.fields
    }

    #[must_use]
    pub fn names(&self) -> BTreeSet<String> {
        self.fields.iter().map(|(n, _)| n.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConvergenceReport {
    pub certified: bool,
    pub normalized: Vec<(String, f64)>,
    pub passed: Vec<(String, bool)>,
    pub reason: String,
}

impl ConvergenceReport {
    #[must_use]
    pub fn as_value(&self) -> Value {
        let normalized: Map<String, Value> =
            self.normalized.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        let passed: Map<String, Value> = self.passed.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
        json!({"certified": self.certified, "normalized": normalized, "passed": passed, "reason": self.reason})
    }

    #[must_use]
    pub fn failed_fields(&self) -> Vec<String> {
        let mut out: Vec<String> = self.passed.iter().filter(|(_, ok)| !ok).map(|(n, _)| n.clone()).collect();
        out.sort();
        out
    }
}

fn positive_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}




pub fn parse_policy(
    payload: &Value,
    required_fields: Option<&[(&str, &str)]>,
    schema_id: &str,
) -> CaeResult<CoupledConvergencePolicy> {
    let Value::Object(map) = payload else {
        return Err(CaeError::contract("coupled_convergence_policy must be a dict"));
    };
    if map.get("schema").and_then(Value::as_str) != Some(schema_id) {
        return Err(CaeError::contract(format!(
            "coupled_convergence_policy.schema must equal {}",
            repr_str(schema_id)
        )));
    }
    let table: Vec<(String, String)> = match required_fields {
        None => map.keys().filter(|k| *k != "schema").map(|k| (k.clone(), "scale".to_string())).collect(),
        Some(fields) => fields.iter().map(|(n, s)| ((*n).to_string(), (*s).to_string())).collect(),
    };
    let expected: BTreeSet<&str> =
        std::iter::once("schema").chain(table.iter().map(|(n, _)| n.as_str())).collect();
    let actual: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    if expected != actual {
        return Err(CaeError::contract(
            "coupled_convergence_policy keys do not match the declared field set",
        ));
    }
    let mut fields = Vec::with_capacity(table.len());
    for (name, scale_key) in &table {
        let row = map.get(name);
        let ok = matches!(row, Some(Value::Object(r)) if r.len() == 2 && r.contains_key(scale_key) && r.contains_key("tolerance"));
        let Some(Value::Object(row)) = row.filter(|_| ok) else {
            return Err(CaeError::contract(format!(
                "coupled_convergence_policy.{name} requires {scale_key} and tolerance"
            )));
        };
        let scale = positive_number(&row[scale_key.as_str()]);
        let tolerance = positive_number(&row["tolerance"]);
        let policy = match (scale, tolerance) {
            (Some(s), Some(t)) => FieldPolicy::new(s, t),
            (None, _) => Err(CaeError::contract("FieldPolicy.scale must be a finite positive real number")),
            (_, None) => {
                Err(CaeError::contract("FieldPolicy.tolerance must be a finite positive real number"))
            }
        }
        .map_err(|e| CaeError::contract(format!("coupled_convergence_policy.{name}: {}", e.message())))?;
        fields.push((name.clone(), policy));
    }
    CoupledConvergencePolicy::new(fields)
}



pub fn assess(norms: &[(String, f64)], policy: &CoupledConvergencePolicy) -> CaeResult<ConvergenceReport> {
    let lookup = |name: &str| norms.iter().find(|(n, _)| n == name).map(|(_, v)| *v);
    let mut missing: Vec<&str> =
        policy.fields.iter().map(|(n, _)| n.as_str()).filter(|n| lookup(n).is_none()).collect();
    if !missing.is_empty() {
        missing.sort_unstable();
        let list: Vec<String> = missing.iter().map(|m| repr_str(m)).collect();
        return Err(CaeError::contract(format!(
            "residual_norms is missing entries for [{}]",
            list.join(", ")
        )));
    }
    let mut normalized = Vec::new();
    let mut passed = Vec::new();
    for (name, item) in &policy.fields {
        let raw = lookup(name).unwrap_or(f64::NAN);
        if !raw.is_finite() || raw < 0.0 {
            return Err(CaeError::contract(format!(
                "residual_norms[{}] must be a non-negative finite real number",
                repr_str(name)
            )));
        }
        let n = raw / item.scale;
        normalized.push((name.clone(), n));
        passed.push((name.clone(), n.is_finite() && n <= item.tolerance));
    }
    let certified = !passed.is_empty() && passed.iter().all(|(_, ok)| *ok);
    let reason = if certified {
        "authored residual policy evaluated"
    } else {
        "one or more fields did not meet their authored tolerance"
    };
    Ok(ConvergenceReport { certified, normalized, passed, reason: reason.into() })
}


pub trait ConvergenceCriterion: Debug + Send + Sync {
    fn state_size(&self) -> Option<usize>;


    fn assess(&self, residual: &[f64], norm: Option<f64>) -> CaeResult<ConvergenceReport>;
    fn describe(&self) -> Value;
    fn is_scalar(&self) -> bool {
        false
    }
    fn policy_fields(&self) -> Option<BTreeSet<String>> {
        None
    }
}

pub type Criterion = Arc<dyn ConvergenceCriterion>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScalarL2Criterion {
    tolerance: f64,
}

impl ScalarL2Criterion {


    pub fn new(tolerance: f64) -> CaeResult<Self> {
        if !tolerance.is_finite() || tolerance <= 0.0 {
            return Err(CaeError::contract("residual tolerance must be finite and positive"));
        }
        Ok(Self { tolerance })
    }

    #[must_use]
    pub fn tolerance(&self) -> f64 {
        self.tolerance
    }



    pub fn shared(tolerance: f64) -> CaeResult<Criterion> {
        Ok(Arc::new(Self::new(tolerance)?))
    }
}

impl ConvergenceCriterion for ScalarL2Criterion {
    fn state_size(&self) -> Option<usize> {
        None
    }
    fn assess(&self, residual: &[f64], norm: Option<f64>) -> CaeResult<ConvergenceReport> {
        let rn = norm.unwrap_or_else(|| norm2(residual));
        let ok = rn <= self.tolerance;
        Ok(ConvergenceReport {
            certified: ok,
            normalized: Vec::new(),
            passed: Vec::new(),
            reason: if ok {
                "scalar residual norm within tolerance"
            } else {
                "scalar residual norm above tolerance"
            }
            .into(),
        })
    }
    fn describe(&self) -> Value {
        json!({"kind": "scalar_l2", "tolerance": self.tolerance})
    }
    fn is_scalar(&self) -> bool {
        true
    }
}

pub type FieldMembers = Vec<(String, Vec<usize>)>;



pub fn checked_members(members: &[(String, Vec<i64>)], state_size: usize) -> CaeResult<FieldMembers> {
    let mut out = Vec::with_capacity(members.len());
    for (name, raw) in members {
        if name.is_empty() {
            return Err(CaeError::contract("field_members keys must be non-empty strings"));
        }
        if raw.is_empty() {
            return Err(CaeError::contract(format!(
                "field_members[{}] must be a non-empty one-dimensional integer index array",
                repr_str(name)
            )));
        }
        if raw.iter().any(|&i| i < 0 || usize::try_from(i).map_or(true, |i| i >= state_size)) {
            return Err(CaeError::contract(format!(
                "field_members[{}] has indices outside the residual frame [0, {state_size})",
                repr_str(name)
            )));
        }
        let mut idx: Vec<usize> = raw.iter().map(|&i| usize::try_from(i).unwrap_or(0)).collect();
        idx.sort_unstable();
        if idx.windows(2).any(|w| w[0] == w[1]) {
            return Err(CaeError::contract(format!(
                "field_members[{}] contains duplicate indices",
                repr_str(name)
            )));
        }
        out.push((name.clone(), idx));
    }
    Ok(out)
}

fn to_i64(members: &FieldMembers) -> Vec<(String, Vec<i64>)> {
    members
        .iter()
        .map(|(n, v)| (n.clone(), v.iter().map(|&i| i64::try_from(i).unwrap_or(i64::MAX)).collect()))
        .collect()
}

fn py_int_list(values: &[usize]) -> String {
    let inner: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("[{}]", inner.join(", "))
}

fn py_str_list(values: &[String]) -> String {
    let inner: Vec<String> = values.iter().map(|v| repr_str(v)).collect();
    format!("[{}]", inner.join(", "))
}

#[derive(Clone, Debug)]
pub struct PerFieldCriterion {
    policy: CoupledConvergencePolicy,
    members: FieldMembers,
    state_size: usize,
}

impl PerFieldCriterion {


    pub fn new(
        policy: CoupledConvergencePolicy,
        field_members: &[(String, Vec<i64>)],
        state_size: usize,
    ) -> CaeResult<Self> {
        if policy.fields.is_empty() {
            return Err(CaeError::contract("per-field convergence policy must name at least one field"));
        }
        if state_size < 1 {
            return Err(CaeError::contract("PerFieldCriterion.state_size must be a positive integer"));
        }
        let members = checked_members(field_members, state_size)?;
        let policy_fields = policy.names();
        let member_fields: BTreeSet<String> = members.iter().map(|(n, _)| n.clone()).collect();
        if policy_fields != member_fields {
            let policy_only: Vec<String> = policy_fields.difference(&member_fields).cloned().collect();
            let members_only: Vec<String> = member_fields.difference(&policy_fields).cloned().collect();
            return Err(CaeError::contract(format!(
                "per-field convergence policy fields must match the residual field set exactly; policy_only={}; members_only={}",
                py_str_list(&policy_only),
                py_str_list(&members_only)
            )));
        }
        let mut covered: Vec<usize> = members.iter().flat_map(|(_, v)| v.iter().copied()).collect();
        covered.sort_unstable();
        let exact = covered.len() == state_size && covered.iter().enumerate().all(|(i, &v)| i == v);
        if !exact {
            let mut shared = Vec::new();
            for w in covered.windows(2) {
                if w[0] == w[1] && shared.last() != Some(&w[0]) {
                    shared.push(w[0]);
                }
            }
            shared.truncate(8);
            let present: BTreeSet<usize> = covered.iter().copied().collect();
            let missing: Vec<usize> = (0..state_size).filter(|i| !present.contains(i)).take(8).collect();
            return Err(CaeError::contract(format!(
                "per-field convergence policy must cover every unknown of the residual frame exactly once ({} memberships for {state_size} unknowns; shared={}; uncovered={})",
                covered.len(),
                py_int_list(&shared),
                py_int_list(&missing)
            )));
        }
        Ok(Self { policy, members, state_size })
    }

    #[must_use]
    pub fn policy(&self) -> &CoupledConvergencePolicy {
        &self.policy
    }

    #[must_use]
    pub fn members(&self) -> &FieldMembers {
        &self.members
    }
}

impl ConvergenceCriterion for PerFieldCriterion {
    fn state_size(&self) -> Option<usize> {
        Some(self.state_size)
    }
    fn assess(&self, residual: &[f64], _norm: Option<f64>) -> CaeResult<ConvergenceReport> {
        if residual.len() != self.state_size {
            return Err(CaeError::contract(format!(
                "per-field convergence criterion is bound to {} unknowns; received a residual of length {}",
                self.state_size,
                residual.len()
            )));
        }
        let norms: Vec<(String, f64)> = self
            .members
            .iter()
            .map(|(name, idx)| {
                let sub: Vec<f64> = idx.iter().map(|&i| residual[i]).collect();
                (name.clone(), norm2(&sub))
            })
            .collect();
        assess(&norms, &self.policy)
    }
    fn describe(&self) -> Value {
        let mut fields = Map::new();
        for (name, item) in &self.policy.fields {
            let count = self.members.iter().find(|(n, _)| n == name).map_or(0, |(_, v)| v.len());
            fields.insert(
                name.clone(),
                json!({"scale": item.scale, "tolerance": item.tolerance, "unknowns": count}),
            );
        }
        json!({"kind": "per_field_l2", "schema": GENERIC_SCHEMA_ID, "state_size": self.state_size, "fields": fields})
    }
    fn policy_fields(&self) -> Option<BTreeSet<String>> {
        Some(self.policy.names())
    }
}



pub fn field_members_from_slices(
    field_slices: &[(String, Vec<(usize, usize)>)],
    state_size: usize,
) -> CaeResult<Vec<(String, Vec<i64>)>> {
    let mut out = Vec::new();
    for (name, slices) in field_slices {
        let mut pieces = Vec::new();
        for &(start, stop) in slices {
            if stop <= start || stop > state_size {
                return Err(CaeError::contract(format!(
                    "field_slices[{}] contains an invalid slice",
                    repr_str(name)
                )));
            }
            pieces.extend((start..stop).map(|i| i64::try_from(i).unwrap_or(i64::MAX)));
        }
        if pieces.is_empty() {
            return Err(CaeError::contract(format!("field_slices[{}] names no unknowns", repr_str(name))));
        }
        out.push((name.clone(), pieces));
    }
    Ok(out)
}



pub fn project_field_members(
    residual_map: &CsrMatrix,
    field_members: &[(String, Vec<i64>)],
    reduced_size: usize,
) -> CaeResult<Vec<(String, Vec<i64>)>> {
    if reduced_size < 1 || residual_map.nrows() != reduced_size {
        return Err(CaeError::contract("residual map row count must equal the reduced frame size"));
    }
    let full = residual_map.ncols();
    let members = checked_members(field_members, full)?;
    let names: Vec<String> = members.iter().map(|(n, _)| n.clone()).collect();
    let mut owner = vec![usize::MAX; full];
    for (k, (_, idx)) in members.iter().enumerate() {
        for &i in idx {
            owner[i] = k;
        }
    }
    if owner.contains(&usize::MAX) {
        return Err(CaeError::contract("full-frame field membership does not cover every unknown"));
    }
    let mut reduced: Vec<Vec<i64>> = vec![Vec::new(); names.len()];
    for i in 0..reduced_size {
        let (cols, vals) = residual_map.row(i);
        let fields: BTreeSet<&str> = cols
            .iter()
            .zip(vals)
            .filter(|(_, v)| **v != 0.0)
            .map(|(c, _)| names[owner[*c]].as_str())
            .collect();
        if fields.len() != 1 {
            let list: Vec<String> = fields.iter().map(|s| (*s).to_string()).collect();
            return Err(CaeError::contract(format!(
                "residual map row {i} mixes fields {}; author reduced-frame membership explicitly",
                py_str_list(&list)
            )));
        }
        let name = fields.iter().next().copied().unwrap_or_default();
        if let Some(k) = names.iter().position(|n| n == name) {
            reduced[k].push(i64::try_from(i).unwrap_or(i64::MAX));
        }
    }
    Ok(names.into_iter().zip(reduced).filter(|(_, rows)| !rows.is_empty()).collect())
}



pub fn criterion_from_policy_payload(
    payload: &Value,
    field_members: &[(String, Vec<i64>)],
    state_size: usize,
) -> CaeResult<PerFieldCriterion> {
    let policy = parse_policy(payload, None, GENERIC_SCHEMA_ID)?;
    PerFieldCriterion::new(policy, field_members, state_size)
}

#[must_use]
pub fn members_as_i64(members: &FieldMembers) -> Vec<(String, Vec<i64>)> {
    to_i64(members)
}

#[must_use]
pub fn repr_name_list(names: &[String]) -> String {
    py_str_list(names)
}

