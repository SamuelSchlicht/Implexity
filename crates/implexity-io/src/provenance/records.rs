// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use implexity_core::json::{DumpOptions, dumps};
use implexity_core::py_repr::repr_str;
use implexity_core::pyobj::{py_eq, py_str, repr, truthy};

use super::blocks::{
    ObjectiveInputs, best_by_l, case_block, design_block, loss_trajectory, objective_block, steer_events,
};
use super::{
    ProvResult, RECORD_VERSION, SCHEMA, environment, finalise, get, jsonable_f64, list, need_float, need_int,
    obj, or,
};

fn item(name: &str, status: &str, detail: &str, measured: Value, source: &str) -> Value {
    let measured = if truthy(&measured) { measured } else { json!({}) };
    json!({"name": name, "status": status, "detail": detail, "measured": measured, "source": source})
}


#[allow(clippy::too_many_lines)]
pub fn fidelity_body(stats: &Value, tolerance_mm: &Value, formats: &[String]) -> ProvResult<Vec<Value>> {
    let st = if truthy(stats) { stats.clone() } else { json!({}) };
    let ch = or(get(&st, "chord"), &Value::Null).clone();
    let vol = or(get(&st, "volume"), &Value::Null).clone();
    let s = |k: &str| get(&st, k).clone();
    let c = |k: &str| get(&ch, k).clone();
    let v = |k: &str| get(&vol, k).clone();
    let mut out = Vec::new();

    let (h, hg) = (s("spacing_mm"), s("geff_spacing_mm"));
    let exact_geff = !h.is_null()
        && !hg.is_null()
        && (need_float(&h, "spacing_mm")? - need_float(&hg, "geff_spacing_mm")?).abs() <= 1e-12;
    out.push(item(
        "geff sampling",
        if exact_geff { "exact" } else { "approximated" },
        if exact_geff {
            "the export spacing IS the design's own analysis spacing, so the neighbourhood-coupled scalar geff is read at the grid it was computed on and no interpolation happens at all"
        } else {
            "geff is evaluated ONCE on the design's own analysis grid and interpolated to the export grid; it is never re-blurred at the export spacing, because its blur radius is an integer and re-blurring gives a different solid at every tolerance (5.32 % of volume, measured). The level set f is NEVER coarsened."
        },
        json!({"export_spacing_mm": h, "geff_spacing_mm": hg, "geff_blur_radius": s("geff_blur_radius")}),
        "bodyexport.extract",
    ));

    out.push(item(
        "sample spacing snapping",
        if py_eq(&s("spacing_requested_mm"), &h) { "exact" } else { "approximated" },
        "the spacing is snapped to an integer division of the design's own analysis spacing so that geff's integer stride and integer blur radius land on the design's own values. Measured on the box case: the exported volume spread across three tolerances was 5.32 % re-blurring at the export spacing, 2.38 % with an unsnapped stride, and 0.5 % snapped.",
        json!({"spacing_requested_mm": s("spacing_requested_mm"), "spacing_mm": h, "snapped_to": s("spacing_snapped_to"),
               "grid": s("grid"), "samples": s("samples")}),
        "bodyexport.snap_spacing",
    ));

    let achieved = c("area_weighted_mm");
    let method = c("method");
    let mut detail = if truthy(&method) { py_str(&method) } else { String::new() };
    if truthy(&c("not_measurable")) {
        detail.push_str(" | ");
        detail.push_str(&py_str(&c("not_measurable")));
    }
    let honoured = if achieved.is_null() {
        Value::Null
    } else {
        json!(need_float(&achieved, "area_weighted_mm")? <= need_float(tolerance_mm, "tolerance_mm")?)
    };
    out.push(item(
        "chord error against the level set",
        if achieved.is_null() { "not measurable" } else { "measured" },
        &detail,
        json!({"requested_mm": tolerance_mm, "achieved_area_weighted_mm": achieved,
               "rms_mm": c("rms_mm"), "p99_mm": c("p99_mm"), "max_mm": c("max_mm"), "honoured": honoured,
               "chord_source": c("chord_source"), "measured_on_area_fraction": c("measured_on_area_fraction"),
               "cap_area_fraction": c("cap_area_fraction"), "lattice_area_fraction": c("lattice_area_fraction"),
               "mask_area_fraction": c("mask_area_fraction"),
               "lattice_q_minus_tau_area_weighted_mm": c("lattice_q_minus_tau_area_weighted_mm")}),
        "bodyexport._chord_report",
    ));

    out.push(item(
        "the cap crease",
        "approximated",
        "max() of two signed distances is the exact intersection as a SET, so the body is the right body; it is not a distance function near the crease where the two surfaces meet, and marching cubes rounds that corner over about one cell. This is the area affected -- compare it against the declared tolerance rather than assuming it negligible.",
        json!({"crease_area_fraction": c("crease_area_fraction"), "crease_nodes": s("crease_nodes"), "spacing_mm": h}),
        "bodyexport.extract",
    ));

    out.push(item(
        "volume closure",
        "measured",
        "levelset is the set the body is DEFINED as, so mesh-vs-levelset is the extraction's own round trip and must be small. density_integral is the physics volume fraction and equals it only for a sharp projection: at an intermediate continuation stage the density field is genuinely grey and the two differ BY CONSTRUCTION, which is a statement about the design, not about the mesh.",
        json!({"mesh_mm3": v("mesh_mm3"), "levelset_mm3": v("levelset_mm3"),
               "density_integral_mm3": v("density_integral_mm3"), "domain_mm3": v("domain_mm3"),
               "mesh_vs_levelset_rel": v("mesh_vs_levelset_rel"), "mesh_vs_density_rel": v("mesh_vs_density_rel"),
               "volume_fraction_mesh": v("volume_fraction_mesh"), "volume_fraction_density": v("volume_fraction_density")}),
        "bodyexport._volume_report",
    ));

    out.push(item(
        "the cap",
        if truthy(&s("cap_verification")) { "measured" } else { "declared" },
        "which boundary the body is capped against, and the cap's own check",
        json!({"cap": s("cap"), "verification": s("cap_verification"), "deadband_nodes": s("deadband_nodes"),
               "deadband_mm": s("deadband_mm")}),
        "bodyexport.extract",
    ));

    if formats.iter().any(|f| f == "3mf") {
        out.push(item(
            "3MF vertex representation",
            "approximated",
            "this is a CORE 3MF <mesh>, not the Volumetric/Implicit extension: the control fields are not in it at all, so the 16-bit quantisation FIDELITY.md quotes for the implicit 3MF does not apply. What does apply is that lib3mf writes vertex positions as decimal text; the deviation this costs is re-measured on the written file and reported in stamped['3mf'].reread.",
            json!({}),
            "implexity.provenance.stamp",
        ));
    }

    out.push(item(
        "the physics in the design",
        "not asserted",
        "nothing in this record says whether the physics in the design is right; the physics package's own validation record carries those caveats, and they are unaffected by anything this export does.",
        json!({}),
        "docs/FIDELITY.md section 3",
    ));
    Ok(out)
}

#[must_use]
pub fn fidelity_optimisation(summary: &Value, case_doc: &Value, extra: &[Value]) -> Vec<Value> {
    let s = if truthy(summary) { summary.clone() } else { json!({}) };
    let doc = if truthy(case_doc) { case_doc.clone() } else { json!({}) };
    let mut out = vec![item(
        "analysis grid",
        "approximated",
        "the physics was solved at this element count, not at the case's declared one if they differ. Every field quantity in this record is a value AT THIS GRID.",
        json!({"grid": or(get(&s, "grid"), get(&doc, "grid")), "case_declared_grid": get(&doc, "grid"),
               "h_mm": or(get(&s, "h_mm"), get(&doc, "h_mm")), "confined": get(&s, "confined")}),
        "the physics package's problem builder",
    )];
    out.extend(extra.iter().map(|i| Value::Object(obj(i))));
    out.push(item(
        "the physics in the design",
        "not asserted",
        "nothing in this record says whether the physics model is right; it says what was solved, at what grid, and what came out.",
        json!({}),
        "docs/FIDELITY.md section 3",
    ));
    out
}

#[derive(Debug, Clone, Copy)]
pub struct BodyRecordInputs<'a> {
    pub design_params: &'a BTreeMap<String, ArrayD<f64>>,
    pub design_meta: &'a Value,
    pub design_version: i64,
    pub case_doc: &'a Value,
    pub report: &'a Value,
    pub request: &'a Value,
    pub service_version: Option<&'a str>,
    pub backend: Option<&'a str>,
    pub h_design_mm: Option<f64>,
    pub domain: &'a Value,
    pub links: &'a Value,
    pub bbox_mm: &'a Value,
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', std::path::MAIN_SEPARATOR]).next().unwrap_or(p)
}


#[allow(clippy::too_many_lines)]
pub fn body_record(inp: &BodyRecordInputs<'_>) -> ProvResult<Value> {
    let rep = if truthy(inp.report) { inp.report.clone() } else { json!({}) };
    let st = or(get(&rep, "extraction"), &Value::Null).clone();
    let acc = or(get(&rep, "accepted"), &Value::Null).clone();
    let ch = or(get(&st, "chord"), &Value::Null).clone();
    let req = Value::Object(obj(or(or(inp.request, get(&rep, "requested")), &Value::Null)));
    let formats: Vec<String> =
        list(or(get(&req, "format"), get(&req, "formats"))).iter().map(py_str).collect();
    let tol = get(&req, "tolerance_mm").clone();
    let s = |k: &str| get(&st, k).clone();
    let a = |k: &str| get(&acc, k).clone();

    let geometry = json!({
        "triangles": s("triangles"), "vertices": s("vertices"), "area_mm2": s("area_mm2"),
        "signed_volume_mm3": s("signed_volume_mm3"), "bbox_mm": inp.bbox_mm, "watertight": s("watertight"),
        "watertight_components": {
            "boundary_edges": s("topology_boundary_edges"),
            "nonmanifold_edges": s("topology_nonmanifold_edges"),
            "orientation_consistent": get(or(get(&st, "orientation"), &Value::Null), "consistent"),
            "signed_volume_positive": a("signed_volume_positive"),
            "definition": "watertight means all four at once: no boundary edge, no non-manifold edge, consistent orientation, positive enclosed volume",
        },
        "components": s("component_count"),
        "components_negative_volume": s("components_negative_volume"),
        "components_note": s("components_note"),
        "genus": s("topology_genus"),
        "euler_characteristic": s("topology_chi"),
        "degenerate_triangles_dropped": s("degenerate_triangles_dropped"),
        "winding_flipped": s("winding_flipped"),
        "units": "millimetre",
    });

    let mut artefacts = Map::new();
    for fmt in &formats {
        let prov_files = get(or(get(&rep, "provenance"), &Value::Null), "files");
        let f = or(
            or(get(or(prov_files, &Value::Null), fmt), get(or(get(&rep, "files"), &Value::Null), fmt)),
            &Value::Null,
        )
        .clone();
        let path = get(&f, "path");
        let file = if truthy(path) {
            let b = basename(&py_str(path)).to_string();
            if b.is_empty() { Value::Null } else { json!(b) }
        } else {
            Value::Null
        };
        let achieved = get(&ch, "area_weighted_mm").clone();
        let honoured = if achieved.is_null() || tol.is_null() {
            Value::Null
        } else {
            json!(need_float(&achieved, "area_weighted_mm")? <= need_float(&tol, "tolerance_mm")?)
        };
        let mut entry = json!({
            "format": fmt,
            "file": file,
            "bytes_as_written": get(&f, "bytes"),
            "sha256_as_written": null,
            "geometry": geometry,
            "tolerance": {
                "requested_mm": tol, "achieved_area_weighted_mm": achieved, "definition": get(&ch, "method"),
                "honoured": honoured,
                "note": "the ACHIEVED chord is measured on the mesh that was written, not predicted from the requested one",
            },
            "extraction": {
                "spacing_mm": s("spacing_mm"), "spacing_requested_mm": s("spacing_requested_mm"),
                "spacing_snapped_to": s("spacing_snapped_to"), "grid": s("grid"), "samples": s("samples"),
                "origin_mm": s("origin_mm"), "cap": s("cap"), "cap_verification": s("cap_verification"),
                "components_filter": get(&req, "components"),
            },
            "volume": s("volume"),
        });
        if fmt == "step" {
            let sr = or(get(&rep, "step"), &Value::Null).clone();
            let header =
                if sr.is_object() || sr.is_null() { get(&sr, "header").clone() } else { Value::Null };
            if let Value::Object(m) = &mut entry {
                m.insert(
                    "brep".into(),
                    json!({
                        "schema": a("step_schema"), "solids": a("step_solids"), "faces": a("step_faces"),
                        "faces_unmerged": a("step_faces_unmerged"), "valid_solid": a("step_valid_solid"),
                        "closed_shell": a("step_closed_shell"), "volume_mm3": a("step_volume_mm3"),
                        "volume_vs_mesh_rel": a("step_volume_vs_mesh_rel"), "roundtrip_valid": a("step_roundtrip_valid"),
                        "header": header, "merged": get(&f, "merged"),
                        "note": "Face count, volume and validity are obtained by the STEP round-trip checker from the written file",
                    }),
                );
            }
        }
        artefacts.insert(fmt.clone(), entry);
    }

    let calib = or(get(&rep, "calibration"), &Value::Null).clone();
    let has_objective = truthy(get(or(inp.case_doc, &Value::Null), "objective"));
    let terms = objective_block(inp.case_doc, &ObjectiveInputs::default())?["terms"].clone();
    let mut rec = json!({
        "schema": SCHEMA,
        "record_version": RECORD_VERSION,
        "kind": "body",
        "title": "watertight solid body, exported from an implicit lattice design",
        "environment": environment(inp.service_version, inp.backend),
        "request": req,
        "artefacts": artefacts,
        "design": design_block(inp.design_params, inp.design_meta, inp.design_version, inp.h_design_mm)?,
        "case": case_block(inp.case_doc, inp.domain, true)?,
        "objective": {
            "source": if has_objective {
                "the case document names an objective; a BODY is a geometry export and does not evaluate it"
            } else {
                "no objective block in the case document"
            },
            "terms": terms,
            "evaluated_here": false,
            "note": "the objective is recorded because it is part of what this design was made UNDER; this artefact is a geometry export and evaluated none of it. An optimisation record carries the term VALUES.",
        },
        "run": null,
        "calibration": {
            "law": get(&calib, "law"), "chord_exponent": get(&calib, "chord_exponent"),
            "triangle_exponent": get(&calib, "triangle_exponent"), "levels": get(&calib, "levels"),
            "skipped": calib.get("skipped").cloned().unwrap_or(json!(false)),
            "note": "the spacing was NOT guessed: two cheap coarse extractions on THIS design fit the exponents, and the achieved chord was re-measured on the mesh that was written",
        },
        "fidelity": fidelity_body(&st, &tol, &formats)?,
        "links": obj(inp.links),
        "warnings": [],
        "statement": format!(
            "This body is the rho >= 0.5 iso-surface of the named design at the named continuation stage, intersected with the named cap, extracted at {} mm and measured on the mesh that was written.",
            py_str(&s("spacing_mm"))
        ),
    });
    hoist_warnings(&mut rec);
    finalise(&mut rec);
    Ok(rec)
}

#[derive(Debug, Clone, Copy)]
pub struct FieldExportInputs<'a> {
    pub design_params: &'a BTreeMap<String, ArrayD<f64>>,
    pub design_meta: &'a Value,
    pub design_version: i64,
    pub request: &'a Value,
    pub sample: &'a Value,
    pub service_version: Option<&'a str>,
    pub backend: Option<&'a str>,
    pub h_design_mm: Option<f64>,
    pub links: &'a Value,
}


pub fn field_export_record(inp: &FieldExportInputs<'_>) -> ProvResult<Value> {
    let req = Value::Object(obj(inp.request));
    let smp = Value::Object(obj(inp.sample));
    let field = {
        let f = or(or(get(&req, "field"), get(&smp, "field")), &Value::Null);
        if truthy(f) { py_str(f) } else { String::new() }
    };
    let representation = {
        let r = get(&req, "representation");
        if truthy(r) { py_str(r) } else { "raw".into() }
    };
    let m = |k: &str| get(&smp, k).clone();
    let artefact = json!({
        "format": "vdb",
        "written_by": "external exchange tool",
        "field": field,
        "representation": representation,
        "design_version": inp.design_version,
        "shape": list(&m("shape")),
        "h_mm": m("h_mm"),
        "origin_mm": list(&m("origin_mm")),
        "planes": m("planes"),
        "range_mm": list(&m("range_mm")),
        "volume_fraction": m("volume_fraction"),
        "lod": obj(&m("lod")),
        "wire_field": m("wire_field"),
        "wire_units": obj(&m("wire_units")),
        "measurements": {
            "service": "field semantics, design version and each source plane served by /v1/section",
            "client": "3-D stacking, range/volume summary and wire-unit measurement where present",
        },
        "note": "The record identifies the sampled design and sampling contract. The final VDB byte hash can only be added after the external writer has embedded this record; that post-write stamp is not part of record_id.",
    });
    let mut rec = json!({
        "schema": SCHEMA,
        "record_version": RECORD_VERSION,
        "kind": "field",
        "title": "sampled signed/occupancy field exported for an external implicit modeller",
        "environment": environment(inp.service_version, inp.backend),
        "request": req,
        "artefacts": {"field_vdb": artefact},
        "design": design_block(inp.design_params, inp.design_meta, inp.design_version, inp.h_design_mm)?,
        "case": null,
        "objective": {"source": "not evaluated by a field export", "terms": [], "evaluated_here": false},
        "run": null,
        "calibration": null,
        "fidelity": {
            "sampling_spacing_mm": m("h_mm"),
            "lod": obj(&m("lod")),
            "design_grid": list(get(inp.design_meta, "design_grid")),
            "h_design_mm": inp.h_design_mm.map(jsonable_f64),
            "note": "This is a sampled field handoff.  Its engineering fidelity is bounded by the named LOD and sampling spacing; no body tolerance or part-quality claim is made.",
        },
        "links": obj(inp.links),
        "warnings": [],
        "statement": format!(
            "This record binds an ad-hoc sampled field export to design version {} and field {}.  The implexity service did not write the VDB file.",
            inp.design_version,
            repr_str(&field)
        ),
    });
    hoist_warnings(&mut rec);
    if truthy(&m("wire_units"))
        && let Some(Value::Array(w)) = rec.get_mut("warnings")
    {
        w.push(warn(
            "wire_units_measured_by_client",
            "low",
            "the wire-unit conversion was measured by the external client against tau values served by implexity; the measurement and its residual are recorded under artefacts.field_vdb.wire_units",
            "artefacts.field_vdb.wire_units",
        ));
    }
    finalise(&mut rec);
    Ok(rec)
}

#[derive(Debug, Clone, Default)]
pub struct OptimisationInputs<'a> {
    pub job_info: Value,
    pub case_start: Value,
    pub case_final: Value,
    pub summary: Value,
    pub timeline: Value,
    pub rows: Value,
    pub design_params: Option<&'a BTreeMap<String, ArrayD<f64>>>,
    pub design_meta: Value,
    pub design_version: Value,
    pub service_version: Option<&'a str>,
    pub backend: Option<&'a str>,
    pub domain: Value,
    pub links: Value,
    pub accepted: Value,
    pub artefacts: Value,
    pub objective: Value,
    pub fidelity_items: Vec<Value>,
    pub driver: Value,
    pub replay: Value,
}


#[allow(clippy::too_many_lines)]
pub fn optimisation_record(inp: &OptimisationInputs<'_>) -> ProvResult<Value> {
    let j = Value::Object(obj(&inp.job_info));
    let s = Value::Object(obj(&inp.summary));
    let rows = list(or(or(&inp.rows, get(&j, "history")), get(&s, "history")));
    let tl = list(or(&inp.timeline, get(&s, "bc_timeline")));
    let events = steer_events(&tl, &rows)?;
    let applied: Vec<&Value> = events.iter().filter(|e| truthy(get(e, "applied"))).collect();
    let traj = loss_trajectory(&rows, &events)?;
    let last_row = rows.last().cloned().unwrap_or(Value::Null);
    let best_row = best_by_l(&rows)?.cloned().unwrap_or(Value::Null);
    let refs = tl
        .iter()
        .rev()
        .find(|e| truthy(get(e, "refs_after")))
        .map_or_else(|| json!({}), |e| Value::Object(obj(get(e, "refs_after"))));
    let case_now = or(&inp.case_final, &inp.case_start).clone();
    let jg = |k: &str| get(&j, k).clone();
    let sg = |k: &str| get(&s, k).clone();

    let design = match (inp.design_params, inp.design_meta.is_null()) {
        (Some(p), false) => {
            design_block(p, &inp.design_meta, need_int(&inp.design_version, "design_version")?, None)?
        }
        _ => Value::Null,
    };
    let objective = if inp.objective.is_null() {
        let oi = ObjectiveInputs {
            objective_meta: sg("objective_meta"),
            last_row: last_row.clone(),
            refs,
            l0: sg("L0"),
            ..ObjectiveInputs::default()
        };
        objective_block(&case_now, &oi)?
    } else {
        inp.objective.clone()
    };
    let case_start =
        if inp.case_start.is_null() { Value::Null } else { case_block(&inp.case_start, &inp.domain, true)? };
    let accepted = obj(&inp.accepted);
    let at_iterations: Vec<Value> = applied.iter().map(|e| get(e, "at_iteration").clone()).collect();
    let statement = if applied.is_empty() {
        "This design is the best iterate of ONE optimisation problem.".to_string()
    } else {
        format!(
            "This design is NOT the optimum of any single problem. It is the endpoint of a TRAJECTORY of problems: the boundary conditions were changed {} time(s) while the optimiser ran, at iteration(s) {}. Its loss curve spans {} different objectives and its two ends are not comparable. Reproducing it means replaying that timeline, not re-solving the final case.",
            applied.len(),
            repr(&Value::Array(at_iterations)),
            applied.len() + 1
        )
    };
    let refused = events.len() - applied.len();
    let mut rec = json!({
        "schema": SCHEMA,
        "record_version": RECORD_VERSION,
        "kind": "optimisation",
        "title": "optimisation run: what problem(s) produced this design",
        "environment": environment(inp.service_version, inp.backend),
        "request": {"job_id": jg("job_id"), "channel": jg("channel"), "seq": jg("seq"), "status": jg("status"),
                    "start_design": jg("design")},
        "artefacts": obj(&inp.artefacts),
        "design": design,
        "case": case_block(&case_now, &inp.domain, true)?,
        "case_start": case_start,
        "objective": objective,
        "optimiser": {
            "grid": or(get(&j, "grid"), get(&s, "grid")),
            "h_mm": sg("h_mm"),
            "iters_per_stage": or(get(&j, "iters"), get(&s, "iters_per_stage")),
            "stages": or(get(&j, "stages"), get(&s, "stages")),
            "total_iters": jg("total_iters"),
            "lr": jg("lr"),
            "momentum_default": or(get(&j, "momentum"), get(&s, "momentum_default")),
            "live_every": jg("live_every"),
            "steerable": jg("steerable"),
            "confined": or(get(&j, "confined"), get(&s, "confined")),
            "start_design": jg("design"),
            "driver": inp.driver,
            "replay": inp.replay,
            "replay_note": "replaying the TIMELINE is the only way to reproduce a steered design -- NOT re-solving the final case, which gives a different design, because the iterate that entered the final regime was produced by the earlier ones",
        },
        "run": {
            "status": jg("status"),
            "iterations": rows.len(),
            "loss": traj,
            "best_row": best_row,
            "best_regime": get(&best_row, "regime"),
            "last_row": last_row,
            "regimes": applied.len() + 1,
            "steered": !applied.is_empty(),
            "steers": events,
            "steers_applied": applied.len(),
            "steers_refused": refused,
            "timeline": jg("timeline"),
            "projection_fallbacks": sg("projection_fallbacks"),
            "worst_abs_dV": sg("worst_abs_dV"),
            "rho_outside_max_final": sg("rho_outside_max_final"),
            "V_final": sg("V_final"),
            "peak_rss_mb": sg("peak_rss_mb"),
            "elapsed_s": sg("elapsed_s"),
            "error": jg("error"),
        },
        "fidelity": fidelity_optimisation(&s, &case_now, &inp.fidelity_items),
        "accepted": if accepted.is_empty() { Value::Null } else { Value::Object(accepted) },
        "links": obj(&inp.links),
        "warnings": [],
        "statement": statement,
    });
    hoist_warnings(&mut rec);
    finalise(&mut rec);
    Ok(rec)
}

fn warn(code: &str, severity: &str, text: &str, where_: &str) -> Value {
    json!({"code": code, "severity": severity, "text": text, "where": [where_]})
}

pub fn hoist_warnings(rec: &mut Value) {
    let mut w: Vec<Value> = Vec::new();
    let d = or(get(rec, "design"), &Value::Null).clone();
    if truthy(get(&d, "continuation_assumed")) {
        w.push(warn(
            "continuation_assumed",
            "high",
            &py_str(get(&d, "continuation_warning")),
            "design.continuation",
        ));
    }
    let run = or(get(rec, "run"), &Value::Null).clone();
    for e in list(get(&run, "steers")) {
        if !truthy(get(&e, "applied")) {
            continue;
        }
        let cd = or(get(&e, "calibration_discontinuity"), &Value::Null).clone();
        let text_of = |k: &str| cd.get(k).map_or_else(String::new, py_str);
        w.push(warn(
            "steer_calibration_discontinuity",
            "high",
            &format!("{} {}", text_of("statement"), text_of("severity")),
            &format!("run.steers[seq={}]", py_str(get(&e, "seq"))),
        ));
    }
    if truthy(&run) {
        let loss = or(get(&run, "loss"), &Value::Null);
        let comparable = loss.get("comparable").cloned().unwrap_or(json!(true));
        if !truthy(&comparable) {
            w.push(warn("loss_not_comparable", "high", &py_str(get(get(&run, "loss"), "note")), "run.loss"));
        }
    }
    if let Some(Value::Object(arts)) = rec.get("artefacts") {
        for (fmt, a) in arts {
            let t = or(get(or(a, &Value::Null), "tolerance"), &Value::Null);
            if get(t, "honoured") == &Value::Bool(false) {
                w.push(warn(
                    "tolerance_not_honoured",
                    "medium",
                    &format!(
                        "the requested tolerance of {} mm was NOT achieved: the measured area-weighted chord is {} mm",
                        py_str(get(t, "requested_mm")),
                        py_str(get(t, "achieved_area_weighted_mm"))
                    ),
                    &format!("artefacts.{fmt}.tolerance"),
                ));
            }
            let g = or(get(or(a, &Value::Null), "geometry"), &Value::Null);
            if get(g, "watertight") == &Value::Bool(false) {
                w.push(warn(
                    "not_watertight",
                    "high",
                    &format!(
                        "this body is NOT watertight: {}",
                        dumps(get(g, "watertight_components"), &DumpOptions::default())
                    ),
                    &format!("artefacts.{fmt}.geometry"),
                ));
            }
        }
    }
    let mut merged: Vec<Value> = Vec::new();
    for x in w {
        let key = (get(&x, "code").clone(), get(&x, "text").clone());
        if let Some(existing) =
            merged.iter_mut().find(|m| get(m, "code") == &key.0 && get(m, "text") == &key.1)
        {
            if let (Some(Value::Array(dst)), Some(Value::Array(src))) =
                (existing.get_mut("where"), x.get("where"))
            {
                dst.extend(src.iter().cloned());
            }
        } else {
            merged.push(x);
        }
    }
    if let Value::Object(m) = rec {
        m.insert("warnings".into(), Value::Array(merged));
    }
}

