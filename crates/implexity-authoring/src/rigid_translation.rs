// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use implexity_geometry::document::{Model, build};
use implexity_geometry::eval::{EvalOptions, eval_points};
use implexity_geometry::field_registration::GridRegistration;
use implexity_geometry::linalg3::solve;

use crate::entities::normalise_region_selector;
use crate::error::{AResult, AuthoringError};
use crate::field_interaction::{GridGeometry, read_spatial_field};
use crate::py::{Arr, get, jfs, py_str, truthy};
use crate::spatial_selection::SpatialSelection;
use crate::surface_regions::{normalize_surface_patch, surface_patch_id};

fn verr(m: &str) -> AuthoringError {
    AuthoringError::value("ValueError", m)
}

#[derive(Clone, Debug)]
pub struct CellContext {
    pub values: Vec<f64>,
    pub grid: GridGeometry,
    pub new_grid: GridGeometry,
    pub identity: Value,
    pub field_id: String,
    pub payload_sha: String,
}

fn move_cell(row: &Value, ctx: &CellContext, policy: &str) -> AResult<Value> {
    let prior = get(row, "field_identity").cloned().unwrap_or_else(|| json!({}));
    let reg_id = ctx.grid.registration.to_wire()["registration_id"].clone();
    if get(row, "field_id") != Some(&Value::from(ctx.field_id.clone()))
        || get(&prior, "registration_id") != Some(&reg_id)
        || get(&prior, "payload_sha256") != Some(&Value::from(ctx.payload_sha.clone()))
    {
        return Err(verr("Typed cell attachment has stale field identity"));
    }
    if get(row, "protected_runs").is_some_and(truthy) || get(row, "protected_by_reason").is_some_and(truthy) {
        return Err(verr("Protected cell attachments require authoritative protection reconstruction"));
    }
    let selection =
        SpatialSelection::from_mapping(row, &ctx.values, ctx.grid.clone(), &BTreeMap::new(), None)?;
    let mut selected = selection.selected.clone();
    if policy == "world_fixed" {
        let m = ctx.new_grid.registration.matrix();
        let rhs: [f64; 3] =
            std::array::from_fn(|a| ctx.grid.registration.origin[a] - ctx.new_grid.registration.origin[a]);
        let shift = solve(&m, &rhs).ok_or_else(|| crate::py::value_error("Singular matrix"))?;
        let integer: [i64; 3] = shift.map(|v| v.round_ties_even() as i64);
        if (0..3).any(|a| (shift[a] - integer[a] as f64).abs() > 1e-10) {
            return Err(verr(
                "World-fixed cells require exact whole-cell translation; fractional cell overlap is unsupported",
            ));
        }
        let s = ctx.new_grid.shape;
        let mut moved = vec![false; selected.len()];
        for i in 0..s[0] {
            for j in 0..s[1] {
                for k in 0..s[2] {
                    if !selected[ctx.grid.flat(i, j, k)] {
                        continue;
                    }
                    let t = [i as i64 + integer[0], j as i64 + integer[1], k as i64 + integer[2]];
                    if (0..3).any(|a| t[a] < 0 || t[a] >= s[a] as i64) {
                        return Err(verr("World-fixed cell attachment leaves the translated grid"));
                    }
                    moved[ctx.new_grid.flat(t[0] as usize, t[1] as usize, t[2] as usize)] = true;
                }
            }
        }
        selected = moved;
    }
    let result = SpatialSelection::new(
        &ctx.field_id,
        &ctx.values,
        ctx.new_grid.clone(),
        Some(&selection.id),
        selection.revision + 1,
        Some(&selected),
        &BTreeMap::new(),
        Some(&ctx.identity),
        &selection.provenance,
        selection.threshold,
    )?
    .serialise(true);
    let mut preserved = row.as_object().cloned().unwrap_or_default();
    if let Some(r) = result.as_object() {
        for (k, v) in r {
            preserved.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(preserved))
}

struct Translator<'a> {
    delta: [f64; 3],
    policy: &'a str,
    before: &'a Model,
    moved: &'a Model,
    cell: Option<&'a CellContext>,
    ids: BTreeSet<String>,
}

impl Translator<'_> {
    fn patch(&self, raw: &Value) -> AResult<Value> {
        let mut result = raw.as_object().cloned().unwrap_or_default();
        let spec = normalize_surface_patch(raw, None, true)?;
        let a0 = Arr::from_opt(spec.get("anchor_mm"))?.data;
        let mut anchor = [a0[0], a0[1], a0[2]];
        if self.policy == "attached" {
            anchor = std::array::from_fn(|i| anchor[i] + self.delta[i]);
        } else {
            let root = self.moved.root().ok_or_else(|| verr("the translated model has no root"))?;
            let v = eval_points(&root, &[anchor], &EvalOptions::exact())?;
            if !v.iter().all(|x| x.is_finite()) || v.iter().any(|x| x.abs() > 1e-7) {
                return Err(verr("World-fixed surface anchor is no longer on the translated surface"));
            }
        }
        for key in ["anchor_mm", "anchor", "point", "center", "origin"] {
            if let Some(v) = result.get(key) {
                let got = Arr::from_json(v)?;
                let same = got.shape == [3] && got.data.iter().zip(&a0).all(|(x, y)| (x - y).abs() <= 1e-10);
                if !same {
                    return Err(verr("Surface anchor aliases disagree"));
                }
                result.insert(key.into(), jfs(&anchor));
            }
        }
        if !result.contains_key("anchor_mm") {
            result.insert("anchor_mm".into(), jfs(&anchor));
        }
        let identities = [
            ("structure_id", self.before.structure_id(), self.moved.structure_id()),
            ("content_id", self.before.content_id(), self.moved.content_id()),
        ];
        let before_root = self.before.doc.get("root").cloned().unwrap_or(Value::Null);
        let moved_root = self.moved.doc.get("root").cloned().unwrap_or(Value::Null);
        if let Some(Value::Object(tracking)) = result.get_mut("tracking") {
            for (key, old, new) in &identities {
                if let Some(t) = tracking.get(*key) {
                    if Some(py_str(t)) != *old || !t.is_string() {
                        return Err(verr("Surface attachment model identity is stale"));
                    }
                    tracking.insert((*key).into(), new.clone().map_or(Value::Null, Value::from));
                }
            }
            if tracking.get("node_id") == Some(&before_root) {
                tracking.insert("node_id".into(), moved_root.clone());
            }
        }
        for (key, old, new) in &identities {
            if let Some(t) = result.get(*key) {
                if Some(py_str(t)) != *old || !t.is_string() {
                    return Err(verr("Surface attachment identity is stale"));
                }
                result.insert((*key).into(), new.clone().map_or(Value::Null, Value::from));
            }
        }
        if result.get("node_id") == Some(&before_root) {
            result.insert("node_id".into(), moved_root);
        }
        if result.contains_key("definition_id") {
            let id = surface_patch_id(&Value::Object(result.clone()));
            result.insert("definition_id".into(), Value::from(id));
        }
        Ok(Value::Object(result))
    }

    fn selector(&self, raw: &Value) -> AResult<Value> {
        let Some(obj) = raw.as_object() else { return Err(verr("Typed selector must be an object")) };
        let mut result = obj.clone();
        let kind = result
            .get("type")
            .or_else(|| result.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match kind.as_str() {
            "surface_patch" => self.patch(raw),
            "surface_patch_set" => {
                let key = if result.contains_key("patches") { "patches" } else { "samples" };
                let items = result
                    .get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| AuthoringError::Key(format!("'{key}'")))?;
                let moved: Vec<Value> = items.iter().map(|p| self.patch(p)).collect::<AResult<_>>()?;
                result.insert(key.into(), Value::Array(moved));
                Ok(Value::Object(result))
            }
            "cell_selection" => {
                let ctx = self
                    .cell
                    .ok_or_else(|| verr("Cell attachment requires the translated registered sampled root"))?;
                move_cell(raw, ctx, self.policy)
            }
            "ref" => {
                if !result.get("id").and_then(Value::as_str).is_some_and(|i| self.ids.contains(i)) {
                    return Err(verr("Load/BC or region selector refers to an unknown region"));
                }
                Ok(Value::Object(result))
            }
            "everywhere" | "nowhere" => Ok(Value::Object(result)),
            "and" | "or" => {
                let key = if result.contains_key("regions") { "regions" } else { "children" };
                let items = result
                    .get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .ok_or_else(|| AuthoringError::Key(format!("'{key}'")))?;
                let moved: Vec<Value> = items.iter().map(|p| self.selector(p)).collect::<AResult<_>>()?;
                result.insert(key.into(), Value::Array(moved));
                Ok(Value::Object(result))
            }
            "not" => {
                let key = if result.contains_key("region") { "region" } else { "child" };
                let child =
                    result.get(key).cloned().ok_or_else(|| AuthoringError::Key(format!("'{key}'")))?;
                result.insert(key.into(), self.selector(&child)?);
                Ok(Value::Object(result))
            }
            "box" | "slab" if self.policy == "attached" => {
                for key in ["lo_mm", "hi_mm"] {
                    let Some(v) = result.get(key) else { continue };
                    let a = Arr::from_json(v)?;
                    let shifted: Vec<f64> = if kind == "box" {
                        if a.shape != [3] {
                            return Err(crate::py::value_error("operands could not be broadcast together"));
                        }
                        a.data.iter().zip(&self.delta).map(|(x, d)| x + d).collect()
                    } else {
                        let axis = result
                            .get("axis")
                            .and_then(Value::as_str)
                            .and_then(|s| "xyz".find(s))
                            .ok_or_else(|| verr("substring not found"))?;
                        a.data.iter().map(|x| x + self.delta[axis]).collect()
                    };
                    if !shifted.iter().all(|v| v.is_finite()) {
                        return Err(verr("Region translation is nonfinite"));
                    }
                    let out = if a.ndim() == 0 {
                        crate::py::jf(shifted[0])
                    } else {
                        crate::py::nested(&a.shape, &shifted)
                    };
                    result.insert(key.into(), out);
                }
                Ok(Value::Object(result))
            }
            _ => Err(verr(
                "This region requires provider-domain translation or world-fixed membership validation",
            )),
        }
    }
}

fn for_each_item(value: &mut Value, mut f: impl FnMut(&mut Value) -> AResult<()>) -> AResult<()> {
    match value {
        Value::Object(m) => m.values_mut().try_for_each(&mut f),
        Value::Array(a) => a.iter_mut().try_for_each(&mut f),
        _ => Err(verr("Engineering entities must be a list or object")),
    }
}

fn translate_engineering(engineering: &mut Value, t: &mut Translator<'_>) -> AResult<()> {
    let regions_v = engineering.get("regions").cloned().unwrap_or_else(|| json!([]));
    t.ids = match &regions_v {
        Value::Object(m) => {
            m.iter().map(|(k, item)| item.get("id").map_or_else(|| k.clone(), py_str)).collect()
        }
        Value::Array(a) => {
            a.iter().map(|item| item.get("id").map_or_else(|| "None".into(), py_str)).collect()
        }
        _ => return Err(verr("Engineering entities must be a list or object")),
    };
    let mut regions = regions_v;
    for_each_item(&mut regions, |region| {
        let aliases: Vec<&str> = ["selector", "selection", "surface_patch", "surface_patch_set"]
            .into_iter()
            .filter(|k| region.get(*k).is_some())
            .collect();
        if aliases.is_empty() {
            return Err(verr("Region has no supported selector"));
        }
        let canonical: Vec<Value> = aliases
            .iter()
            .map(|k| normalise_region_selector(&region[*k], "translation.region"))
            .collect::<AResult<_>>()?;
        if canonical[1..].iter().any(|c| *c != canonical[0]) {
            return Err(verr("Engineering region selector aliases disagree"));
        }
        for k in aliases {
            let moved = t.selector(&region[k])?;
            if let Some(m) = region.as_object_mut() {
                m.insert(k.into(), moved);
            }
        }
        Ok(())
    })?;
    if let Some(m) = engineering.as_object_mut()
        && m.contains_key("regions")
    {
        m.insert("regions".into(), regions);
    }
    for family in ["loads", "boundary_conditions"] {
        let mut items = engineering.get(family).cloned().unwrap_or_else(|| json!([]));
        for_each_item(&mut items, |item| {
            let has_spec = item.get("spec").is_some();
            let action =
                if has_spec { item.get_mut("spec").ok_or_else(|| verr("missing spec"))? } else { item };
            if ["anchor_mm", "position_mm", "origin", "point", "center", "bounds_mm"]
                .iter()
                .any(|k| action.get(*k).is_some())
            {
                return Err(verr("Load/BC positions must use an explicit typed region or glyph patch"));
            }
            let kind = action.get("kind").and_then(Value::as_str).unwrap_or_default();
            if !["pressure", "traction", "heat_flux", "volumetric_heat", "dirichlet_T", "robin", "clamp"]
                .contains(&kind)
            {
                return Err(verr("Load/BC spatial semantics require explicit support"));
            }
            match action.get("region").cloned() {
                Some(Value::String(r)) => {
                    if !t.ids.contains(&r) {
                        return Err(verr("Load/BC refers to an unknown region"));
                    }
                }
                Some(region @ Value::Object(_)) => {
                    if region.get("type").and_then(Value::as_str) == Some("ref")
                        && !region.get("id").and_then(Value::as_str).is_some_and(|i| t.ids.contains(i))
                    {
                        return Err(verr("Load/BC refers to an unknown region"));
                    }
                    let moved = t.selector(&region)?;
                    if let Some(m) = action.as_object_mut() {
                        m.insert("region".into(), moved);
                    }
                }
                _ => return Err(verr("Load/BC needs an explicit region")),
            }
            if let Some(glyph) = action.get("glyph").cloned() {
                let keys: Vec<&str> = ["patch", "selection", "surface_patch"]
                    .into_iter()
                    .filter(|k| glyph.get(*k).is_some())
                    .collect();
                if keys.is_empty() {
                    return Err(verr("Glyph lacks a supported surface anchor"));
                }
                let mut g = glyph.as_object().cloned().unwrap_or_default();
                for k in keys {
                    g.insert(k.into(), t.patch(&glyph[k])?);
                }
                if let Some(m) = action.as_object_mut() {
                    m.insert("glyph".into(), Value::Object(g));
                }
            }
            Ok(())
        })?;
        if let Some(m) = engineering.as_object_mut()
            && m.contains_key(family)
        {
            m.insert(family.into(), items);
        }
    }
    Ok(())
}

const ANALYTIC_KINDS: [&str; 22] = [
    "sphere",
    "box",
    "rounded_box",
    "cylinder",
    "capsule",
    "cone",
    "torus",
    "plane",
    "constant",
    "translate",
    "rotate",
    "union",
    "intersect",
    "difference",
    "negate",
    "offset",
    "shell",
    "scale.uniform",
    "scale.nonuniform",
    "scale.nonuniform_safe",
    "fillet",
    "chamfer",
];

fn mut_path<'a>(v: &'a mut Value, keys: &[&str]) -> Option<&'a mut Map<String, Value>> {
    let mut cur = v;
    for k in keys {
        cur = cur.get_mut(*k)?;
    }
    cur.as_object_mut()
}


pub fn translate_bundle(
    document: &Value,
    problem: &Value,
    delta_mm: &Value,
    attachment_policy: &str,
    node_id: Option<&str>,
) -> AResult<Value> {
    let d = Arr::from_json(delta_mm)
        .map_err(|_| verr("Translation requires three finite displacements in millimetres"))?;
    let delta = match d.vec3() {
        Some(v) if v.iter().all(|x| x.is_finite()) => v,
        _ => return Err(verr("Translation requires three finite displacements in millimetres")),
    };
    if attachment_policy != "attached" && attachment_policy != "world_fixed" {
        return Err(verr("Unknown translation attachment policy"));
    }
    let mut out = document.clone();
    let mut engineering = problem.clone();
    let model = build(&out, None, None)?;
    let root = out.get("root").map_or_else(|| "None".into(), py_str);
    if let Some(n) = node_id
        && n != root
    {
        return Err(verr(
            "Rigid translation currently supports the entire root part, not a selected subgraph",
        ));
    }
    if truthy(&engineering) {
        let Some(e) = engineering.as_object() else {
            return Err(verr("Only explicit typed version-2 engineering problems are supported"));
        };
        if e.get("schema").and_then(Value::as_str) != Some("implexity-differentiable-problem/2") {
            return Err(verr("Only explicit typed version-2 engineering problems are supported"));
        }
        if ["base_case", "setup", "backend"].iter().any(|k| e.get(*k).is_some_and(truthy)) {
            return Err(verr(
                "Inherited/provider-bound cases require an explicit provider domain-frame transformation",
            ));
        }
        let allowed = [
            "schema",
            "model",
            "name",
            "description",
            "regions",
            "loads",
            "boundary_conditions",
            "analysis",
            "materials",
        ];
        if e.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(verr(
                "Engineering problem contains provider or spatial data requiring explicit frame handling",
            ));
        }
        if e.get("analysis").and_then(|a| a.get("overrides")).is_some_and(truthy) {
            return Err(verr(
                "Analysis overrides may contain spatial references and cannot be translated implicitly",
            ));
        }
    }
    let nodes = out
        .get("nodes")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| AuthoringError::Key("'nodes'".into()))?;
    let mut reachable = BTreeSet::new();
    let mut stack = vec![root.clone()];
    while let Some(nid) = stack.pop() {
        if !reachable.insert(nid.clone()) {
            continue;
        }
        let node =
            nodes.get(&nid).ok_or_else(|| AuthoringError::Key(crate::py::repr(&Value::from(nid.clone()))))?;
        for child in node.get("children").and_then(Value::as_array).cloned().unwrap_or_default() {
            stack.push(child.get("node").map_or_else(|| "None".into(), py_str));
        }
    }
    let mut bindings: Vec<(String, String, Value)> = Vec::new();
    for nid in &reachable {
        if let Some(params) = nodes[nid].get("params").and_then(Value::as_object) {
            for (key, value) in params {
                if let Some(a) = value.as_object().and_then(|m| m.get("array")) {
                    bindings.push((nid.clone(), key.clone(), a.clone()));
                }
            }
        }
    }
    let meta = out.get("meta").and_then(|m| m.get("implexity")).cloned().unwrap_or_else(|| json!({}));
    let selections: Vec<Value> = meta
        .get("interaction")
        .and_then(|i| i.get("spatial_selections"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let sampled = nodes[&root].get("kind").and_then(Value::as_str) == Some("cell_grid_field");
    if !sampled
        && reachable.iter().any(|n| {
            !ANALYTIC_KINDS.contains(&nodes[n].get("kind").and_then(Value::as_str).unwrap_or_default())
        })
    {
        return Err(verr(
            "This graph contains a field-bearing or unqualified operation; its frame needs explicit support",
        ));
    }
    if !bindings.is_empty()
        && !(sampled && bindings.len() == 1 && bindings[0].0 == root && bindings[0].1 == "samples")
    {
        return Err(verr(
            "Mixed, transformed or shared sampled graphs require accumulated registration support",
        ));
    }
    if !sampled
        && (!selections.is_empty()
            || meta.get("spatial_fields").is_some_and(truthy)
            || meta.get("topology").is_some_and(truthy))
    {
        return Err(verr("Non-sampled geometry has registered fields that require explicit frame handling"));
    }
    let mut evidence = json!({"kind": "rigid_translation", "delta_mm": jfs(&delta),
        "attachment_policy": attachment_policy, "node_id": root,
        "scope": if sampled { "sampled_root" } else { "analytic_root" },
        "arrays_preserved": true, "selection_count": selections.len()});
    let unchanged = || json!({"document": document, "problem": problem, "evidence": evidence.clone()});
    let mut cell_context: Option<CellContext> = None;
    if sampled {
        if bindings.is_empty() {
            return Err(verr("Sampled root must bind one inline sample array"));
        }
        let field_id = py_str(&bindings[0].2);
        let shared = nodes.iter().any(|(nid, n)| {
            *nid != root
                && n.get("params").and_then(Value::as_object).is_some_and(|p| {
                    p.values().any(|v| {
                        v.as_object().and_then(|m| m.get("array")).is_some_and(|a| py_str(a) == field_id)
                    })
                })
        });
        if shared {
            return Err(verr("Shared sample arrays cannot be translated independently"));
        }
        let field = read_spatial_field(&out, &field_id)?;
        let entry = out.get("arrays").and_then(|a| a.get(&field_id)).cloned().unwrap_or_else(|| json!({}));
        let payload_sha = entry.get("sha256").filter(|v| truthy(v)).map(py_str);
        let Some(payload_sha) = payload_sha.filter(|_| entry.get("b64").is_some_and(truthy)) else {
            return Err(verr("Rigid sample translation requires an identified inline array"));
        };
        let old_reg = field.grid.registration.to_wire();
        let reg_id = py_str(&old_reg["registration_id"]);
        let source =
            nodes[&root].get("attrs").and_then(|a| a.get("source")).cloned().unwrap_or_else(|| json!({}));
        let spatial =
            meta.get("spatial_fields").and_then(|s| s.get(&field_id)).cloned().unwrap_or_else(|| json!({}));
        let topology = meta.get("topology").cloned().unwrap_or_else(|| json!({}));
        let mut registrations: Vec<Option<Value>> = vec![
            spatial.get("grid").cloned(),
            source.get("registration").cloned(),
            entry.get("grid").cloned(),
        ];
        if truthy(&topology) {
            if topology.get("array_key").map(py_str).as_deref() != Some(field_id.as_str()) {
                return Err(verr("Another topology field is registered in this bundle"));
            }
            registrations.push(topology.get("registration").cloned());
        }
        for reg in registrations.into_iter().flatten().filter(|r| !r.is_null()) {
            let calculated = py_str(&GridRegistration::from_wire(&reg)?.to_wire()["registration_id"]);
            let declared = reg.get("registration_id").map_or_else(|| calculated.clone(), py_str);
            if calculated != reg_id || declared != calculated {
                return Err(verr("Sample registration representations disagree or are stale"));
            }
        }
        let params = nodes[&root].get("params").cloned().unwrap_or_else(|| json!({}));
        let origin = Arr::from_opt(params.get("origin"))?;
        let spacing = Arr::from_opt(params.get("spacing"))?;
        let old_origin = Arr::from_json(&old_reg["origin"])?;
        let old_basis = Arr::from_json(&old_reg["basis"])?;
        let diag_ok = spacing.shape == [3]
            && old_basis.shape == [3, 3]
            && (0..3).all(|i| {
                (0..3).all(|j| old_basis.data[3 * i + j] == if i == j { spacing.data[i] } else { 0.0 })
            });
        if origin.shape != [3] || spacing.shape != [3] || origin.data != old_origin.data || !diag_ok {
            return Err(verr("Sample evaluator origin/spacing differs from its physical registration"));
        }
        if spatial.get("protected_masks").is_some_and(truthy) {
            return Err(verr("Registered protection masks require an explicit authoritative mask decoder"));
        }
        for row in &selections {
            let identity = row.get("field_identity").cloned().unwrap_or_else(|| json!({}));
            if row.get("field_id").map(py_str).as_deref() != Some(field_id.as_str())
                || identity.get("registration_id").map(py_str).as_deref() != Some(reg_id.as_str())
                || identity.get("payload_sha256").map(py_str).as_deref() != Some(payload_sha.as_str())
            {
                return Err(verr("Saved cell selection has stale field registration or payload identity"));
            }
            if row.get("protected_runs").is_some_and(truthy)
                || row.get("protected_by_reason").is_some_and(truthy)
            {
                return Err(verr(
                    "Saved protected selections require authoritative protection reconstruction",
                ));
            }
            SpatialSelection::from_mapping(row, &field.values, field.grid.clone(), &BTreeMap::new(), None)?;
        }
        if delta.iter().all(|v| *v == 0.0) {
            return Ok(unchanged());
        }
        let new_origin: [f64; 3] = std::array::from_fn(|a| origin.data[a] + delta[a]);
        let mut new_reg = old_reg.clone();
        if let Some(m) = new_reg.as_object_mut() {
            m.insert("origin".into(), jfs(&new_origin));
        }
        let new_reg = GridRegistration::from_wire(&new_reg)?.to_wire();
        let new_grid = GridGeometry::from_shape(&field.grid.shape, &new_reg)?;
        let bounds = new_grid.serialise()["bounds_mm"].clone();
        if let Some(p) = mut_path(&mut out, &["nodes", &root, "params"]) {
            p.insert("origin".into(), jfs(&new_origin));
        }
        if let Some(s) = mut_path(&mut out, &["meta", "implexity", "spatial_fields", &field_id]) {
            if s.contains_key("grid") {
                s.insert("grid".into(), new_reg.clone());
            }
            if s.contains_key("bounds_mm") {
                s.insert("bounds_mm".into(), bounds.clone());
            }
        }
        if let Some(s) = mut_path(&mut out, &["nodes", &root, "attrs", "source"])
            && s.contains_key("registration")
        {
            s.insert("registration".into(), new_reg.clone());
        }
        if let Some(e) = mut_path(&mut out, &["arrays", &field_id]) {
            if e.contains_key("grid") {
                e.insert("grid".into(), new_reg.clone());
            }
            if e.contains_key("bounds_mm") {
                e.insert("bounds_mm".into(), bounds.clone());
            }
        }
        if truthy(&topology)
            && let Some(t) = mut_path(&mut out, &["meta", "implexity", "topology"])
        {
            t.insert("registration".into(), new_reg.clone());
            if t.contains_key("registration_id") {
                t.insert("registration_id".into(), new_reg["registration_id"].clone());
            }
        }
        let moved = build(&out, None, None)?;
        let identity = json!({"structure_id": moved.structure_id(), "content_id": moved.content_id(),
            "registration_id": new_reg["registration_id"], "payload_sha256": payload_sha});
        let ctx = CellContext {
            values: field.values.clone(),
            grid: field.grid.clone(),
            new_grid,
            identity,
            field_id: field_id.clone(),
            payload_sha: payload_sha.clone(),
        };
        let moved_rows: Vec<Value> =
            selections.iter().map(|row| move_cell(row, &ctx, attachment_policy)).collect::<AResult<_>>()?;
        if let Some(list) = out
            .get_mut("meta")
            .and_then(|m| m.get_mut("implexity"))
            .and_then(|m| m.get_mut("interaction"))
            .and_then(|m| m.get_mut("spatial_selections"))
            .and_then(Value::as_array_mut)
        {
            for (slot, row) in list.iter_mut().zip(moved_rows) {
                *slot = row;
            }
        }
        if let Some(m) = evidence.as_object_mut() {
            m.insert("field_id".into(), Value::from(field_id));
            m.insert("old_registration_id".into(), Value::from(reg_id));
            m.insert("new_registration_id".into(), new_reg["registration_id"].clone());
        }
        cell_context = Some(ctx);
    } else {
        if delta.iter().all(|v| *v == 0.0) {
            return Ok(unchanged());
        }
        let mut name = format!("{root}_rigid_translation");
        while nodes.contains_key(&name) {
            name.push('_');
        }
        let o = crate::py::obj_mut(&mut out)?;
        if let Some(n) = o.get_mut("nodes").and_then(Value::as_object_mut) {
            n.insert(
                name.clone(),
                json!({"kind": "translate", "children": [{"name": "part", "node": root}],
                    "params": {"dx_mm": delta[0], "dy_mm": delta[1], "dz_mm": delta[2]}}),
            );
        }
        o.insert("root".into(), Value::from(name.clone()));
        if let Some(outputs) = o.get_mut("outputs").and_then(Value::as_object_mut) {
            for target in outputs.values_mut() {
                if target.as_str() == Some(root.as_str()) {
                    *target = Value::from(name.clone());
                }
            }
        }
    }
    let moved = build(&out, None, None)?;
    if truthy(&engineering) {
        let mut t = Translator {
            delta,
            policy: attachment_policy,
            before: &model,
            moved: &moved,
            cell: cell_context.as_ref(),
            ids: BTreeSet::new(),
        };
        translate_engineering(&mut engineering, &mut t)?;
        if let Some(m) = engineering.get_mut("model").and_then(Value::as_object_mut) {
            m.insert("structure_id".into(), moved.structure_id().map_or(Value::Null, Value::from));
            m.insert("content_id".into(), moved.content_id().map_or(Value::Null, Value::from));
        }
    }
    if let Some(arrays) = document.get("arrays").and_then(Value::as_object) {
        for (key, original) in arrays {
            let strip = |v: &Value| -> Map<String, Value> {
                v.as_object()
                    .map(|m| {
                        m.iter()
                            .filter(|(k, _)| *k != "grid" && *k != "bounds_mm")
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let now = out
                .get("arrays")
                .and_then(|a| a.get(key))
                .ok_or_else(|| AuthoringError::Key(crate::py::repr(&Value::from(key.clone()))))?;
            if strip(now) != strip(original) {
                return Err(verr("Translation unexpectedly changed array payload or nonspatial metadata"));
            }
        }
    }
    Ok(json!({"document": out, "problem": engineering, "evidence": evidence}))
}
