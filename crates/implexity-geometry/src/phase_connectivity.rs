// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

use crate::pyfmt;

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PhaseError {
    #[error("{0}")]
    Value(String),
    #[error("phase connectivity gate: {}", .issues.join("; "))]
    Gate {
        issues: Vec<String>,
        report: Box<Value>,
    },
    #[error("{0}")]
    Runtime(String),
}

type PR<T> = Result<T, PhaseError>;

fn verr<T>(m: impl Into<String>) -> PR<T> {
    Err(PhaseError::Value(m.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape3(pub [usize; 3]);

impl Shape3 {
    fn len(self) -> usize {
        self.0.iter().product()
    }
    fn coords(self, c: usize) -> [usize; 3] {
        let [_, ny, nz] = self.0;
        [c / (ny * nz), (c / nz) % ny, c % nz]
    }
    fn neighbours(self, c: usize) -> impl Iterator<Item = usize> {
        let [nx, ny, nz] = self.0;
        let [i, j, k] = self.coords(c);
        let st = [ny * nz, nz, 1];
        let lim = [nx, ny, nz];
        let p = [i, j, k];
        (0..3).flat_map(move |a| {
            let lo = (p[a] > 0).then(|| c - st[a]);
            let hi = (p[a] + 1 < lim[a]).then(|| c + st[a]);
            [lo, hi].into_iter().flatten()
        })
    }
    fn py(self) -> String {
        pyfmt::PyObj::Tuple(
            self.0.iter().map(|n| pyfmt::PyObj::Int(i64::try_from(*n).unwrap_or(0))).collect(),
        )
        .repr()
    }
}

#[must_use]
pub fn label(mask: &[bool], shape: Shape3) -> (Vec<i32>, usize) {
    let mut ids = vec![0i32; mask.len()];
    let mut count = 0usize;
    let mut stack = Vec::new();
    for seed in 0..mask.len() {
        if !mask[seed] || ids[seed] != 0 {
            continue;
        }
        count += 1;
        let id = i32::try_from(count).unwrap_or(i32::MAX);
        ids[seed] = id;
        stack.push(seed);
        while let Some(c) = stack.pop() {
            for q in shape.neighbours(c) {
                if mask[q] && ids[q] == 0 {
                    ids[q] = id;
                    stack.push(q);
                }
            }
        }
    }
    (ids, count)
}

#[must_use]
pub fn binary_erosion(mask: &[bool], shape: Shape3) -> Vec<bool> {
    let [nx, ny, nz] = shape.0;
    (0..mask.len())
        .map(|c| {
            if !mask[c] {
                return false;
            }
            let [i, j, k] = shape.coords(c);
            if i == 0 || j == 0 || k == 0 || i + 1 == nx || j + 1 == ny || k + 1 == nz {
                return false;
            }
            shape.neighbours(c).all(|q| mask[q])
        })
        .collect()
}


pub fn boundary_mask(shape: Shape3, faces: Option<&[(usize, &str)]>) -> PR<Vec<bool>> {
    if shape.0.contains(&0) {
        return verr("positive 3-D cell shape required");
    }
    let mut out = vec![false; shape.len()];
    for &(axis, side) in faces.unwrap_or(&[]) {
        if axis > 2 || (side != "lo" && side != "hi") {
            return verr("invalid phase-connectivity boundary face");
        }
        for (c, o) in out.iter_mut().enumerate() {
            let p = shape.coords(c)[axis];
            if (side == "lo" && p == 0) || (side == "hi" && p + 1 == shape.0[axis]) {
                *o = true;
            }
        }
    }
    Ok(out)
}

fn validated_phase(phase: &[f64], shape: Shape3) -> PR<()> {
    if shape.0.contains(&0)
        || phase.len() != shape.len()
        || !phase.iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p))
    {
        return verr("finite 3-D phase fraction in [0,1] required");
    }
    Ok(())
}

fn validated_spacing(spacing: &[f64]) -> PR<[f64; 3]> {
    if spacing.len() != 3 || !spacing.iter().all(|h| h.is_finite() && *h > 0.0) {
        return verr("positive physical cell spacing required");
    }
    Ok([spacing[0], spacing[1], spacing[2]])
}


pub fn validated_thresholds(values: &[f64]) -> PR<Vec<f64>> {
    let unique: BTreeSet<u64> = values.iter().map(|v| v.to_bits()).collect();
    if values.is_empty()
        || values.iter().any(|v| !v.is_finite() || !(*v > 0.0 && *v <= 1.0))
        || unique.len() != values.len()
    {
        return verr("unique thresholds in (0,1] required");
    }
    Ok(values.to_vec())
}

fn validated_mask(mask: &[bool], shape: Shape3, name: &str) -> PR<Vec<bool>> {
    if mask.len() != shape.len() {
        return verr(format!("{name} must have the phase-field shape {}", shape.py()));
    }
    Ok(mask.to_vec())
}

fn validated_labels(labels: (&str, &str)) -> PR<(String, String)> {
    let (a, b) = (labels.0.trim(), labels.1.trim());
    if a.is_empty() || b.is_empty() {
        return verr("labels must contain two nonempty opaque strings");
    }
    Ok((a.to_string(), b.to_string()))
}

#[derive(Clone, Debug)]
pub struct RoleMasks {
    pub terminal_a: Vec<bool>,
    pub terminal_b: Vec<bool>,
    pub anchor: Vec<bool>,
    pub authorized_single_terminal: Option<Vec<bool>>,
}

struct Roles {
    a: Vec<bool>,
    b: Vec<bool>,
    anchor: Vec<bool>,
    auth: Vec<bool>,
}

fn validated_roles(shape: Shape3, m: &RoleMasks) -> PR<Roles> {
    let a = validated_mask(&m.terminal_a, shape, "terminal_a")?;
    let b = validated_mask(&m.terminal_b, shape, "terminal_b")?;
    let anchor = validated_mask(&m.anchor, shape, "anchor")?;
    let auth = match &m.authorized_single_terminal {
        None => vec![false; shape.len()],
        Some(x) => validated_mask(x, shape, "authorized_single_terminal")?,
    };
    if a.iter().zip(&b).any(|(x, y)| *x && *y) {
        return verr("terminal_a and terminal_b masks must be disjoint");
    }
    Ok(Roles { a, b, anchor, auth })
}

fn ids_touching(ids: &[i32], mask: &[bool]) -> BTreeSet<i32> {
    ids.iter().zip(mask).filter(|(i, m)| **m && **i != 0).map(|(i, _)| *i).collect()
}

struct Components {
    phase_binary: Vec<bool>,
    complement_binary: Vec<bool>,
    phase_ids: Vec<i32>,
    complement_ids: Vec<i32>,
    phase_count: usize,
    complement_count: usize,
    through: BTreeSet<i32>,
    closed: BTreeSet<i32>,
    single: BTreeSet<i32>,
    authorized_single: BTreeSet<i32>,
    unauthorized_single: BTreeSet<i32>,
    anchored: BTreeSet<i32>,
    unanchored: BTreeSet<i32>,
}

fn all_ids(count: usize) -> BTreeSet<i32> {
    (1..=count).map(|i| i32::try_from(i).unwrap_or(i32::MAX)).collect()
}

fn classify(phase: &[f64], threshold: f64, r: &Roles, shape: Shape3, eroded: bool) -> Components {
    let mut phase_binary: Vec<bool> = phase.iter().map(|p| *p >= threshold).collect();
    let mut complement_binary: Vec<bool> = phase_binary.iter().map(|b| !b).collect();
    if eroded {
        let ce = binary_erosion(&complement_binary, shape);
        complement_binary = (0..phase.len())
            .map(|c| ce[c] || (complement_binary[c] && (r.a[c] || r.b[c] || r.auth[c])))
            .collect();
        let pe = binary_erosion(&phase_binary, shape);
        phase_binary = (0..phase.len()).map(|c| pe[c] || (phase_binary[c] && r.anchor[c])).collect();
    }
    let (complement_ids, complement_count) = label(&complement_binary, shape);
    let (phase_ids, phase_count) = label(&phase_binary, shape);
    let ta = ids_touching(&complement_ids, &r.a);
    let tb = ids_touching(&complement_ids, &r.b);
    let through: BTreeSet<i32> = ta.intersection(&tb).copied().collect();
    let terminal: BTreeSet<i32> = ta.union(&tb).copied().collect();
    let closed: BTreeSet<i32> = all_ids(complement_count).difference(&terminal).copied().collect();
    let single: BTreeSet<i32> = terminal.difference(&through).copied().collect();
    let auth = ids_touching(&complement_ids, &r.auth);
    let authorized_single: BTreeSet<i32> = single.intersection(&auth).copied().collect();
    let unauthorized_single: BTreeSet<i32> = single.difference(&authorized_single).copied().collect();
    let anchored = ids_touching(&phase_ids, &r.anchor);
    let unanchored: BTreeSet<i32> = all_ids(phase_count).difference(&anchored).copied().collect();
    Components {
        phase_binary,
        complement_binary,
        phase_ids,
        complement_ids,
        phase_count,
        complement_count,
        through,
        closed,
        single,
        authorized_single,
        unauthorized_single,
        anchored,
        unanchored,
    }
}

fn selected(ids: &[i32], set: &BTreeSet<i32>) -> Vec<bool> {
    ids.iter().map(|i| set.contains(i)).collect()
}

fn count_in(ids: &[i32], set: &BTreeSet<i32>) -> usize {
    if set.is_empty() { 0 } else { ids.iter().filter(|i| set.contains(i)).count() }
}

fn volume(fraction: &[f64], ids: &[i32], set: &BTreeSet<i32>, cell_volume: f64) -> f64 {
    if set.is_empty() {
        return 0.0;
    }
    let v: Vec<f64> = fraction.iter().zip(ids).filter(|(_, i)| set.contains(i)).map(|(f, _)| *f).collect();
    crate::numpy::sum(&v) * cell_volume
}

fn masked_sum(fraction: &[f64], mask: &[bool]) -> f64 {
    let v: Vec<f64> = fraction.iter().zip(mask).filter(|(_, m)| **m).map(|(f, _)| *f).collect();
    crate::numpy::sum(&v)
}

fn frac_or_null(a: f64, b: f64) -> Value {
    if b == 0.0 { Value::Null } else { json!(a / b) }
}

fn neutral_metrics(
    phase: &[f64],
    cell_volume: f64,
    threshold: f64,
    r: &Roles,
    shape: Shape3,
    eroded: bool,
) -> Value {
    let c = classify(phase, threshold, r, shape, eroded);
    let complement: Vec<f64> = phase.iter().map(|p| 1.0 - p).collect();
    let qc = masked_sum(&complement, &c.complement_binary) * cell_volume;
    let qp = masked_sum(phase, &c.phase_binary) * cell_volume;
    let closed_v = volume(&complement, &c.complement_ids, &c.closed, cell_volume);
    let single_v = volume(&complement, &c.complement_ids, &c.single, cell_volume);
    let unauth_v = volume(&complement, &c.complement_ids, &c.unauthorized_single, cell_volume);
    let unanch_v = volume(phase, &c.phase_ids, &c.unanchored, cell_volume);
    let passed = !c.through.is_empty()
        && c.closed.is_empty()
        && c.unauthorized_single.is_empty()
        && c.unanchored.is_empty();
    json!({
        "threshold": threshold, "one_cell_eroded": eroded,
        "phase_components": c.phase_count, "complement_components": c.complement_count,
        "complement_through_components": c.through.len(), "complement_through_path": !c.through.is_empty(),
        "closed_complement_components": c.closed.len(), "closed_complement_cells": count_in(&c.complement_ids, &c.closed),
        "single_terminal_components": c.single.len(), "authorized_single_terminal_components": c.authorized_single.len(),
        "unauthorized_single_terminal_components": c.unauthorized_single.len(),
        "unauthorized_single_terminal_cells": count_in(&c.complement_ids, &c.unauthorized_single),
        "unanchored_phase_components": c.unanchored.len(), "unanchored_phase_cells": count_in(&c.phase_ids, &c.unanchored),
        "qualified_complement_volume_m3": qc, "closed_complement_volume_m3": closed_v,
        "closed_complement_fraction_of_qualified": frac_or_null(closed_v, qc),
        "single_terminal_complement_volume_m3": single_v,
        "unauthorized_single_terminal_complement_volume_m3": unauth_v,
        "qualified_phase_volume_m3": qp, "unanchored_phase_volume_m3": unanch_v,
        "unanchored_phase_fraction_of_qualified": frac_or_null(unanch_v, qp),
        "zero_closed_complement_components": c.closed.is_empty(),
        "zero_unauthorized_single_terminal_components": c.unauthorized_single.is_empty(),
        "zero_unanchored_phase_components": c.unanchored.is_empty(),
        "hard_connectivity_passed": passed,
    })
}


pub fn audit_complementary_phase_connectivity(
    phase: &[f64],
    shape: Shape3,
    spacing_m: &[f64],
    roles: &RoleMasks,
    thresholds: &[f64],
    one_cell_erosion: bool,
    labels: (&str, &str),
) -> PR<Value> {
    validated_phase(phase, shape)?;
    let spacing = validated_spacing(spacing_m)?;
    let ts = validated_thresholds(thresholds)?;
    let labels = validated_labels(labels)?;
    let r = validated_roles(shape, roles)?;
    let cell_volume = spacing[0] * spacing[1] * spacing[2];
    let rows: Vec<Value> =
        ts.iter().map(|t| neutral_metrics(phase, cell_volume, *t, &r, shape, false)).collect();
    let erosion_rows: Vec<Value> = if one_cell_erosion {
        ts.iter().map(|t| neutral_metrics(phase, cell_volume, *t, &r, shape, true)).collect()
    } else {
        Vec::new()
    };
    let sigs: BTreeSet<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{}|{}|{}|{}",
                r["complement_through_path"],
                r["closed_complement_components"],
                r["unauthorized_single_terminal_components"],
                r["unanchored_phase_components"]
            )
        })
        .collect();
    let all_passed = |rows: &[Value]| rows.iter().all(|r| r["hard_connectivity_passed"] == Value::Bool(true));
    let gray: Vec<f64> = phase.iter().map(|p| 4.0 * p * (1.0 - p)).collect();
    Ok(json!({
        "connectivity": "6-neighbour face adjacency; not corner/edge contact",
        "classification": "complementary hard partition: primary fraction >= threshold; complement fraction < threshold",
        "terminal_definition": "provider-authored exact cell masks",
        "labels": {"phase": labels.0, "complement": labels.1, "terminal_a": "terminal_a", "terminal_b": "terminal_b", "anchor": "anchor"},
        "rows": rows,
        "one_cell_erosion": {"requested": one_cell_erosion, "terminal_and_anchor_cells_retained": one_cell_erosion, "rows": erosion_rows},
        "all_thresholds_passed": all_passed(&rows),
        "erosion_all_thresholds_passed": if one_cell_erosion { json!(all_passed(&erosion_rows)) } else { Value::Null },
        "gray_measure": crate::numpy::mean(&gray),
        "threshold_sensitive": sigs.len() > 1,
        "geometry_modified": false,
        "differentiable_metric": false,
        "caveats": [
            "Threshold audit of a relaxed cell field, not sharp reconstructed-interface convergence.",
            "A through component may contain dead branches; no guaranteed phase renewal or minimum feature width.",
            "Phase contact with an authored anchor does not prove sufficient constraint rank.",
            "One-cell erosion is a discrete robustness screen, not a differentiable length-scale constraint.",
        ],
    }))
}

fn neutral_gate_issues(rows: &Value, context: &str, labels: &(String, String)) -> Vec<String> {
    let mut issues = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        let prefix =
            format!("{context} threshold {}", pyfmt::g(row["threshold"].as_f64().unwrap_or(f64::NAN)));
        if row["complement_through_path"] != Value::Bool(true) {
            issues.push(format!("{prefix}: no {} terminal-a to terminal-b path", labels.1));
        }
        let n = |k: &str| row[k].as_u64().unwrap_or(0);
        if n("closed_complement_components") > 0 {
            issues.push(format!(
                "{prefix}: {} closed {} component(s)",
                n("closed_complement_components"),
                labels.1
            ));
        }
        if n("unauthorized_single_terminal_components") > 0 {
            issues.push(format!(
                "{prefix}: {} unauthorized single-terminal {} component(s)",
                n("unauthorized_single_terminal_components"),
                labels.1
            ));
        }
        if n("unanchored_phase_components") > 0 {
            issues.push(format!(
                "{prefix}: {} unanchored {} component(s)",
                n("unanchored_phase_components"),
                labels.0
            ));
        }
    }
    issues
}


pub fn require_complementary_phase_connectivity(
    phase: &[f64],
    shape: Shape3,
    spacing_m: &[f64],
    roles: &RoleMasks,
    thresholds: Option<&[f64]>,
    require_one_cell_erosion: bool,
    labels: (&str, &str),
) -> PR<Value> {
    let ts = validated_thresholds(thresholds.unwrap_or(&[0.5]))?;
    let lb = validated_labels(labels)?;
    let mut report = audit_complementary_phase_connectivity(
        phase,
        shape,
        spacing_m,
        roles,
        &ts,
        require_one_cell_erosion,
        labels,
    )?;
    let mut issues = neutral_gate_issues(&report["rows"], "base", &lb);
    if require_one_cell_erosion {
        issues.extend(neutral_gate_issues(&report["one_cell_erosion"]["rows"], "one-cell erosion", &lb));
    }
    gate(&mut report, issues, require_one_cell_erosion)
}

fn gate(report: &mut Value, issues: Vec<String>, erosion: bool) -> PR<Value> {
    let required: Vec<Value> =
        report["rows"].as_array().into_iter().flatten().map(|r| r["threshold"].clone()).collect();
    report["gate"] = json!({"required_thresholds": required, "one_cell_erosion_required": erosion, "passed": issues.is_empty(), "issues": issues});
    if issues.is_empty() {
        Ok(report.clone())
    } else {
        Err(PhaseError::Gate { issues, report: Box::new(report.clone()) })
    }
}

#[derive(Clone, Debug)]
pub struct ComponentFields {
    pub threshold: f64,
    pub one_cell_eroded: bool,
    pub labels: (String, String),
    pub phase_component_id: Vec<i32>,
    pub complement_component_id: Vec<i32>,
    pub masks: Vec<(String, Vec<bool>)>,
    pub component_ids: Vec<(String, Vec<i32>)>,
}


pub fn complementary_phase_component_fields(
    phase: &[f64],
    shape: Shape3,
    threshold: f64,
    roles: &RoleMasks,
    one_cell_eroded: bool,
    labels: (&str, &str),
) -> PR<ComponentFields> {
    validated_phase(phase, shape)?;
    let t = validated_thresholds(&[threshold])?[0];
    let lb = validated_labels(labels)?;
    let r = validated_roles(shape, roles)?;
    let c = classify(phase, t, &r, shape, one_cell_eroded);
    let masks = vec![
        ("qualified_phase_mask".to_string(), c.phase_binary.clone()),
        ("qualified_complement_mask".into(), c.complement_binary.clone()),
        ("through_complement_mask".into(), selected(&c.complement_ids, &c.through)),
        ("closed_complement_mask".into(), selected(&c.complement_ids, &c.closed)),
        ("single_terminal_complement_mask".into(), selected(&c.complement_ids, &c.single)),
        (
            "authorized_single_terminal_complement_mask".into(),
            selected(&c.complement_ids, &c.authorized_single),
        ),
        (
            "unauthorized_single_terminal_complement_mask".into(),
            selected(&c.complement_ids, &c.unauthorized_single),
        ),
        ("anchored_phase_mask".into(), selected(&c.phase_ids, &c.anchored)),
        ("unanchored_phase_mask".into(), selected(&c.phase_ids, &c.unanchored)),
    ];
    let v = |s: &BTreeSet<i32>| s.iter().copied().collect::<Vec<i32>>();
    let component_ids = vec![
        ("through_complement".to_string(), v(&c.through)),
        ("closed_complement".into(), v(&c.closed)),
        ("single_terminal_complement".into(), v(&c.single)),
        ("authorized_single_terminal_complement".into(), v(&c.authorized_single)),
        ("unauthorized_single_terminal_complement".into(), v(&c.unauthorized_single)),
        ("anchored_phase".into(), v(&c.anchored)),
        ("unanchored_phase".into(), v(&c.unanchored)),
    ];
    Ok(ComponentFields {
        threshold: t,
        one_cell_eroded,
        labels: lb,
        phase_component_id: c.phase_ids,
        complement_component_id: c.complement_ids,
        masks,
        component_ids,
    })
}


pub fn fill_terminal_free_complement_components(
    primary: &[bool],
    shape: Shape3,
    terminal_a: &[bool],
    terminal_b: &[bool],
) -> PR<(Vec<bool>, Vec<bool>, Value)> {
    if shape.0.contains(&0) || primary.len() != shape.len() {
        return verr("positive 3-D primary-phase mask required");
    }
    let a = validated_mask(terminal_a, shape, "terminal_a")?;
    let b = validated_mask(terminal_b, shape, "terminal_b")?;
    if a.iter().zip(&b).any(|(x, y)| *x && *y) {
        return verr("terminal_a and terminal_b masks must be disjoint");
    }
    let comp: Vec<bool> = primary.iter().map(|p| !p).collect();
    let (ids, count) = label(&comp, shape);
    let ta = ids_touching(&ids, &a);
    let tb = ids_touching(&ids, &b);
    let retained: BTreeSet<i32> = ta.union(&tb).copied().collect();
    let filled_ids: BTreeSet<i32> = all_ids(count).difference(&retained).copied().collect();
    let filled: Vec<bool> =
        (0..primary.len()).map(|c| filled_ids.contains(&ids[c]) && !(a[c] || b[c])).collect();
    let updated: Vec<bool> = primary.iter().zip(&filled).map(|(p, f)| *p || *f).collect();
    let preserved = (0..primary.len()).filter(|c| a[*c] || b[*c]).all(|c| updated[c] == primary[c]);
    if !preserved {
        return Err(PhaseError::Runtime("terminal cells changed during complement fill".into()));
    }
    let filled_count = filled.iter().filter(|f| **f).count();
    let report = json!({
        "schema": "implexity-terminal-free-complement-fill/1", "operation": "terminal_free_complement_to_primary",
        "connectivity": "6-neighbour face adjacency", "shape": shape.0, "complement_components_before": count,
        "filled_component_count": filled_ids.len(), "filled_cell_count": filled_count,
        "retained_terminal_component_count": retained.len(),
        "retained_through_component_count": ta.intersection(&tb).count(),
        "retained_single_terminal_component_count": ta.symmetric_difference(&tb).count(),
        "terminal_cells_preserved": preserved, "geometry_modified": filled_count > 0,
    });
    Ok((updated, filled, report))
}

fn settlement_summary(c: &Components) -> Value {
    let terminal: BTreeSet<i32> = c.through.union(&c.single).copied().collect();
    json!({
        "phase_components": c.phase_count, "complement_components": c.complement_count,
        "complement_through_path": !c.through.is_empty(), "complement_through_components": c.through.len(),
        "closed_complement_components": c.closed.len(), "closed_complement_cells": count_in(&c.complement_ids, &c.closed),
        "single_terminal_complement_components": c.single.len(),
        "authorized_single_terminal_complement_components": c.authorized_single.len(),
        "unauthorized_single_terminal_complement_components": c.unauthorized_single.len(),
        "unauthorized_single_terminal_complement_cells": count_in(&c.complement_ids, &c.unauthorized_single),
        "anchored_phase_components": c.anchored.len(), "unanchored_phase_components": c.unanchored.len(),
        "unanchored_phase_cells": count_in(&c.phase_ids, &c.unanchored),
        "role_connected_cells": count_in(&c.phase_ids, &c.anchored) + count_in(&c.complement_ids, &terminal),
    })
}

fn settlement_hard_issues(c: &Components, stage: &str) -> Vec<String> {
    let mut issues = Vec::new();
    if c.through.is_empty() {
        issues.push(format!("{stage}: no complement terminal-a to terminal-b path"));
    }
    if !c.unauthorized_single.is_empty() {
        issues.push(format!(
            "{stage}: {} unauthorized single-terminal complement component(s)",
            c.unauthorized_single.len()
        ));
    }
    issues
}


#[allow(clippy::too_many_lines)]
pub fn settle_endpoint_phase_components(
    primary: &[bool],
    shape: Shape3,
    roles: &RoleMasks,
) -> PR<(Vec<bool>, Vec<bool>, Vec<bool>, Value)> {
    if shape.0.contains(&0) || primary.len() != shape.len() {
        return verr("positive 3-D primary-phase mask required");
    }
    let r = validated_roles(shape, roles)?;
    let n = primary.len();
    let terminal: Vec<bool> = (0..n).map(|c| r.a[c] || r.b[c]).collect();
    if (0..n).any(|c| r.anchor[c] && terminal[c]) {
        return verr("anchor and terminal masks must be disjoint");
    }
    if (0..n).any(|c| r.anchor[c] && !primary[c]) {
        return verr("anchor cells must belong to the primary phase");
    }
    if (0..n).any(|c| terminal[c] && primary[c]) {
        return verr("terminal cells must belong to the complement phase");
    }
    let original = primary.to_vec();
    let mut cur = primary.to_vec();
    let classify_b = |v: &[bool]| {
        let f: Vec<f64> = v.iter().map(|b| if *b { 1.0 } else { 0.0 }).collect();
        classify(&f, 0.5, &r, shape, false)
    };
    let mut comps = classify_b(&cur);
    let mut report = Map::new();
    report.insert("schema".into(), json!("implexity-endpoint-phase-settlement/1"));
    report.insert("operation".into(), json!("bidirectional_component_settlement"));
    report.insert("connectivity".into(), json!("6-neighbour face adjacency"));
    report.insert("shape".into(), json!(shape.0));
    report.insert(
        "update_policy".into(),
        json!("simultaneous whole-component changes with monotone growth of role-connected cells"),
    );
    report.insert("initial".into(), settlement_summary(&comps));
    report.insert("passes".into(), json!([]));
    report.insert("converged".into(), json!(false));
    let fail = |report: &mut Map<String, Value>, issues: Vec<String>| -> PhaseError {
        report.insert("hard_gate".into(), json!({"passed": false, "issues": issues}));
        PhaseError::Gate { issues, report: Box::new(Value::Object(report.clone())) }
    };
    let initial = settlement_hard_issues(&comps, "initial");
    if !initial.is_empty() {
        return Err(fail(&mut report, initial));
    }
    let (mut op_p2c, mut op_c2p, mut op_rm, mut op_fill) = (0usize, 0usize, 0usize, 0usize);
    let maximum = n;
    let mut passes = Vec::new();
    let mut converged = false;
    for pass in 0..=maximum {
        let hard = settlement_hard_issues(&comps, &format!("pass {pass}"));
        if !hard.is_empty() {
            report.insert("passes".into(), Value::Array(passes));
            report.insert("final".into(), settlement_summary(&comps));
            return Err(fail(&mut report, hard));
        }
        if comps.unanchored.is_empty() && comps.closed.is_empty() {
            converged = true;
            break;
        }
        if pass == maximum {
            return Err(PhaseError::Runtime("endpoint phase settlement exceeded finite bound".into()));
        }
        let remove = selected(&comps.phase_ids, &comps.unanchored);
        let fill = selected(&comps.complement_ids, &comps.closed);
        if (0..n).any(|c| (remove[c] && r.anchor[c]) || (fill[c] && terminal[c])) {
            return Err(PhaseError::Runtime("authored role cell selected for settlement".into()));
        }
        let before = settlement_summary(&comps)["role_connected_cells"].as_u64().unwrap_or(0);
        let updated: Vec<bool> = (0..n).map(|c| (cur[c] && !remove[c]) || fill[c]).collect();
        if (0..n).any(|c| (r.anchor[c] && !updated[c]) || (terminal[c] && updated[c])) {
            return Err(PhaseError::Runtime("authored role cell changed during settlement".into()));
        }
        let next = classify_b(&updated);
        let after = settlement_summary(&next)["role_connected_cells"].as_u64().unwrap_or(0);
        if after <= before {
            return Err(PhaseError::Runtime(
                "endpoint settlement violated monotone convergence invariant".into(),
            ));
        }
        let removed = remove.iter().filter(|x| **x).count();
        let filled = fill.iter().filter(|x| **x).count();
        passes.push(json!({"pass": pass + 1, "primary_to_complement_components": comps.unanchored.len(), "primary_to_complement_cells": removed,
            "complement_to_primary_components": comps.closed.len(), "complement_to_primary_cells": filled,
            "role_connected_cells_before": before, "role_connected_cells_after": after}));
        op_p2c += removed;
        op_c2p += filled;
        op_rm += comps.unanchored.len();
        op_fill += comps.closed.len();
        cur = updated;
        comps = next;
    }
    if !converged {
        return Err(PhaseError::Runtime("endpoint phase settlement did not converge".into()));
    }
    report.insert("passes".into(), Value::Array(passes.clone()));
    let final_summary = settlement_summary(&comps);
    let mut final_issues = settlement_hard_issues(&comps, "final");
    if final_summary["closed_complement_components"].as_u64().unwrap_or(0) > 0 {
        final_issues.push("final: terminal-free complement component remains".into());
    }
    if final_summary["unanchored_phase_components"].as_u64().unwrap_or(0) > 0 {
        final_issues.push("final: unanchored primary component remains".into());
    }
    if !final_issues.is_empty() {
        report.insert("final".into(), final_summary);
        return Err(fail(&mut report, final_issues));
    }
    let p2c: Vec<bool> = (0..n).map(|c| original[c] && !cur[c]).collect();
    let c2p: Vec<bool> = (0..n).map(|c| !original[c] && cur[c]).collect();
    let preserved = (0..n).filter(|c| terminal[*c] || r.anchor[*c]).all(|c| cur[c] == original[c]);
    if !preserved {
        return Err(PhaseError::Runtime("authored role cells changed during settlement".into()));
    }
    report.insert("converged".into(), json!(true));
    report.insert("pass_count".into(), json!(passes.len()));
    report.insert("final".into(), final_summary);
    report.insert(
        "operation_counts".into(),
        json!({"primary_to_complement_components": op_rm, "primary_to_complement_cells": op_p2c,
            "complement_to_primary_components": op_fill, "complement_to_primary_cells": op_c2p}),
    );
    report.insert(
        "net_counts".into(),
        json!({"primary_to_complement_cells": p2c.iter().filter(|x| **x).count(), "complement_to_primary_cells": c2p.iter().filter(|x| **x).count()}),
    );
    report.insert("role_cells_preserved".into(), json!(preserved));
    report.insert("geometry_modified".into(), json!(cur != original));
    report.insert("hard_gate".into(), json!({"passed": true, "issues": []}));
    Ok((cur, p2c, c2p, Value::Object(report)))
}

const NEUTRAL_TO_LEGACY_ROW: [(&str, &str); 23] = [
    ("complement_components", "fluid_components"),
    ("phase_components", "solid_components"),
    ("complement_through_components", "fluid_through_components"),
    ("complement_through_path", "fluid_through_path"),
    ("closed_complement_components", "closed_fluid_components"),
    ("closed_complement_cells", "closed_fluid_cells"),
    ("single_terminal_components", "one_opening_only_components"),
    ("authorized_single_terminal_components", "authorized_one_port_components"),
    ("unauthorized_single_terminal_components", "unauthorized_one_port_components"),
    ("unauthorized_single_terminal_cells", "unauthorized_one_port_cells"),
    ("unanchored_phase_components", "disconnected_solid_components"),
    ("unanchored_phase_cells", "disconnected_solid_cells"),
    ("qualified_complement_volume_m3", "qualified_fluid_volume_m3"),
    ("closed_complement_volume_m3", "closed_fluid_volume_m3"),
    ("closed_complement_fraction_of_qualified", "closed_fluid_fraction_of_qualified"),
    ("single_terminal_complement_volume_m3", "one_opening_only_fluid_volume_m3"),
    ("unauthorized_single_terminal_complement_volume_m3", "unauthorized_one_port_fluid_volume_m3"),
    ("qualified_phase_volume_m3", "qualified_solid_volume_m3"),
    ("unanchored_phase_volume_m3", "disconnected_solid_volume_m3"),
    ("unanchored_phase_fraction_of_qualified", "disconnected_solid_fraction_of_qualified"),
    ("zero_closed_complement_components", "zero_closed_fluid_components"),
    ("zero_unauthorized_single_terminal_components", "zero_unauthorized_one_port_components"),
    ("zero_unanchored_phase_components", "zero_floating_solid_components"),
];

fn legacy_row(row: &Value) -> Value {
    let Value::Object(m) = row else { return row.clone() };
    Value::Object(
        m.iter()
            .map(|(k, v)| {
                (
                    NEUTRAL_TO_LEGACY_ROW
                        .iter()
                        .find(|(n, _)| n == k)
                        .map_or_else(|| k.clone(), |(_, l)| (*l).to_string()),
                    v.clone(),
                )
            })
            .collect(),
    )
}

fn legacy_report(report: &Value, terminal_definition: &str) -> Value {
    let mut result = report.as_object().cloned().unwrap_or_default();
    result.remove("labels");
    result.insert("terminal_definition".into(), json!(terminal_definition));
    result.insert(
        "rows".into(),
        Value::Array(report["rows"].as_array().into_iter().flatten().map(legacy_row).collect()),
    );
    let mut erosion = report["one_cell_erosion"].as_object().cloned().unwrap_or_default();
    if let Some(v) = erosion.shift_remove("terminal_and_anchor_cells_retained") {
        erosion.insert("terminal_and_support_cells_retained".into(), v);
    }
    let rows: Vec<Value> =
        erosion.get("rows").and_then(Value::as_array).into_iter().flatten().map(legacy_row).collect();
    erosion.insert("rows".into(), Value::Array(rows));
    result.insert("one_cell_erosion".into(), Value::Object(erosion));
    result.insert("pressure_drainage_added".into(), json!(false));
    Value::Object(result)
}

#[derive(Clone, Debug, Default)]
pub struct LegacyRoles<'a> {
    pub inlet_faces: Option<&'a [(usize, &'a str)]>,
    pub outlet_faces: Option<&'a [(usize, &'a str)]>,
    pub support_faces: Option<&'a [(usize, &'a str)]>,
    pub inlet_mask: Option<&'a [bool]>,
    pub outlet_mask: Option<&'a [bool]>,
    pub solid_support_mask: Option<&'a [bool]>,
    pub authorized_one_port_mask: Option<&'a [bool]>,
}

fn legacy_role(
    shape: Shape3,
    faces: Option<&[(usize, &str)]>,
    mask: Option<&[bool]>,
    name: &str,
) -> PR<Vec<bool>> {
    if let Some(m) = mask {
        if faces.is_some_and(|f| !f.is_empty()) {
            return verr(format!("{name} mask and legacy faces are mutually exclusive"));
        }
        return validated_mask(m, shape, name);
    }
    boundary_mask(shape, faces)
}

fn legacy_masks(shape: Shape3, l: &LegacyRoles<'_>) -> PR<(RoleMasks, &'static str)> {
    let inlet = legacy_role(shape, l.inlet_faces, l.inlet_mask, "inlet")?;
    let outlet = legacy_role(shape, l.outlet_faces, l.outlet_mask, "outlet")?;
    let support = legacy_role(shape, l.support_faces, l.solid_support_mask, "solid support")?;
    let auth = match l.authorized_one_port_mask {
        None => None,
        Some(m) => Some(validated_mask(m, shape, "authorized one-port")?),
    };
    if inlet.iter().zip(&outlet).any(|(a, b)| *a && *b) {
        return verr("inlet and outlet masks must be disjoint");
    }
    let def = if l.inlet_mask.is_some() || l.outlet_mask.is_some() || l.solid_support_mask.is_some() {
        "provider-authored exact cell masks"
    } else {
        "legacy complete Cartesian faces"
    };
    Ok((
        RoleMasks {
            terminal_a: inlet,
            terminal_b: outlet,
            anchor: support,
            authorized_single_terminal: auth,
        },
        def,
    ))
}


pub fn audit_phase_connectivity(
    solid: &[f64],
    shape: Shape3,
    spacing_m: &[f64],
    roles: &LegacyRoles<'_>,
    thresholds: Option<&[f64]>,
    one_cell_erosion: bool,
) -> PR<Value> {
    validated_phase(solid, shape)?;
    let (masks, def) = legacy_masks(shape, roles)?;
    let report = audit_complementary_phase_connectivity(
        solid,
        shape,
        spacing_m,
        &masks,
        thresholds.unwrap_or(&[0.05, 0.5, 0.95]),
        one_cell_erosion,
        ("solid", "fluid"),
    )?;
    Ok(legacy_report(&report, def))
}

fn legacy_gate_issues(rows: &Value, context: &str) -> Vec<String> {
    let mut issues = Vec::new();
    for row in rows.as_array().into_iter().flatten() {
        let prefix =
            format!("{context} threshold {}", pyfmt::g(row["threshold"].as_f64().unwrap_or(f64::NAN)));
        let n = |k: &str| row[k].as_u64().unwrap_or(0);
        if row["fluid_through_path"] != Value::Bool(true) {
            issues.push(format!("{prefix}: no inlet-to-outlet path"));
        }
        if n("closed_fluid_components") > 0 {
            issues.push(format!("{prefix}: {} closed fluid component(s)", n("closed_fluid_components")));
        }
        if n("unauthorized_one_port_components") > 0 {
            issues.push(format!(
                "{prefix}: {} unauthorized one-port component(s)",
                n("unauthorized_one_port_components")
            ));
        }
        if n("disconnected_solid_components") > 0 {
            issues.push(format!(
                "{prefix}: {} floating solid component(s)",
                n("disconnected_solid_components")
            ));
        }
    }
    issues
}


pub fn require_phase_connectivity(
    solid: &[f64],
    shape: Shape3,
    spacing_m: &[f64],
    roles: &LegacyRoles<'_>,
    thresholds: Option<&[f64]>,
    require_one_cell_erosion: bool,
) -> PR<Value> {
    let mut report =
        audit_phase_connectivity(solid, shape, spacing_m, roles, thresholds, require_one_cell_erosion)?;
    let mut issues = legacy_gate_issues(&report["rows"], "base");
    if require_one_cell_erosion {
        issues.extend(legacy_gate_issues(&report["one_cell_erosion"]["rows"], "one-cell erosion"));
    }
    gate(&mut report, issues, require_one_cell_erosion)
}


pub fn phase_connectivity_component_fields(
    solid: &[f64],
    shape: Shape3,
    threshold: f64,
    roles: &LegacyRoles<'_>,
    one_cell_eroded: bool,
) -> PR<ComponentFields> {
    validated_phase(solid, shape)?;
    let (masks, _) = legacy_masks(shape, roles)?;
    let f = complementary_phase_component_fields(
        solid,
        shape,
        threshold,
        &masks,
        one_cell_eroded,
        ("solid", "fluid"),
    )?;
    let rename = |k: &str| -> String {
        match k {
            "qualified_phase_mask" => "qualified_solid_mask",
            "qualified_complement_mask" => "qualified_fluid_mask",
            "through_complement_mask" => "through_fluid_mask",
            "closed_complement_mask" => "closed_fluid_mask",
            "single_terminal_complement_mask" => "one_port_fluid_mask",
            "authorized_single_terminal_complement_mask" => "authorized_one_port_fluid_mask",
            "unauthorized_single_terminal_complement_mask" => "unauthorized_one_port_fluid_mask",
            "anchored_phase_mask" => "anchored_solid_mask",
            "unanchored_phase_mask" => "floating_solid_mask",
            "through_complement" => "through_fluid",
            "closed_complement" => "closed_fluid",
            "single_terminal_complement" => "one_port_fluid",
            "authorized_single_terminal_complement" => "authorized_one_port_fluid",
            "unauthorized_single_terminal_complement" => "unauthorized_one_port_fluid",
            "anchored_phase" => "anchored_solid",
            "unanchored_phase" => "floating_solid",
            other => other,
        }
        .to_string()
    };
    Ok(ComponentFields {
        masks: f.masks.into_iter().map(|(k, v)| (rename(&k), v)).collect(),
        component_ids: f.component_ids.into_iter().map(|(k, v)| (rename(&k), v)).collect(),
        ..f
    })
}
