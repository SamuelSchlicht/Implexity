// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use implexity_core::contracts::{
    CaeProvider, Evaluation, FieldValue, LegacySingleArrayProviderCapabilities, ProviderCapabilities,
    ProviderDescriptor, ProviderProblem, Sensitivity, TOPOLOGY_COORDINATE,
};
use implexity_core::orchestration::{OrchestrationPlan, RegisteredAddIn};
use implexity_core::py_repr::repr_str;
use implexity_core::{CaeError, CaeResult};
use implexity_linalg::CsrMatrix;
use implexity_linalg::dense::DenseMatrix;
use implexity_optim::design::{DesignLayout, NamedArrays};
use implexity_optim::numeric::{float_value, str_list_repr};
use implexity_optim::optimizer::OptimizerLifecycleConfig;
use implexity_optim::provider_ops::LifecycleDeclaration;
use implexity_optim::provider_ops::{
    DesignOp, DesignOperations, DesignSensitivities, DesignSensitivity, design_interface,
};
use implexity_solve::implicit_block::{
    BlockCallbacks, BlockOptions, ImplicitBlockSystem, ImplicitSolveResult,
};
use implexity_solve::matrix::{FnAction, Jacobian, checked_matrix};
use ndarray::ArrayD;
use serde_json::{Map, Value};

use crate::addin::{
    FieldContribution, FieldRegistration, JsonMap, NumericalPortBinding, ResidualContribution,
    ResponseContribution, States, addin_operations,
};
use crate::orchestration_runtime::{ExecutionContext, edge_ports};
use crate::results::json_problem;

pub const COMPOSITE_NAME: &str = "orchestrated_composite";

fn contract(message: impl Into<String>) -> CaeError {
    CaeError::contract(message.into())
}


pub fn registration_wire(
    raw: &Value,
    shape: &[usize],
    require_identity: bool,
) -> CaeResult<Map<String, Value>> {
    let Some(m) = raw.as_object() else {
        return Err(contract("field registration must be a mapping"));
    };
    let schema = m.get("schema").and_then(Value::as_str);
    if schema != Some("implexity-grid-registration/1") {
        return Err(contract("field registration requires a supported explicit schema"));
    }
    let (Some(Value::Array(dims)), Some(Value::Array(origin)), Some(Value::Array(basis))) =
        (m.get("shape"), m.get("origin"), m.get("basis"))
    else {
        return Err(contract("field registration is missing shape, origin, or basis"));
    };
    let dims_ok = dims.len() == 3 && dims.iter().all(|v| v.as_u64().is_some_and(|d| d > 0) && !v.is_f64());
    if !dims_ok {
        return Err(contract("field registration shape must contain three positive integers"));
    }
    let dims: Vec<usize> =
        dims.iter().filter_map(Value::as_u64).map(|d| usize::try_from(d).unwrap_or(0)).collect();
    if dims != shape {
        return Err(contract("explicit field registration shape disagrees with its exact array"));
    }
    let rows: Vec<&Vec<Value>> = basis.iter().filter_map(Value::as_array).collect();
    if origin.len() != 3 || basis.len() != 3 || rows.len() != 3 || rows.iter().any(|r| r.len() != 3) {
        return Err(contract("field registration origin/basis must be three-dimensional"));
    }
    let num = |v: &Value| -> CaeResult<f64> {
        match v {
            Value::Number(n) => {
                n.as_f64().ok_or_else(|| contract("field registration origin/basis must be finite"))
            }
            Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            _ => Err(contract("field registration origin/basis must be finite")),
        }
    };
    let origin_f: Vec<f64> = origin.iter().map(num).collect::<CaeResult<_>>()?;
    let basis_f: Vec<Vec<f64>> =
        rows.iter().map(|r| r.iter().map(num).collect::<CaeResult<Vec<f64>>>()).collect::<CaeResult<_>>()?;
    if origin_f.iter().chain(basis_f.iter().flatten()).any(|v| !v.is_finite()) {
        return Err(contract("field registration origin/basis must be finite"));
    }
    let b = &basis_f;
    let determinant = b[0][0] * (b[1][1] * b[2][2] - b[1][2] * b[2][1])
        - b[0][1] * (b[1][0] * b[2][2] - b[1][2] * b[2][0])
        + b[0][2] * (b[1][0] * b[2][1] - b[1][1] * b[2][0]);
    if !determinant.is_finite() || determinant.abs() < 1e-15 {
        return Err(contract("field registration basis must be invertible"));
    }
    let text = |k: &str| m.get(k).map_or_else(String::new, crate::pyval::py_str);
    let centering = text("centering");
    let axis_order = text("axis_order");
    let frame = text("frame");
    if centering != "cell" && centering != "node" {
        return Err(contract("field registration centering must be cell or node"));
    }
    let mut sorted: Vec<char> = axis_order.chars().collect();
    sorted.sort_unstable();
    if sorted != ['x', 'y', 'z'] {
        return Err(contract("field registration axis_order must permute xyz"));
    }
    if frame.is_empty() {
        return Err(contract("field registration frame is required"));
    }
    let mut wire = Map::new();
    wire.insert("schema".into(), Value::String("implexity-grid-registration/1".into()));
    wire.insert("shape".into(), Value::Array(dims.iter().map(|d| Value::from(*d)).collect()));
    wire.insert("origin".into(), Value::Array(origin_f.iter().map(|v| float_value(*v)).collect()));
    wire.insert(
        "basis".into(),
        Value::Array(
            basis_f.iter().map(|r| Value::Array(r.iter().map(|v| float_value(*v)).collect())).collect(),
        ),
    );
    wire.insert("centering".into(), Value::String(centering));
    wire.insert("axis_order".into(), Value::String(axis_order));
    wire.insert("frame".into(), Value::String(frame));
    let identity = crate::canonical::canonical_sha256(&Value::Object(wire.clone()));
    let supplied = m.get("registration_id");
    if require_identity && supplied.and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err(contract("authoritative field registration_id is required"));
    }
    if let Some(s) = supplied
        && !s.is_null()
        && s.as_str() != Some(identity.as_str())
    {
        return Err(contract("explicit field registration has stale identity"));
    }
    wire.insert("registration_id".into(), Value::String(identity));
    Ok(wire)
}

struct ResidualRow {
    contribution: ResidualContribution,
    compatibility: bool,
    owner: String,
}

struct ResponseRow {
    contribution: ResponseContribution,
    compatibility: bool,
}

struct FieldRow {
    contribution: FieldContribution,
    compatibility: bool,
    owner: String,
}

struct Blocks {
    residuals: Vec<ResidualRow>,
    slices: Vec<(String, usize, usize)>,
    scales: BTreeMap<String, Vec<f64>>,
    size: usize,
}

impl Blocks {
    fn split(&self, u: &[f64]) -> States {
        self.slices.iter().map(|(n, a, b)| (n.clone(), u[*a..*b].to_vec())).collect()
    }

    fn slice(&self, name: &str) -> Option<(usize, usize)> {
        self.slices.iter().find(|(n, _, _)| n == name).map(|(_, a, b)| (*a, *b))
    }

    fn argument(contribution_named: bool, values: &NamedArrays) -> NamedArrays {
        if contribution_named {
            values.clone()
        } else {
            NamedArrays::single(
                TOPOLOGY_COORDINATE,
                values.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default(),
            )
        }
    }

    fn state_keys(
        &self,
        dependencies: Option<&[String]>,
        named: bool,
        blocks: &[String],
        label: &str,
    ) -> CaeResult<()> {
        if !named {
            return Ok(());
        }
        let names: Vec<String> = match dependencies {
            None => self.slices.iter().map(|(n, _, _)| n.clone()).collect(),
            Some(d) => d.to_vec(),
        };
        let unique: BTreeSet<&String> = names.iter().collect();
        if unique.len() != names.len() || names.iter().any(|n| self.slice(n).is_none()) {
            return Err(contract(format!("{label}: invalid declared state dependencies")));
        }
        let have: BTreeSet<&String> = blocks.iter().collect();
        if have != unique {
            let missing: Vec<String> = unique.difference(&have).map(|s| (*s).clone()).collect();
            let extra: Vec<String> = have.difference(&unique).map(|s| (*s).clone()).collect();
            return Err(contract(format!(
                "{label}: state derivatives do not match declared dependencies; missing={}, extra={}",
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        Ok(())
    }
}

type DesignRow = (String, (usize, usize), Vec<f64>, Vec<Jacobian>);

struct Callbacks {
    blocks: Arc<Blocks>,
    layout: DesignLayout,
}

impl BlockCallbacks<JsonMap> for Callbacks {
    fn residual(&self, u: &[f64], design: &[f64], context: &JsonMap) -> CaeResult<Vec<f64>> {
        let states = self.blocks.split(u);
        let values = self.layout.unpack(design)?;
        let mut out = Vec::with_capacity(u.len());
        for row in &self.blocks.residuals {
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, &values);
            let r = c.callbacks.residual(&states, &arg, context)?;
            if r.len() != c.state.size || r.iter().any(|v| !v.is_finite()) {
                return Err(contract(format!(
                    "residual {}: wrong shape/nonfinite values",
                    repr_str(&c.state.name)
                )));
            }
            let scale = &self.blocks.scales[&c.state.name];
            out.extend(r.iter().zip(scale).map(|(v, s)| v / s));
        }
        Ok(out)
    }

    fn state_jacobian(&self, u: &[f64], design: &[f64], context: &JsonMap) -> CaeResult<Jacobian> {
        let states = self.blocks.split(u);
        let values = self.layout.unpack(design)?;
        let (mut rows, mut cols, mut vals) = (Vec::new(), Vec::new(), Vec::new());
        let mut offset = 0usize;
        for row in &self.blocks.residuals {
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, &values);
            let mut blocks = c.callbacks.state_jacobians(&states, &arg, context)?;
            let names: Vec<String> = blocks.keys().cloned().collect();
            self.blocks.state_keys(
                c.state_dependencies.as_deref(),
                c.named,
                &names,
                &format!("residual {}", c.state.name),
            )?;
            let unknown: Vec<String> =
                names.iter().filter(|n| self.blocks.slice(n).is_none()).cloned().collect();
            if !unknown.is_empty() {
                let mut sorted = unknown;
                sorted.sort();
                return Err(contract(format!(
                    "residual {}: unknown state Jacobian blocks {}",
                    repr_str(&c.state.name),
                    str_list_repr(&sorted)
                )));
            }
            let declared: Vec<String> = match (&c.state_dependencies, c.named) {
                (Some(d), true) => d.clone(),
                _ => self.blocks.slices.iter().map(|(n, _, _)| n.clone()).collect(),
            };
            let scale = &self.blocks.scales[&c.state.name];
            for (name, a, b) in &self.blocks.slices {
                let shape = (c.state.size, b - a);
                let Some(raw) = blocks.remove(name) else {
                    if !row.compatibility && declared.contains(name) {
                        return Err(contract(format!(
                            "residual {}: missing explicit state Jacobian block {}",
                            repr_str(&c.state.name),
                            repr_str(name)
                        )));
                    }
                    continue;
                };
                let matrix =
                    checked_matrix(raw, shape, &format!("state Jacobian {}->{name}", c.state.name), false)?;
                let csr = matrix.to_csr()?;
                for (i, s) in scale.iter().enumerate().take(csr.nrows()) {
                    let (idx, data) = csr.row(i);
                    for (j, v) in idx.iter().zip(data) {
                        rows.push(offset + i);
                        cols.push(a + j);
                        vals.push(v / s);
                    }
                }
            }
            offset += c.state.size;
        }
        let n = self.blocks.size;
        let m = CsrMatrix::from_triplets(n, n, &rows, &cols, &vals).map_err(|e| contract(e.to_string()))?;
        Ok(Jacobian::Csc(m.to_csc()))
    }

    fn design_jacobian(&self, u: &[f64], design: &[f64], context: &JsonMap) -> CaeResult<Jacobian> {
        let states = self.blocks.split(u);
        let values = self.layout.unpack(design)?;
        let mut rows: Vec<DesignRow> = Vec::new();
        let slices = self.layout.slices();
        for row in &self.blocks.residuals {
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, &values);
            let raw = c.callbacks.design_jacobians(&states, &arg, context)?;
            let label = format!("residual derivative {}", c.state.name);
            let keys: Vec<String> = raw.keys().cloned().collect();
            let expected = self.layout.slices().into_iter().map(|(n, _, _)| n).collect::<Vec<_>>();
            let (missing, extra): (Vec<String>, Vec<String>) = (
                expected.iter().filter(|n| !keys.contains(n)).cloned().collect(),
                keys.iter().filter(|n| !expected.contains(n)).cloned().collect(),
            );
            if !missing.is_empty() || !extra.is_empty() {
                return Err(contract(format!(
                    "{label}: coordinate mismatch; missing={}, extra={}",
                    str_list_repr(&missing),
                    str_list_repr(&extra)
                )));
            }
            let mut raw = raw;
            let mut checked = Vec::new();
            for (name, a, b) in &slices {
                let block = raw.remove(name).ok_or_else(|| contract(format!("{label}: missing {name}")))?;
                checked.push(checked_matrix(block, (c.state.size, b - a), &format!("{label}/{name}"), true)?);
            }
            let (s0, s1) = self.blocks.slice(&c.state.name).unwrap_or((0, 0));
            rows.push((c.state.name.clone(), (s0, s1), self.blocks.scales[&c.state.name].clone(), checked));
        }
        let rows = Arc::new(rows);
        let slices = Arc::new(slices);
        let (n, d) = (u.len(), self.layout.size());
        let (fr, fs) = (Arc::clone(&rows), Arc::clone(&slices));
        let forward = move |v: &[f64]| -> CaeResult<Vec<f64>> {
            let mut out = vec![0.0; n];
            for (_, (a, b), scale, blocks) in fr.iter() {
                for ((_, s0, s1), block) in fs.iter().zip(blocks) {
                    let part = block.apply(&v[*s0..*s1], false)?;
                    for (o, p) in out[*a..*b].iter_mut().zip(part) {
                        *o += p;
                    }
                }
                for (o, s) in out[*a..*b].iter_mut().zip(scale) {
                    *o /= s;
                }
            }
            Ok(out)
        };
        let (tr, ts) = (Arc::clone(&rows), Arc::clone(&slices));
        let transpose = move |w: &[f64]| -> CaeResult<Vec<f64>> {
            let mut out = vec![0.0; d];
            for (_, (a, b), scale, blocks) in tr.iter() {
                let row: Vec<f64> = w[*a..*b].iter().zip(scale).map(|(x, s)| x / s).collect();
                for ((_, s0, s1), block) in ts.iter().zip(blocks) {
                    let part = block.apply(&row, true)?;
                    for (o, p) in out[*s0..*s1].iter_mut().zip(part) {
                        *o += p;
                    }
                }
            }
            Ok(out)
        };
        Ok(Jacobian::Operator(Arc::new(FnAction::new((n, d), forward, transpose))))
    }
}

pub struct CompositeOrchestratedProvider {
    plan: Arc<OrchestrationPlan>,
    entries: BTreeMap<String, Arc<RegisteredAddIn>>,
    context: ExecutionContext,
    strict: bool,
    blocks: Arc<Blocks>,
    responses: Vec<ResponseRow>,
    fields: Vec<FieldRow>,
    design_coordinates: Vec<String>,
    initial: Vec<f64>,
    numerical_port_bindings: Vec<NumericalPortBinding>,
}

impl std::fmt::Debug for CompositeOrchestratedProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeOrchestratedProvider")
            .field("design_coordinates", &self.design_coordinates)
            .field("strict", &self.strict)
            .finish_non_exhaustive()
    }
}

impl CompositeOrchestratedProvider {

    #[allow(clippy::too_many_lines)]
    pub fn new(
        plan: Arc<OrchestrationPlan>,
        entries: BTreeMap<String, Arc<RegisteredAddIn>>,
        context: ExecutionContext,
    ) -> CaeResult<Self> {
        let strict = !plan
            .selected_addins
            .iter()
            .any(|a| entries.get(a).is_some_and(|e| e.compatibility_mode || e.contract.compatibility_mode));
        let mut residuals: Vec<ResidualRow> = Vec::new();
        let mut responses: Vec<ResponseRow> = Vec::new();
        let mut fields: Vec<FieldRow> = Vec::new();
        let mut response_owners: BTreeMap<String, String> = BTreeMap::new();
        let mut field_owners: BTreeMap<String, String> = BTreeMap::new();
        for aid in &plan.selected_addins {
            let entry = &entries[aid];
            let compatibility = entry.compatibility_mode || entry.contract.compatibility_mode;
            let ops = entry.adapter.as_deref().and_then(addin_operations);
            let mut contributed = false;
            if let Some(ops) = ops {
                if let Some(rows) = ops.residual_contributions(&context.values) {
                    let rows = rows?;
                    contributed |= !rows.is_empty();
                    residuals.extend(rows.into_iter().map(|c| ResidualRow {
                        contribution: c,
                        compatibility,
                        owner: aid.clone(),
                    }));
                }
                if let Some(rows) = ops.response_contributions(&context.values) {
                    let rows = rows?;
                    contributed |= !rows.is_empty();
                    for r in rows {
                        if response_owners.contains_key(&r.name) {
                            return Err(contract(format!(
                                "duplicate response_contributions name {}",
                                repr_str(&r.name)
                            )));
                        }
                        response_owners.insert(r.name.clone(), aid.clone());
                        responses.push(ResponseRow { contribution: r, compatibility });
                    }
                }
                if let Some(rows) = ops.field_contributions(&context.values) {
                    let rows = rows?;
                    contributed |= !rows.is_empty();
                    for f in rows {
                        if field_owners.contains_key(&f.name) {
                            return Err(contract(format!(
                                "duplicate field_contributions name {}",
                                repr_str(&f.name)
                            )));
                        }
                        field_owners.insert(f.name.clone(), aid.clone());
                        fields.push(FieldRow { contribution: f, compatibility, owner: aid.clone() });
                    }
                }
            }
            if !contributed {
                return Err(contract(format!(
                    "selected add-in {} supplies no native residual/response/field contribution; it cannot be silently omitted",
                    repr_str(aid)
                )));
            }
        }
        if residuals.is_empty() {
            return Err(contract("selected composite add-ins provide no residual contributions"));
        }
        for (response, aid) in &plan.response_providers {
            let aid = crate::pyval::py_str(aid);
            if response_owners.get(response) != Some(&aid) {
                return Err(contract(format!(
                    "composite runtime lacks response contribution {} from its selected owner {}",
                    repr_str(response),
                    repr_str(&aid)
                )));
            }
        }
        for r in &residuals {
            if !r.contribution.named && !r.compatibility {
                return Err(contract(format!(
                    "state {}: legacy residual contribution is forbidden on the canonical path; use NamedResidualContribution with explicit dependency coverage",
                    repr_str(&r.contribution.state.name)
                )));
            }
        }
        for r in &responses {
            if !r.contribution.named && !r.compatibility {
                return Err(contract(format!(
                    "response {}: legacy response contribution is forbidden on the canonical path; use NamedResponseContribution with explicit dependency coverage",
                    repr_str(&r.contribution.name)
                )));
            }
        }
        let coords = |named: bool, declared: &[String]| -> Vec<String> {
            if named { declared.to_vec() } else { vec![TOPOLOGY_COORDINATE.to_string()] }
        };
        let mut all_coords: Vec<Vec<String>> = Vec::new();
        for r in &residuals {
            all_coords.push(coords(r.contribution.named, &r.contribution.design_coordinates));
        }
        for r in &responses {
            all_coords.push(coords(r.contribution.named, &r.contribution.design_coordinates));
        }
        for names in &all_coords {
            let unique: BTreeSet<&String> = names.iter().collect();
            if names.is_empty() || names.iter().any(String::is_empty) || unique.len() != names.len() {
                return Err(contract("contribution coordinate declarations must be nonempty unique ids"));
            }
        }
        let design_coordinates = if strict {
            let planned = plan.active_design_coordinates.clone();
            if planned.is_empty() {
                return Err(contract("strict residual graph requires active design coordinates"));
            }
            let set: BTreeSet<&String> = planned.iter().collect();
            if all_coords.iter().any(|n| n.iter().collect::<BTreeSet<_>>() != set) {
                return Err(contract(format!(
                    "strict residual/response contributions must explicitly cover every active design coordinate {}",
                    str_list_repr(&planned)
                )));
            }
            planned
        } else {
            let mut common: BTreeSet<String> = all_coords[0].iter().cloned().collect();
            for names in &all_coords[1..] {
                common.retain(|c| names.contains(c));
            }
            if !common.contains(TOPOLOGY_COORDINATE) {
                return Err(contract(format!(
                    "legacy residual contributions do not share {}",
                    repr_str(TOPOLOGY_COORDINATE)
                )));
            }
            let mut out = vec![TOPOLOGY_COORDINATE.to_string()];
            out.extend(common.into_iter().filter(|c| c != TOPOLOGY_COORDINATE));
            out
        };
        let mut slices = Vec::new();
        let mut scales = BTreeMap::new();
        let mut initial = Vec::new();
        let mut offset = 0usize;
        for r in &residuals {
            let state = &r.contribution.state;
            let n = state.size;
            if state.name.is_empty()
                || slices.iter().any(|(s, _, _): &(String, usize, usize)| *s == state.name)
                || n < 1
                || state.initial.len() != n
            {
                return Err(contract(format!(
                    "state block {} has invalid size/name/initial state",
                    repr_str(&state.name)
                )));
            }
            let scale = if state.residual_scale.len() == 1 {
                vec![state.residual_scale[0]; n]
            } else {
                state.residual_scale.clone()
            };
            if scale.len() != n || scale.iter().any(|s| !s.is_finite() || *s <= 0.0) {
                return Err(contract(format!(
                    "state block {}: residual scales must be finite and positive",
                    repr_str(&state.name)
                )));
            }
            scales.insert(state.name.clone(), scale);
            slices.push((state.name.clone(), offset, offset + n));
            offset += n;
            initial.extend_from_slice(&state.initial);
        }
        if initial.iter().any(|v| !v.is_finite()) {
            return Err(contract("residual initial state contains nonfinite entries"));
        }
        for f in &fields {
            let c = &f.contribution;
            if c.name.is_empty() || c.units.is_empty() || c.domain.is_empty() {
                return Err(contract("solver field requires explicit name, units and domain"));
            }
            if !["node", "cell", "face", "global", "quadrature"].contains(&c.association.as_str()) {
                return Err(contract(format!("field {}: invalid mesh association", repr_str(&c.name))));
            }
            if matches!(c.association.as_str(), "node" | "cell") && !f.compatibility {
                let shared = context
                    .get("field_registrations")
                    .and_then(Value::as_object)
                    .and_then(|m| m.get(&c.name))
                    .is_some_and(|v| !v.is_null());
                let single = context.get("field_registration").is_some_and(|v| !v.is_null());
                if c.registration.is_none() && !shared && !single {
                    return Err(contract(format!(
                        "field {}: canonical node/cell fields require authoritative registration",
                        repr_str(&c.name)
                    )));
                }
            }
        }
        let blocks = Arc::new(Blocks { residuals, slices, scales, size: offset });
        let mut out = Self {
            plan,
            entries,
            context,
            strict,
            blocks,
            responses,
            fields,
            design_coordinates,
            initial,
            numerical_port_bindings: Vec::new(),
        };
        out.numerical_port_bindings = out.validate_numerical_bindings()?;
        Ok(out)
    }

    #[must_use]
    pub fn numerical_port_bindings(&self) -> &[NumericalPortBinding] {
        &self.numerical_port_bindings
    }

    fn validate_numerical_bindings(&self) -> CaeResult<Vec<NumericalPortBinding>> {
        let mut declarations: Vec<NumericalPortBinding> = Vec::new();
        for aid in &self.plan.selected_addins {
            let entry = &self.entries[aid];
            if let Some(rows) = entry
                .adapter
                .as_deref()
                .and_then(addin_operations)
                .and_then(|o| o.numerical_port_bindings(&self.context.values))
            {
                declarations.extend(rows?);
            }
        }
        let mut used = vec![false; declarations.len()];
        for edge in &self.plan.coupling_edges {
            let source_entry = &self.entries[&edge.source];
            let target_entry = &self.entries[&edge.target];
            if source_entry.compatibility_mode && target_entry.compatibility_mode {
                continue;
            }
            let label = format!("residual edge {}->{}:{}", edge.source, edge.target, edge.quantity);
            let (source_port, target_port) = edge_ports(edge, source_entry, target_entry)
                .map_err(|_| contract(format!("{label} is not bound to unique typed ports")))?;
            let key = (&edge.source, &edge.target, &source_port.port_id, &target_port.port_id);
            let matches: Vec<usize> = declarations
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    (&r.source_addin, &r.target_addin, &r.source_port_id, &r.target_port_id) == key
                })
                .map(|(i, _)| i)
                .collect();
            if matches.len() != 1 {
                return Err(contract(format!(
                    "{label} requires exactly one NumericalPortBinding, found {}",
                    matches.len()
                )));
            }
            let row = &declarations[matches[0]];
            used[matches[0]] = true;
            let deps = &row.state_dependencies;
            let unique: BTreeSet<&String> = deps.iter().collect();
            if deps.is_empty()
                || unique.len() != deps.len()
                || deps.iter().any(|d| self.blocks.slice(d).is_none())
            {
                return Err(contract(format!("{label} has invalid state dependencies")));
            }
            let source_states: BTreeSet<&String> = self
                .blocks
                .residuals
                .iter()
                .filter(|r| r.owner == edge.source)
                .map(|r| &r.contribution.state.name)
                .collect();
            let target_rows: Vec<&ResidualRow> =
                self.blocks.residuals.iter().filter(|r| r.owner == edge.target).collect();
            let mut target_dependencies: BTreeSet<String> = BTreeSet::new();
            for r in &target_rows {
                match (&r.contribution.state_dependencies, r.contribution.named) {
                    (Some(d), true) => target_dependencies.extend(d.iter().cloned()),
                    _ => target_dependencies.extend(self.blocks.slices.iter().map(|(n, _, _)| n.clone())),
                }
            }
            if !source_states.is_empty() && !deps.iter().any(|d| source_states.contains(d)) {
                return Err(contract(format!(
                    "{label} numerical binding does not name any state owned by its source add-in"
                )));
            }
            if !target_rows.is_empty() && !deps.iter().all(|d| target_dependencies.contains(d)) {
                return Err(contract(format!(
                    "{label} numerical binding is not covered by the target residual's declared state dependencies"
                )));
            }
        }
        if used.iter().any(|u| !u) {
            return Err(contract(
                "numerical port bindings contain declarations for no selected coupling edge",
            ));
        }
        Ok(declarations)
    }

    fn layout(&self, design: &NamedArrays) -> CaeResult<DesignLayout> {
        let layout = DesignLayout::from_values(design)?;
        let names = design.names();
        let missing: Vec<String> =
            self.design_coordinates.iter().filter(|n| !names.contains(n)).cloned().collect();
        let extra: Vec<String> =
            names.iter().filter(|n| !self.design_coordinates.contains(n)).cloned().collect();
        if !missing.is_empty() || !extra.is_empty() {
            return Err(contract(format!(
                "residual graph requires exact named design keys; missing={}, extra={}",
                str_list_repr(&missing),
                str_list_repr(&extra)
            )));
        }
        Ok(layout)
    }

    fn system(&self, layout: &DesignLayout) -> CaeResult<ImplicitBlockSystem<JsonMap>> {
        let ctx = &self.context.values;
        let tolerance = ctx.get("residual_tolerance").map_or(Ok(1e-10), implexity_optim::pyval::py_float)?;
        let max_iterations =
            ctx.get("max_newton_iterations").map_or(Ok(40), implexity_optim::pyval::py_int)?;
        let condition_limit =
            ctx.get("condition_limit").map_or(Ok(1e12), implexity_optim::pyval::py_float)?;
        let criterion = match ctx.get("coupled_convergence_policy") {
            None | Some(Value::Null) => None,
            Some(payload) => {
                let members: Vec<(String, Vec<i64>)> = self
                    .blocks
                    .slices
                    .iter()
                    .map(|(n, a, b)| {
                        (n.clone(), (*a..*b).map(|i| i64::try_from(i).unwrap_or(i64::MAX)).collect())
                    })
                    .collect();
                let c = implexity_solve::convergence::criterion_from_policy_payload(
                    payload,
                    &members,
                    self.initial.len(),
                )?;
                Some(Arc::new(c) as implexity_solve::convergence::Criterion)
            }
        };
        let options = BlockOptions::<JsonMap> {
            tolerance,
            max_iterations: usize::try_from(max_iterations)
                .map_err(|_| contract("max_newton_iterations must be nonnegative"))?,
            condition_limit,
            criterion,
            ..BlockOptions::default()
        };
        ImplicitBlockSystem::new(
            Arc::new(Callbacks { blocks: Arc::clone(&self.blocks), layout: layout.clone() }),
            options,
        )
    }

    fn prepare(&self, design: &NamedArrays) -> CaeResult<Prepared> {
        let layout = self.layout(design)?;
        let flat = layout.pack(design, "design")?;
        let system = self.system(&layout)?;
        let solution = system.solve(&flat, &self.initial, &self.context.values)?;
        let states = self.blocks.split(&solution.state);
        let values = layout.unpack(&flat)?;
        Ok(Prepared { layout, flat, system, solution, states, values })
    }

    fn diagnostics(
        &self,
        solution: &ImplicitSolveResult,
        states: &States,
        design: &NamedArrays,
    ) -> CaeResult<JsonMap> {
        let ctx = &self.context.values;
        let mut raw_norms = Map::new();
        for row in &self.blocks.residuals {
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, design);
            let raw = c.callbacks.residual(states, &arg, ctx)?;
            let l2 = raw.iter().map(|v| v * v).sum::<f64>().sqrt();
            let mut m = Map::new();
            m.insert("l2".into(), float_value(l2));
            m.insert("units".into(), Value::String(c.state.residual_units.clone()));
            raw_norms.insert(c.state.name.clone(), Value::Object(m));
        }
        let mut registrations = Map::new();
        if let Some(Value::Object(rows)) = ctx.get("design_field_registrations") {
            for (name, raw) in rows {
                let Some(array) = design.get(name).filter(|a| a.ndim() == 3) else {
                    return Err(contract(format!(
                        "{name}: only declared three-dimensional coordinates can have a grid registration"
                    )));
                };
                registrations
                    .insert(name.clone(), Value::Object(registration_wire(raw, array.shape(), true)?));
            }
        }
        let mut out = Map::new();
        out.insert("design_field_registrations".into(), Value::Object(registrations));
        out.insert("residual_norm".into(), float_value(solution.residual_norm));
        out.insert("residual_norm_is_scaled".into(), Value::Bool(true));
        out.insert("unscaled_residual_norms".into(), Value::Object(raw_norms));
        out.insert("iterations".into(), Value::from(solution.iterations));
        out.insert("condition_number".into(), float_value(solution.condition_number));
        out.insert("state_solves".into(), Value::from(1));
        out.insert("condition_number_kind".into(), Value::String("1norm_estimate".into()));
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        if ctx.get("coupled_convergence_policy").is_some_and(|v| !v.is_null())
            && let Some(report) = &solution.convergence
        {
            out.insert("coupled_convergence".into(), report.as_value());
            out.insert("coupled_convergence_frame".into(), Value::String("scaled_residual".into()));
        }
        Ok(out)
    }

    fn preflight_report(&self) -> JsonMap {
        let mut out = Map::new();
        out.insert("ok".into(), Value::Bool(true));
        out.insert("issues".into(), Value::Array(Vec::new()));
        out.insert(
            "design_coordinates".into(),
            Value::Array(self.design_coordinates.iter().cloned().map(Value::String).collect()),
        );
        out.insert("orchestrationPlan".into(), self.plan.as_dict());
        out
    }


    pub fn evaluate_named(&self, design: &NamedArrays) -> CaeResult<Evaluation> {
        let p = self.prepare(design)?;
        let ctx = &self.context.values;
        let mut responses = BTreeMap::new();
        for row in &self.responses {
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, &p.values);
            let value = c.callbacks.value(&p.states, &arg, ctx)?;
            if !value.is_finite() {
                return Err(contract(format!("response {} returned nonfinite value", repr_str(&c.name))));
            }
            responses.insert(c.name.clone(), value);
        }
        let mut fields = BTreeMap::new();
        let mut metadata = Map::new();
        for row in &self.fields {
            let f = &row.contribution;
            let arr = f.value.value(&p.states, &p.values, ctx)?;
            if arr.is_empty() || arr.iter().any(|v| !v.is_finite()) {
                return Err(contract(format!("solver field {} is empty/nonfinite", repr_str(&f.name))));
            }
            if !f.components.is_empty()
                && (arr.ndim() == 0 || arr.shape()[arr.ndim() - 1] != f.components.len())
            {
                return Err(contract(format!(
                    "solver field {}: component count mismatch",
                    repr_str(&f.name)
                )));
            }
            let mut meta = Map::new();
            meta.insert("units".into(), Value::String(f.units.clone()));
            meta.insert("association".into(), Value::String(f.association.clone()));
            meta.insert("domain".into(), Value::String(f.domain.clone()));
            meta.insert(
                "components".into(),
                Value::Array(f.components.iter().cloned().map(Value::String).collect()),
            );
            meta.insert("shape".into(), Value::Array(arr.shape().iter().map(|d| Value::from(*d)).collect()));
            meta.insert("source_addin".into(), Value::String(row.owner.clone()));
            meta.insert("origin".into(), Value::String("converged_shared_state".into()));
            let raw_reg: Option<Value> = match &f.registration {
                Some(FieldRegistration::Fixed(v)) => Some(v.clone()),
                Some(FieldRegistration::Computed(cb)) => Some(cb(&p.states, &p.values, ctx)?),
                None => ctx
                    .get("field_registrations")
                    .and_then(Value::as_object)
                    .and_then(|m| m.get(&f.name))
                    .cloned()
                    .or_else(|| ctx.get("field_registration").cloned())
                    .filter(|v| !v.is_null()),
            };
            if let Some(raw_reg) = raw_reg {
                let shape: Vec<usize> = arr.shape().iter().take(3).copied().collect();
                let reg = registration_wire(&raw_reg, &shape, !row.compatibility)?;
                if !matches!(f.association.as_str(), "cell" | "node")
                    || reg.get("centering").and_then(Value::as_str) != Some(f.association.as_str())
                {
                    return Err(contract("field registration centering disagrees with its mesh association"));
                }
                meta.insert("registration".into(), Value::Object(reg));
            } else if matches!(f.association.as_str(), "node" | "cell") {
                meta.insert(
                    "registration_status".into(),
                    Value::String("legacy_missing_non_authoritative".into()),
                );
            }
            fields.insert(f.name.clone(), FieldValue::Array(arr));
            metadata.insert(f.name.clone(), Value::Object(meta));
        }
        let mut diagnostics = self.diagnostics(&p.solution, &p.states, &p.values)?;
        diagnostics.insert("field_metadata".into(), Value::Object(metadata));
        Ok(Evaluation { provider: COMPOSITE_NAME.into(), responses, diagnostics, fields })
    }


    #[allow(clippy::too_many_lines)]
    pub fn sensitivities_named(
        &self,
        design: &NamedArrays,
        names: &[String],
    ) -> CaeResult<DesignSensitivities> {
        let unique: BTreeSet<&String> = names.iter().collect();
        if names.is_empty()
            || unique.len() != names.len()
            || names.iter().any(|n| !self.responses.iter().any(|r| &r.contribution.name == n))
        {
            return Err(contract("batched sensitivities require unique known response names"));
        }
        let p = self.prepare(design)?;
        let ctx = &self.context.values;
        let (n, m, d) = (p.solution.state.len(), names.len(), p.layout.size());
        let mut gu = DenseMatrix::zeros(n, m);
        let mut direct = DenseMatrix::zeros(d, m);
        let mut values = BTreeMap::new();
        for (j, name) in names.iter().enumerate() {
            let row = self
                .responses
                .iter()
                .find(|r| &r.contribution.name == name)
                .ok_or_else(|| contract("unknown response"))?;
            let c = &row.contribution;
            let arg = Blocks::argument(c.named, &p.values);
            let value = c.callbacks.value(&p.states, &arg, ctx)?;
            if !value.is_finite() {
                return Err(contract(format!("response {} returned nonfinite value", repr_str(name))));
            }
            values.insert(name.clone(), value);
            let sg = c.callbacks.state_gradients(&p.states, &arg, ctx)?;
            let keys: Vec<String> = sg.keys().cloned().collect();
            self.blocks.state_keys(
                c.state_dependencies.as_deref(),
                c.named,
                &keys,
                &format!("response {name}"),
            )?;
            for (state_name, g) in &sg {
                let Some((a, b)) = self.blocks.slice(state_name) else {
                    return Err(contract(format!(
                        "response {}: unknown state {}",
                        repr_str(name),
                        repr_str(state_name)
                    )));
                };
                if g.len() != b - a || g.iter().any(|v| !v.is_finite()) {
                    return Err(contract(format!(
                        "response {}: invalid gradient for state {}",
                        repr_str(name),
                        repr_str(state_name)
                    )));
                }
                for (i, v) in g.iter().enumerate() {
                    gu.data[(a + i) * m + j] = *v;
                }
            }
            let dg = c.callbacks.design_gradients(&p.states, &arg, ctx)?;
            let dg = if c.named {
                dg
            } else {
                let topo = p.values.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default();
                let raw = dg.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default();
                if raw.len() != topo.len() {
                    return Err(contract("response direct design gradient size mismatch"));
                }
                let reshaped = ArrayD::from_shape_vec(topo.raw_dim(), raw.iter().copied().collect())
                    .map_err(|e| contract(e.to_string()))?;
                NamedArrays::single(TOPOLOGY_COORDINATE, reshaped)
            };
            let packed = p.layout.pack(&dg, &format!("response derivative {name}"))?;
            for (i, v) in packed.iter().enumerate() {
                direct.data[i * m + j] = *v;
            }
        }
        let adj = p.system.adjoint_many(&p.flat, &p.solution.state, &gu, &direct, ctx)?;
        let mut diagnostics = self.diagnostics(&p.solution, &p.states, &p.values)?;
        diagnostics.insert("adjoint_rhs_count".into(), Value::from(names.len()));
        diagnostics.insert("adjoint_factorizations".into(), Value::from(1));
        diagnostics.insert("design_linearizations".into(), Value::from(1));
        diagnostics.insert("factorization".into(), Value::String(adj.factorization.clone()));
        diagnostics.insert(
            "transpose_residual_norms".into(),
            Value::Object(
                names
                    .iter()
                    .zip(&adj.transpose_residual_norms)
                    .map(|(n, v)| (n.clone(), float_value(*v)))
                    .collect(),
            ),
        );
        diagnostics.insert(
            "transpose_relative_residuals".into(),
            Value::Object(
                names
                    .iter()
                    .zip(&adj.transpose_relative_residuals)
                    .map(|(n, v)| (n.clone(), float_value(*v)))
                    .collect(),
            ),
        );
        let mut gradients = BTreeMap::new();
        for (j, name) in names.iter().enumerate() {
            let column: Vec<f64> = (0..d).map(|i| adj.gradients.data[i * m + j]).collect();
            gradients.insert(name.clone(), p.layout.unpack(&column)?);
        }
        Ok(DesignSensitivities { responses: values, gradients, diagnostics })
    }

    fn lifecycle_config(&self) -> CaeResult<OptimizerLifecycleConfig> {
        OptimizerLifecycleConfig::new(
            self.design_coordinates.clone(),
            "sensitivity_design",
            "evaluate_design",
            None,
            None,
            self.strict,
            !self.strict,
        )
    }


    pub fn coupling_report(&self, for_optimization: bool) -> CaeResult<JsonMap> {
        crate::orchestration_coupling::validate_orchestration_couplings(
            COMPOSITE_NAME,
            &self.plan,
            &self.entries,
            &self.context.values,
            for_optimization,
            Some(&crate::orchestration_coupling::RuntimeEvidence::Residual(
                self.numerical_port_bindings
                    .iter()
                    .map(|b| {
                        (
                            b.source_addin.clone(),
                            b.target_addin.clone(),
                            b.source_port_id.clone(),
                            b.target_port_id.clone(),
                        )
                    })
                    .collect(),
            )),
        )
    }
}

struct Prepared {
    layout: DesignLayout,
    flat: Vec<f64>,
    system: ImplicitBlockSystem<JsonMap>,
    solution: ImplicitSolveResult,
    states: States,
    values: NamedArrays,
}

impl CaeProvider for CompositeOrchestratedProvider {
    fn name(&self) -> &str {
        COMPOSITE_NAME
    }
    fn provider_id(&self) -> Option<&str> {
        Some(COMPOSITE_NAME)
    }
    fn implementation(&self) -> &'static str {
        "implexity.cae.residual_runtime.CompositeOrchestratedProvider"
    }
    fn capabilities(&self) -> CaeResult<ProviderCapabilities> {
        let responses: Vec<String> = {
            let mut r: Vec<String> = self.responses.iter().map(|r| r.contribution.name.clone()).collect();
            r.sort();
            r
        };
        let analyses = vec!["intent-composed multiphysics".to_string()];
        let fields: Vec<String> = self.fields.iter().map(|f| f.contribution.name.clone()).collect();
        if self.strict {
            let mut d = ProviderDescriptor::new(COMPOSITE_NAME, analyses, responses);
            d.fields = fields;
            d.sensitivities = true;
            d.design_coordinates.clone_from(&self.design_coordinates);
            d.nonlinear = true;
            Ok(ProviderCapabilities::Descriptor(Box::new(d.checked()?)))
        } else {
            let mut c = LegacySingleArrayProviderCapabilities::new(COMPOSITE_NAME, analyses, responses);
            c.base.fields = fields;
            c.base.sensitivities = true;
            c.base.design_coordinates.clone_from(&self.design_coordinates);
            c.base.nonlinear = true;
            Ok(ProviderCapabilities::Legacy(Box::new(c.checked()?)))
        }
    }
    fn normalise_problem(&self, problem: &Value) -> CaeResult<ProviderProblem> {
        Ok(json_problem(if problem.is_null() { Value::Object(Map::new()) } else { problem.clone() }))
    }
    fn preflight(
        &self,
        _problem: &ProviderProblem,
        topology: Option<&ArrayD<f64>>,
    ) -> CaeResult<Map<String, Value>> {
        if self.strict && topology.is_some() {
            return Err(contract("bare-array residual preflight is compatibility-only"));
        }
        Ok(self.preflight_report())
    }
    fn evaluate(&self, _problem: &ProviderProblem, topology: &ArrayD<f64>) -> CaeResult<Evaluation> {
        if self.strict {
            return Err(contract("bare-array residual evaluation is compatibility-only"));
        }
        self.evaluate_named(&NamedArrays::single(TOPOLOGY_COORDINATE, topology.clone()))
    }
    fn sensitivity(
        &self,
        _problem: &ProviderProblem,
        topology: &ArrayD<f64>,
        response: &str,
    ) -> CaeResult<Sensitivity> {
        if self.strict {
            return Err(contract("bare-array residual sensitivity is compatibility-only"));
        }
        let out =
            self.sensitivity_named(&NamedArrays::single(TOPOLOGY_COORDINATE, topology.clone()), response)?;
        Ok(Sensitivity {
            provider: COMPOSITE_NAME.into(),
            response: response.into(),
            value: out.value,
            gradient: out.gradients.get(TOPOLOGY_COORDINATE).cloned().unwrap_or_default(),
            diagnostics: out.diagnostics,
        })
    }
    fn coupling_validation(
        &self,
        _problem: Option<&ProviderProblem>,
        for_optimization: bool,
    ) -> Option<Result<Value, String>> {
        Some(self.coupling_report(for_optimization).map(Value::Object).map_err(|e| e.to_string()))
    }
    fn interface(&self, name: &str) -> Option<&(dyn std::any::Any + Send + Sync)> {
        design_interface::<Self>(name)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl CompositeOrchestratedProvider {
    fn sensitivity_named(&self, design: &NamedArrays, response: &str) -> CaeResult<DesignSensitivity> {
        let mut out = self.sensitivities_named(design, &[response.to_string()])?;
        let value = out.responses[response];
        let gradients = out.gradients.remove(response).unwrap_or_default();
        let mut diagnostics = out.diagnostics;
        let norm = diagnostics
            .get("transpose_residual_norms")
            .and_then(|m| m.get(response))
            .cloned()
            .unwrap_or(Value::Null);
        diagnostics.insert("transpose_residual_norm".into(), norm);
        Ok(DesignSensitivity { value, gradients, diagnostics })
    }
}

impl DesignOperations for CompositeOrchestratedProvider {
    fn provides(&self, op: DesignOp) -> bool {
        matches!(
            op,
            DesignOp::Evaluate
                | DesignOp::Sensitivity
                | DesignOp::EvaluateDesign
                | DesignOp::PreflightDesign
                | DesignOp::SensitivityDesign
                | DesignOp::SensitivitiesDesign
                | DesignOp::OptimizerLifecycle
        )
    }
    fn evaluate_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        _op: usize,
    ) -> CaeResult<Evaluation> {
        self.evaluate_named(design)
    }
    fn preflight_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
    ) -> CaeResult<Map<String, Value>> {
        self.layout(design)?;
        Ok(self.preflight_report())
    }
    fn sensitivity_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        response: &str,
        _op: usize,
    ) -> CaeResult<DesignSensitivity> {
        self.sensitivity_named(design, response)
    }
    fn sensitivities_design(
        &self,
        _problem: &ProviderProblem,
        design: &NamedArrays,
        responses: &[String],
        _op: usize,
    ) -> CaeResult<DesignSensitivities> {
        self.sensitivities_named(design, responses)
    }
    fn optimizer_lifecycle(&self, _problem: Option<&ProviderProblem>) -> CaeResult<LifecycleDeclaration> {
        self.lifecycle_config().map(LifecycleDeclaration::Typed)
    }
}
