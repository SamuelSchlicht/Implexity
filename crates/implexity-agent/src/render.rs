// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END

use std::time::Instant;

use base64::Engine as _;
use implexity_mesh::grid::Field3;
use implexity_mesh::model_view::{evaluate_model_blocks, registered_field_sampler};
use implexity_mesh::raster::{self, Background, ChartEvent, ScalarSectionOptions, SectionPalette};
use implexity_render::render3d::{
    self, CameraRequest, CapColour, ColourSpec, RasterOptions, RenderMesh, SpacingHint,
    VertexColouring,
};
use serde_json::{Map, Value, json};

use crate::error::{AgentError, AgentResult};
use crate::host::{AgentModel, HostOp};
use crate::pyval::{is_hex, py_str};
use crate::runtime::{AgentManager, field};
use crate::views::{
    RENDER_SECTION_MAX_PIXELS, RENDER_SECTION_MAX_PNG_BYTES, normalise_bbox,
    normalise_render_section_request, render_section_dimensions,
};

fn contract(e: impl std::fmt::Display) -> AgentError {
    AgentError::contract(e.to_string())
}

struct RetainedRenderField {
    id: String,
    field: String,
    array: implexity_render::artifact::StoredArray,
    read: Value,
}

impl implexity_render::artifact::ResultArtifactStore for RetainedRenderField {
    fn inspect(
        &self,
        id: &str,
    ) -> Result<(Value, std::collections::BTreeSet<String>), implexity_render::RenderError> {
        if id != self.id {
            return Err(implexity_render::RenderError::Invalid(
                "retained render identity differs".into(),
            ));
        }
        Ok((
            json!({"identities":self.read,"metadata":{"fields":{self.field.clone():self.read["metadata"]}}}),
            [self.field.clone()].into_iter().collect(),
        ))
    }

    fn read_arrays(
        &self,
        id: &str,
        names: &std::collections::BTreeSet<String>,
    ) -> Result<
        std::collections::BTreeMap<String, implexity_render::artifact::StoredArray>,
        implexity_render::RenderError,
    > {
        if id != self.id || names.iter().any(|name| name != &self.field) {
            return Err(implexity_render::RenderError::Invalid(
                "only the selected retained field is available".into(),
            ));
        }
        Ok([(self.field.clone(), self.array.clone())]
            .into_iter()
            .collect())
    }

    fn registration_from_wire(
        &self,
        wire: &Value,
    ) -> Result<implexity_render::artifact::Registration, implexity_render::RenderError> {
        let grid = implexity_geometry::field_registration::GridRegistration::from_wire(wire)
            .map_err(|e| implexity_render::RenderError::Invalid(e.to_string()))?;
        Ok(implexity_render::artifact::Registration {
            shape: grid.shape,
            origin: grid.origin,
            matrix: grid.basis,
            centering: grid.centering,
            axis_order: grid.axis_order,
            frame: grid.frame,
            wire: wire.clone(),
        })
    }
}

fn inline_image(png: &[u8], width: usize, height: usize) -> Value {
    json!({"schema": "implexity-inline-image/1", "mime_type": "image/png",
           "data_base64": base64::engine::general_purpose::STANDARD.encode(png),
           "bytes": png.len(), "sha256": implexity_io::digest::sha256_hex(png),
           "width_px": width, "height_px": height})
}

fn background(name: &str) -> Background {
    if name == "white" {
        Background::White
    } else {
        Background::Dark
    }
}

fn content_of(status: &Value) -> Value {
    status.get("content_id").cloned().unwrap_or(Value::Null)
}

fn bbox_of(v: &Value) -> Option<[[f64; 3]; 2]> {
    let rows = v.as_array().filter(|r| r.len() == 2)?;
    let row = |r: &Value| -> Option<[f64; 3]> {
        let a = r.as_array().filter(|a| a.len() == 3)?;
        Some([a[0].as_f64()?, a[1].as_f64()?, a[2].as_f64()?])
    };
    Some([row(&rows[0])?, row(&rows[1])?])
}

fn time_record(timing: &[(&str, f64)]) -> Value {
    Value::Object(
        timing
            .iter()
            .map(|(k, v)| {
                (
                    (*k).to_owned(),
                    json!((v * 1000.0).round_ties_even() / 1000.0),
                )
            })
            .collect(),
    )
}

fn render_identity_ok(r: &Map<String, Value>) -> bool {
    let keys = [
        "schema",
        "job_id",
        "epoch",
        "solve_id",
        "design_state_id",
        "checkpoint_sha256",
        "model_content_id",
    ];
    let text = |k: &str| {
        r.get(k)
            .filter(|v| crate::pyval::truthy(v))
            .map(py_str)
            .unwrap_or_default()
    };
    r.len() == keys.len()
        && keys.iter().all(|k| r.contains_key(*k))
        && r.get("schema") == Some(&json!("implexity-optimization-render-identity/1"))
        && is_hex(&text("job_id"), 12)
        && r.get("epoch")
            .is_some_and(|e| crate::pyval::is_int(e) && e.as_i64().is_some_and(|x| x >= 0))
        && is_hex(&text("solve_id"), 64)
        && text("design_state_id")
            .strip_prefix("design-")
            .is_some_and(|h| is_hex(h, 64))
        && is_hex(&text("checkpoint_sha256"), 64)
        && is_hex(&text("model_content_id"), 16)
}

impl AgentManager {
    pub(crate) fn require_model(&self) -> AgentResult<std::sync::Arc<dyn AgentModel>> {
        self.host
            .model()
            .ok_or_else(|| HostOp::InspectParameters.unavailable())
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn render_source(
        &self,
        source: &Value,
        status: &Value,
    ) -> AgentResult<(Map<String, Value>, String)> {
        let mut identity = Map::new();
        identity.insert("kind".into(), json!("current_model"));
        identity.insert(
            "structure_id".into(),
            status.get("structure_id").cloned().unwrap_or(Value::Null),
        );
        identity.insert("content_id".into(), content_of(status));
        identity.insert("job_id".into(), Value::Null);
        identity.insert("epoch".into(), Value::Null);
        identity.insert("design_state_id".into(), Value::Null);
        let requested = source
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let live = status.get("live_optimisation").filter(|l| l.is_object());
        if requested == "current_model" && live.is_none() {
            let product_meta = status
                .get("document")
                .and_then(|d| d.get("meta"))
                .and_then(|m| m.get("implexity"))
                .filter(|m| m.is_object());
            if let Some(ri) = product_meta
                .and_then(|m| m.get("render_identity"))
                .filter(|v| !v.is_null())
            {
                let Some(r) = ri.as_object().filter(|r| render_identity_ok(r)) else {
                    return Err(AgentError::contract(
                        "the content-bound optimization render identity is malformed",
                    ));
                };
                let content = status
                    .get("content_id")
                    .filter(|v| crate::pyval::truthy(v))
                    .map(py_str)
                    .unwrap_or_default();
                if py_str(&r["model_content_id"]) != content {
                    return Err(AgentError::contract(
                        "optimization render identity does not match the current model parameter content",
                    ));
                }
                identity.insert(
                    "provenance_kind".into(),
                    json!("detached_optimization_epoch"),
                );
                for key in [
                    "job_id",
                    "epoch",
                    "solve_id",
                    "design_state_id",
                    "checkpoint_sha256",
                    "model_content_id",
                ] {
                    identity.insert(key.into(), r[key].clone());
                }
                identity.insert(
                    "identity_source".into(),
                    json!("content_bound_model_metadata"),
                );
            }
            let declared = product_meta
                .and_then(|m| m.get("truth_status"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("authored_model");
            return Ok((identity, declared.to_owned()));
        }
        let (job_id, epoch) = if requested == "current_optimization_state" {
            let job_id = source.get("job_id").cloned().unwrap_or(Value::Null);
            if live.and_then(|l| l.get("job_id")) != Some(&job_id) {
                return Err(AgentError::contract(
                    "the requested optimization job is not the current in-memory model state",
                ));
            }
            (
                py_str(&job_id),
                source.get("epoch").cloned().unwrap_or(Value::Null),
            )
        } else {
            let job_id = live
                .and_then(|l| l.get("job_id"))
                .filter(|v| crate::pyval::truthy(v))
                .map(py_str)
                .unwrap_or_default();
            if job_id.is_empty() {
                return Ok((identity, "undeclared".into()));
            }
            (job_id, json!("current"))
        };
        let info = self.host.call(HostOp::JobInfo(job_id.clone()))?;
        let rows: Vec<&Value> = info
            .get("history")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let content = content_of(status);
        let iteration = |row: &Value| {
            row.get("i")
                .or_else(|| row.get("iteration"))
                .cloned()
                .unwrap_or(Value::Null)
        };
        let mut matching: Vec<&Value> = rows
            .into_iter()
            .filter(|r| r.is_object() && r.get("document_content_id") == Some(&content))
            .collect();
        if epoch != "current" {
            matching.retain(|r| iteration(r) == epoch);
        }
        let Some(row) = matching.last() else {
            if requested == "current_model" {
                identity.insert("job_id".into(), json!(job_id));
                identity.insert(
                    "job_status".into(),
                    info.get("status").cloned().unwrap_or(Value::Null),
                );
                return Ok((identity, "pre_run_model".into()));
            }
            return Err(AgentError::contract(
                "the requested optimization epoch is not the exact iterate currently applied to the model",
            ));
        };
        let mut design_state_id = row
            .get("design_state_id")
            .filter(|v| crate::pyval::truthy(v))
            .cloned();
        if design_state_id.is_none() {
            design_state_id = row
                .get("push")
                .and_then(|p| p.get("verified_design_state_id"))
                .cloned();
        }
        let Some(design_state_id) =
            design_state_id.filter(|d| d.as_str().is_some_and(|s| !s.is_empty()))
        else {
            return Err(AgentError::contract(
                "the current optimization state has no verified design identity",
            ));
        };
        let truth = row
            .get("diagnostics")
            .and_then(|d| d.get("truth_status"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("undeclared")
            .to_owned();
        identity.insert("kind".into(), json!("current_optimization_state"));
        identity.insert("job_id".into(), json!(job_id));
        identity.insert("epoch".into(), iteration(row));
        identity.insert("design_state_id".into(), design_state_id);
        identity.insert(
            "job_status".into(),
            info.get("status").cloned().unwrap_or(Value::Null),
        );
        Ok((identity, truth))
    }

    fn current_problem(&self) -> Value {
        self.state_without_plan()
            .ok()
            .and_then(|s| s.get("engineering_problem").cloned())
            .unwrap_or(Value::Null)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn render_section(&self, payload: &Map<String, Value>) -> AgentResult<Value> {
        let request = normalise_render_section_request(&Value::Object(payload.clone()))?;
        if request["source"]["kind"] == "result_artifact" {
            let artifact = request["source"]["artifact_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let store = self.host.result_store(&artifact)?;
            let resolver = |r: &Value, bbox: [[f64; 3]; 2]| {
                let m = r.as_object().cloned().unwrap_or_default();
                render_section_dimensions(&m, &bbox)
                    .map_err(|e| implexity_render::RenderError::Invalid(e.message().to_owned()))
            };
            return implexity_render::artifact::render_section(
                store.as_ref(),
                &Value::Object(request),
                &resolver,
            )
            .map_err(contract);
        }
        let models = self.require_model()?;
        let before = models.status_value()?;
        if before.get("loaded") != Some(&Value::Bool(true)) {
            return Err(AgentError::contract(
                "render section requires a loaded model",
            ));
        }
        let (source_identity, truth_status) = self.render_source(&request["source"], &before)?;
        let (bbox, bounds_source) = if let Some(b) = bbox_of(&request["bbox_mm"]) {
            (b, "request_bbox_mm")
        } else {
            let extent = before.get("aabb").and_then(|a| a.get("bbox_mm")).cloned();
            let Some(raw) = extent.filter(|b| b.as_array().is_some_and(|a| a.len() == 2)) else {
                return Err(AgentError::contract(
                    "the model has no bounded view; provide a finite bbox_mm",
                ));
            };
            (normalise_bbox(&raw)?, "model_aabb")
        };
        let (width, height, mut framing) = render_section_dimensions(&request, &bbox)?;
        let axis = match request["plane"].as_str() {
            Some("x") => 0,
            Some("y") => 1,
            _ => 2,
        };
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        let (lower, upper) = (bbox[0], bbox[1]);
        let position = request["position"].as_f64().unwrap_or(0.5);
        let mut at = lower[axis] + position * (upper[axis] - lower[axis]);
        #[allow(clippy::cast_precision_loss)]
        let (wf, hf) = (width as f64, height as f64);
        let mut pixel = ((upper[u] - lower[u]) / wf).max((upper[v] - lower[v]) / hf);
        let (span_u, span_v) = (pixel * wf, pixel * hf);
        let physical = framing["physical_span_mm"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (pu, pv) = (
            physical.first().and_then(Value::as_f64).unwrap_or(0.0),
            physical.get(1).and_then(Value::as_f64).unwrap_or(0.0),
        );
        let close = |a: f64, b: f64| (a - b).abs() <= 1e-12_f64.max(1e-12 * a.abs().max(b.abs()));
        framing.insert("bounds_source".into(), json!(bounds_source));
        framing.insert("pixel_size_mm".into(), json!(pixel));
        framing.insert("sampled_span_mm".into(), json!([span_u, span_v]));
        framing.insert(
            "letterboxed".into(),
            json!(!(close(span_u, pu) && close(span_v, pv))),
        );
        framing.insert("equal_physical_scale".into(), json!(true));
        let (cu, cv) = (0.5 * (lower[u] + upper[u]), 0.5 * (lower[v] + upper[v]));
        let us = implexity_mesh::numeric::linspace(
            cu - 0.5 * span_u + 0.5 * pixel,
            cu + 0.5 * span_u - 0.5 * pixel,
            width,
        );
        let vs = implexity_mesh::numeric::linspace(
            cv - 0.5 * span_v + 0.5 * pixel,
            cv + 0.5 * span_v - 0.5 * pixel,
            height,
        );
        let mut points = Vec::with_capacity(width * height);
        let mut outside = Vec::with_capacity(width * height);
        for &uu in &us {
            for &vv in &vs {
                let mut p = [0.0; 3];
                p[axis] = at;
                p[u] = uu;
                p[v] = vv;
                points.push(p);
                outside.push(uu < lower[u] || uu > upper[u] || vv < lower[v] || vv > upper[v]);
            }
        }
        let overlay: Vec<String> = request["overlay_regions"]
            .as_array()
            .map(|a| a.iter().map(py_str).collect())
            .unwrap_or_default();
        let (region_masks, region_legend) = if overlay.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let problem = self.current_problem();
            implexity_render::rendering::region_masks(
                models.as_ref(),
                &problem,
                &overlay,
                &points,
                pixel,
                bbox,
            )
            .map_err(contract)?
        };
        let masks: Vec<&[bool]> = region_masks.iter().map(Vec::as_slice).collect();
        let expected = source_identity
            .get("content_id")
            .cloned()
            .unwrap_or(Value::Null);
        let bg = request["background"].as_str().unwrap_or("dark").to_owned();
        let field_name = request["field"]
            .as_str()
            .unwrap_or("model_boundary")
            .to_owned();
        let (png, field_record) = if field_name == "model_boundary" {
            let evaluated = models
                .evaluate_exact(&points)
                .map_err(|e| AgentError::failed(e.to_string()))?;
            let values = evaluated.values;
            if values.len() != width * height || values.iter().any(|x| !x.is_finite()) {
                return Err(AgentError::refused(
                    "the authoritative model evaluator returned an invalid section",
                ));
            }
            let after = models.status_value()?;
            if content_of(&before) != expected
                || json!(evaluated.content_id) != expected
                || content_of(&after) != expected
            {
                return Err(AgentError::refused(
                    "the model changed while its section was being rendered",
                ));
            }
            let lo = values.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let extent = 1e-9_f64.max(lo.abs()).max(hi.abs());
            let edge_width = (pixel * 0.9).min(0.1 * extent);
            let rho: Vec<f64> = values
                .iter()
                .map(|x| if *x < 0.0 { 1.0 } else { 0.0 })
                .collect();
            let edge: Vec<f64> = values
                .iter()
                .map(|x| if x.abs() <= edge_width { 1.0 } else { 0.0 })
                .collect();
            let mut img = raster::section_rgb(
                &rho,
                width,
                height,
                None,
                Some(&edge),
                Some(&outside),
                background(&bg),
            );
            raster::overlay_region_contours(&mut img, &masks, background(&bg));
            let inside: Vec<f64> = values
                .iter()
                .zip(&outside)
                .filter(|(_, o)| !**o)
                .map(|(x, _)| if *x < 0.0 { 1.0 } else { 0.0 })
                .collect();
            #[allow(clippy::cast_precision_loss)]
            let solid_fraction = if inside.is_empty() {
                0.0
            } else {
                implexity_mesh::numeric::pairwise_sum(&inside) / inside.len() as f64
            };
            let record = json!({"name": field_name, "source": "authoritative_implicit_model_evaluator",
                                "mode": evaluated.mode, "evaluator": evaluated.evaluator,
                                "units": evaluated.units, "value_range": [lo, hi],
                                "solid_fraction": solid_fraction});
            (img.to_png(), record)
        } else {
            let sampled = implexity_render::rendering::sample_field_section(
                models.as_ref(),
                &field_name,
                request["plane"].as_str().unwrap_or("z"),
                position,
                bbox,
                width,
                height,
            )
            .map_err(contract)?;
            at = sampled.at_mm;
            pixel = sampled.pixel_size_mm;
            let after = models.status_value()?;
            if content_of(&before) != expected
                || sampled.content_id != expected
                || content_of(&after) != expected
            {
                return Err(AgentError::refused(
                    "the model changed while its registered field was rendered",
                ));
            }
            let catalog = models.registered_fields().map_err(contract)?;
            let Some(descriptor) = catalog
                .iter()
                .find(|r| r["field"] == field_name.as_str())
                .cloned()
            else {
                return Err(AgentError::contract(
                    "the requested field is not renderable",
                ));
            };
            let mut palette = request["palette"].as_str().unwrap_or("auto").to_owned();
            if palette == "auto" {
                descriptor["suggested_palette"]
                    .as_str()
                    .unwrap_or("sequential")
                    .clone_into(&mut palette);
            }
            let finite: Vec<f64> = sampled
                .values
                .iter()
                .copied()
                .filter(|x| x.is_finite())
                .collect();
            if finite.is_empty() {
                return Err(AgentError::contract(
                    "the requested section misses the field registration",
                ));
            }
            let value_range = request["value_range"]
                .as_array()
                .and_then(|a| Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]));
            let opts = ScalarSectionOptions {
                outside: Some(&sampled.outside),
                palette: SectionPalette::from_name(&palette, descriptor.get("categorical"))
                    .map_err(contract)?,
                value_range,
                label: descriptor["label"].as_str().unwrap_or_default().to_owned(),
                region_masks: masks.clone(),
                background: background(&bg),
                draw_legend: true,
            };
            let img = raster::scalar_section_rgb(&sampled.values, width, height, &opts)
                .map_err(contract)?;
            let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let mut record = descriptor.as_object().cloned().unwrap_or_default();
            record.insert("name".into(), json!(field_name));
            record.insert(
                "source".into(),
                json!("authoritative_registered_model_parameter"),
            );
            record.insert("palette".into(), json!(palette));
            record.insert(
                "display_range".into(),
                value_range.map_or_else(|| json!([lo, hi]), |r| json!(r)),
            );
            (img.to_png(), Value::Object(record))
        };
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") || png.len() > RENDER_SECTION_MAX_PNG_BYTES {
            return Err(AgentError::refused(
                "the bounded section renderer produced an invalid PNG",
            ));
        }
        let bg_rgb = if bg == "white" {
            json!([255, 255, 255])
        } else {
            json!([8, 9, 11])
        };
        Ok(json!({
            "schema": "implexity-rendered-section/1", "kind": "render_section",
            "truth_status": truth_status, "source": source_identity,
            "view": {"plane": request["plane"], "position": position, "at_mm": at, "bbox_mm": bbox,
                     "pixel_size_mm": pixel, "width_px": width, "height_px": height,
                     "framing": framing, "background": bg},
            "field": field_record, "overlay_regions": region_legend,
            "presentation": {"background": bg, "background_rgb": bg_rgb},
            "render_status": "sampled_preview",
            "image": inline_image(&png, width, height),
        }))
    }
}

fn native_field_record(
    grid: &implexity_mesh::model_view::RegisteredGrid,
    bbox: [[f64; 3]; 2],
    descriptor: &Value,
) -> AgentResult<implexity_render::artifact::FieldRecord> {
    let shape = grid.values.shape;
    #[allow(clippy::cast_precision_loss)]
    let upper: [f64; 3] =
        std::array::from_fn(|a| grid.origin[a] + grid.spacing[a] * shape[a] as f64);
    let registration = implexity_geometry::field_registration::axis_aligned_registration(
        shape,
        grid.origin,
        upper,
        "cell",
    )
    .map_err(contract)?;
    let values = grid.values.data.clone();
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    Ok(implexity_render::artifact::FieldRecord {
        shape: shape.to_vec(),
        stored: implexity_render::artifact::StoredArray {
            shape: shape.to_vec(),
            dtype: "float64".into(),
            bytes,
            values: values.clone(),
            real_numeric: true,
        },
        registration: implexity_render::artifact::Registration {
            shape,
            origin: grid.origin,
            matrix: [
                [grid.spacing[0], 0.0, 0.0],
                [0.0, grid.spacing[1], 0.0],
                [0.0, 0.0, grid.spacing[2]],
            ],
            centering: "cell".into(),
            axis_order: "xyz".into(),
            frame: "model".into(),
            wire: registration.to_wire(),
        },
        values,
        bbox_mm: bbox,
        descriptor: descriptor.clone(),
    })
}

fn registration_arrays_match(a: &Value, b: &Value) -> bool {
    ["origin_mm", "spacing_mm", "bbox_mm"].iter().all(|k| {
        let flat = |v: &Value| -> Option<Vec<f64>> {
            match v {
                Value::Array(items) => items.iter().try_fold(Vec::new(), |mut acc, x| {
                    match x {
                        Value::Array(_) => acc.extend(flat_inner(x)?),
                        other => acc.push(other.as_f64()?),
                    }
                    Some(acc)
                }),
                other => other.as_f64().map(|x| vec![x]),
            }
        };
        match (a.get(*k).and_then(flat), b.get(*k).and_then(flat)) {
            (Some(x), Some(y)) => {
                x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| (p - q).abs() <= 1e-12)
            }
            _ => false,
        }
    })
}

fn flat_inner(v: &Value) -> Option<Vec<f64>> {
    v.as_array()?.iter().map(Value::as_f64).collect()
}

impl AgentManager {
    fn guarded_status(
        models: &dyn AgentModel,
        expected: &str,
        message: &str,
    ) -> AgentResult<Value> {
        let status = models.status_value()?;
        if status.get("content_id").and_then(Value::as_str) != Some(expected) {
            return Err(AgentError::refused(message));
        }
        Ok(status)
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn render_3d(&self, payload: &Map<String, Value>) -> AgentResult<Value> {
        let request =
            render3d::normalise_request(&Value::Object(payload.clone())).map_err(contract)?;
        if request.source["kind"] == "optimization_epoch_field" {
            let field = request.source["field"].as_str().unwrap_or_default();
            if request.surface_field != field
                || request.color_field.as_deref().is_some_and(|c| c != field)
            {
                return Err(AgentError::contract(
                    "retained epoch rendering requires the selected field for surface and colour",
                ));
            }
            let read = self.host.call(HostOp::ReadEpochField(json!({"job_id":request.source["job_id"],
                "epoch":request.source["epoch"],"field":field,"operating_point":request.source["operating_point"],
                "maximum_bytes":268_435_456,"mode":"payload"})))?;
            if read["available"] != true
                || read["encoding"] != "base64-C"
                || read["job_id"] != request.source["job_id"]
                || read["epoch"] != request.source["epoch"]
                || read["field"] != field
            {
                return Err(AgentError::refused(
                    "the selected epoch field is unavailable or has no scalar payload",
                ));
            }
            let shape: Vec<usize> = read["shape"]
                .as_array()
                .ok_or_else(|| AgentError::contract("retained field shape is missing"))?
                .iter()
                .map(|d| {
                    d.as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| AgentError::contract("retained field shape is invalid"))
                })
                .collect::<AgentResult<_>>()?;
            let count = shape
                .iter()
                .try_fold(1_usize, |a, b| a.checked_mul(*b))
                .ok_or_else(|| AgentError::contract("retained field shape overflows"))?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(read["data"].as_str().unwrap_or_default())
                .map_err(contract)?;
            if bytes.len() > 268_435_456
                || read["sha256"].as_str()
                    != Some(implexity_io::digest::sha256_hex(&bytes).as_str())
            {
                return Err(AgentError::refused(
                    "the retained field content identity differs",
                ));
            }
            let dtype = read["dtype"]
                .as_str()
                .ok_or_else(|| AgentError::contract("retained field dtype is missing"))?;
            let mut encoded =
                implexity_io::npy::header_bytes(dtype, false, &shape).map_err(contract)?;
            encoded.extend_from_slice(&bytes);
            let decoded = implexity_io::npy::NpyArray::from_bytes(&encoded).map_err(contract)?;
            if count.checked_mul(decoded.data.itemsize()) != Some(bytes.len()) {
                return Err(AgentError::refused(
                    "retained field byte count differs from its dtype and shape",
                ));
            }
            let exact_integers = match &decoded.data {
                implexity_io::npy::NpyData::I64(values) => values
                    .iter()
                    .all(|value| value.unsigned_abs() <= (1_u64 << 53)),
                implexity_io::npy::NpyData::U64(values) => {
                    values.iter().all(|value| *value <= (1_u64 << 53))
                }
                _ => true,
            };
            if !exact_integers {
                return Err(AgentError::refused(
                    "this integer field exceeds exact float64 visualization precision",
                ));
            }
            let values: Vec<f64> = decoded
                .to_f64()
                .ok_or_else(|| {
                    AgentError::refused("retained rendering requires real numeric scalar data")
                })?
                .iter()
                .copied()
                .collect();
            drop(encoded);
            drop(decoded);
            if values.iter().any(|v| !v.is_finite()) {
                return Err(AgentError::refused(
                    "retained rendering requires finite scalar values",
                ));
            }
            let id = format!("result-{}", &implexity_io::digest::sha256_hex(&bytes)[..32]);
            let store = RetainedRenderField {
                id: id.clone(),
                field: field.to_owned(),
                array: implexity_render::artifact::StoredArray {
                    shape,
                    dtype: dtype.into(),
                    bytes,
                    values,
                    real_numeric: true,
                },
                read: read.clone(),
            };
            let mut internal = request.clone();
            internal.source = json!({"kind":"result_artifact","artifact_id":id});
            let mut result =
                implexity_render::artifact::render_3d(&store, &internal).map_err(contract)?;
            let mut source = request.source.as_object().cloned().unwrap_or_default();
            for key in [
                "design_state_id",
                "checkpoint_sha256",
                "solve_id",
                "sha256",
                "scope",
                "operating_point",
                "operating_points",
            ] {
                source.insert(key.into(), read.get(key).cloned().unwrap_or(Value::Null));
            }
            source.insert("live_model_mutated".into(), json!(false));
            source.insert("solve_started".into(), json!(false));
            result["source"] = Value::Object(source);
            return Ok(result);
        }
        if request.source["kind"] == "result_artifact" {
            let artifact = request.source["artifact_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let store = self.host.result_store(&artifact)?;
            return implexity_render::artifact::render_3d(store.as_ref(), &request)
                .map_err(contract);
        }
        let (models, before, source_identity, truth_status) = if request.source["kind"]
            == "optimization_epoch"
        {
            let job_id = py_str(&request.source["job_id"]);
            let epoch = request.source["epoch"].as_i64().ok_or_else(|| {
                AgentError::contract("saved-epoch rendering requires a nonnegative integer epoch")
            })?;
            let exported = match self.host.call(HostOp::ExportEpoch(job_id.clone(), epoch)) {
                Ok(v) => v,
                Err(AgentError::KeyError(_)) => {
                    return Err(AgentError::contract(format!(
                        "optimization job {job_id} has no published epoch {epoch}"
                    )));
                }
                Err(e) => return Err(e),
            };
            if exported.get("job_id") != Some(&json!(job_id))
                || exported.get("epoch") != Some(&json!(epoch))
            {
                return Err(AgentError::refused(
                    "the exported epoch identity differs from the requested epoch",
                ));
            }
            let view = self
                .host
                .detached_model(&exported["document"])
                .map_err(|e| {
                    AgentError::refused(format!(
                        "the exported epoch document does not build: {}",
                        e.message()
                    ))
                })?;
            let status = view.status_value()?;
            if exported.get("content_id") != status.get("content_id") {
                return Err(AgentError::refused(
                    "the exported epoch document identity drifted",
                ));
            }
            let render_identity = exported["document"]
                .pointer("/meta/implexity/render_identity")
                .and_then(Value::as_object)
                .filter(|r| render_identity_ok(r))
                .ok_or_else(|| {
                    AgentError::refused(
                        "the exported epoch render metadata is missing or malformed",
                    )
                })?;
            if render_identity.get("model_content_id") != status.get("content_id") {
                return Err(AgentError::refused(
                    "the exported epoch render metadata content identity drifted",
                ));
            }
            for key in ["job_id", "epoch", "solve_id", "design_state_id"] {
                if render_identity.get(key) != exported.get(key) {
                    return Err(AgentError::refused(
                        "the exported epoch render metadata identity drifted",
                    ));
                }
            }
            if render_identity.get("checkpoint_sha256")
                != exported.get("checkpoint").and_then(|c| c.get("sha256"))
            {
                return Err(AgentError::refused(
                    "the exported epoch checkpoint identity drifted",
                ));
            }
            let mut identity = render_identity.clone();
            identity.shift_remove("schema");
            identity.insert("kind".into(), json!("optimization_epoch"));
            identity.insert(
                "provenance_kind".into(),
                json!("detached_optimization_epoch"),
            );
            identity.insert(
                "identity_source".into(),
                json!("content_bound_model_metadata"),
            );
            identity.insert(
                "structure_id".into(),
                status.get("structure_id").cloned().unwrap_or(Value::Null),
            );
            identity.insert(
                "content_id".into(),
                status.get("content_id").cloned().unwrap_or(Value::Null),
            );
            identity.insert("live_model_mutated".into(), json!(false));
            for key in [
                "document_sha256",
                "result_authority",
                "canonical_eligible",
                "observational_only",
            ] {
                identity.insert(
                    key.into(),
                    exported.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            let authority = exported
                .get("result_authority")
                .filter(|v| crate::pyval::truthy(v))
                .map_or_else(|| "undeclared".to_owned(), py_str);
            (
                view,
                status,
                identity,
                format!("optimization_epoch_{authority}"),
            )
        } else {
            let view = self.require_model()?;
            let status = view.status_value()?;
            let (identity, truth) = self.render_source(&request.source, &status)?;
            (view, status, identity, truth)
        };
        if before.get("loaded") != Some(&Value::Bool(true)) {
            return Err(AgentError::contract("render 3d requires a loaded model"));
        }
        let Some(expected) = source_identity
            .get("content_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
        else {
            return Err(AgentError::refused(
                "the render source has no stable model content identity",
            ));
        };
        let descriptors: Vec<Value> = {
            let live = models.live_lock();
            Self::guarded_status(
                models.as_ref(),
                &expected,
                "the model changed before its render fields were inspected",
            )?;
            let d = models.registered_fields().map_err(contract)?;
            drop(live);
            Self::guarded_status(
                models.as_ref(),
                &expected,
                "the model changed while its render fields were inspected",
            )?;
            d
        };
        let descriptor_of = |name: &str| descriptors.iter().find(|r| r["field"] == name).cloned();
        let surface_name = request.surface_field.clone();
        let native = render3d::is_native_quality(&request.quality);
        let mut timing: Vec<(&str, f64)> = Vec::new();
        let started = Instant::now();
        let mut base_bbox: Option<[[f64; 3]; 2]> = None;
        let mut extent_record = Value::Null;
        let mut authoritative = false;
        let mut surface_descriptor = Value::Null;
        if surface_name == "model_boundary" {
            if let Some(extent) = before.get("aabb").filter(|a| a.is_object()) {
                extent_record = extent.clone();
                base_bbox = extent.get("bbox_mm").and_then(bbox_of);
                authoritative = extent.get("known") == Some(&Value::Bool(true));
            }
        } else {
            let Some(d) = descriptor_of(&surface_name) else {
                return Err(AgentError::contract(
                    "the requested surface field is not registered",
                ));
            };
            if native && d.get("native_analysis_grid") != Some(&Value::Bool(true)) {
                return Err(AgentError::contract(
                    "native/analysis current-model rendering requires the field descriptor to declare native_analysis_grid=true",
                ));
            }
            if native && d["registration"].get("centering").and_then(Value::as_str) != Some("cell")
            {
                return Err(AgentError::contract(
                    "native/analysis current-model rendering requires a cell-centred registered field",
                ));
            }
            base_bbox = d["registration"].get("bbox_mm").and_then(bbox_of);
            authoritative = true;
            surface_descriptor = d;
        }
        let (crop, bounds_source) = match request.crop_mm {
            None => {
                let Some(base) = base_bbox else {
                    return Err(AgentError::contract(
                        "the surface has no bounded view; provide crop_mm",
                    ));
                };
                if surface_name == "model_boundary" {
                    if authoritative {
                        let span: [f64; 3] = std::array::from_fn(|a| base[1][a] - base[0][a]);
                        if span.iter().any(|s| *s <= 0.0)
                            || base.iter().flatten().any(|v| !v.is_finite())
                        {
                            return Err(AgentError::contract(
                                "the model bound cannot frame a 3d view; provide crop_mm",
                            ));
                        }
                        let halo: [f64; 3] = span.map(|s| 0.025 * s);
                        (
                            [
                                std::array::from_fn(|a| base[0][a] - halo[a]),
                                std::array::from_fn(|a| base[1][a] + halo[a]),
                            ],
                            "known_model_aabb_with_sampling_halo",
                        )
                    } else {
                        (base, "model_frame_hint")
                    }
                } else {
                    (base, "field_registration")
                }
            }
            Some(asked) => {
                if surface_name != "model_boundary"
                    && let Some(base) = base_bbox
                {
                    let largest = base
                        .iter()
                        .flatten()
                        .map(|v| v.abs())
                        .fold(0.0_f64, f64::max);
                    let tolerance = 1e-10 * largest.max(1.0);
                    if (0..3).any(|a| {
                        asked[0][a] < base[0][a] - tolerance || asked[1][a] > base[1][a] + tolerance
                    }) {
                        return Err(AgentError::contract(
                            "render 3d crop_mm must remain inside the surface bounds",
                        ));
                    }
                }
                (asked, "request_crop_mm")
            }
        };
        let (values, shape, extraction_bbox, mut sampling, mut surface_record) = if native {
            let grid = {
                let live = models.live_lock();
                Self::guarded_status(
                    models.as_ref(),
                    &expected,
                    "the model changed before its native 3d field was read",
                )?;
                let g = models
                    .resolve_registered_field(&surface_name)
                    .map_err(contract)?;
                drop(live);
                Self::guarded_status(
                    models.as_ref(),
                    &expected,
                    "the model changed while its native 3d field was read",
                )?;
                g
            };
            if grid.record.get("content_id").and_then(Value::as_str) != Some(expected.as_str()) {
                return Err(AgentError::refused(
                    "the model changed while its native 3d field was read",
                ));
            }
            let record =
                native_field_record(&grid, base_bbox.unwrap_or(crop), &surface_descriptor)?;
            let (vals, shape, bbox, rec) =
                implexity_render::artifact::native_surface_samples(&record, crop, &request.quality)
                    .map_err(contract)?;
            let mut surface = surface_descriptor.as_object().cloned().unwrap_or_default();
            surface.insert("name".into(), json!(surface_name));
            surface.insert(
                "source".into(),
                json!("declared_native_registered_model_parameter"),
            );
            surface.insert(
                "parameter_transform_applied".into(),
                surface_descriptor
                    .get("parameter_transform_applied")
                    .cloned()
                    .unwrap_or(json!(false)),
            );
            surface.insert("iso_value".into(), json!(request.iso_value));
            (vals, shape, bbox, rec, surface)
        } else {
            let hint = if render3d::GEOMETRY_QUALITIES
                .iter()
                .any(|(q, _)| *q == request.quality)
            {
                if surface_name == "model_boundary" {
                    let _live = models.live_lock();
                    Self::guarded_status(
                        models.as_ref(),
                        &expected,
                        "the model changed before its sampling resolution was inspected",
                    )?;
                    models
                        .geometry_sampling_hint()
                        .map_err(contract)?
                        .map(|h| SpacingHint {
                            spacing_mm: h.spacing_mm,
                            source: Some("model_declared_geometry_lattice".into()),
                            declared_by: h.declared_by,
                        })
                } else {
                    let spacing = surface_descriptor["registration"]["spacing_mm"]
                        .as_array()
                        .map_or(f64::NAN, |a| {
                            a.iter()
                                .filter_map(Value::as_f64)
                                .map(f64::abs)
                                .fold(f64::INFINITY, f64::min)
                        });
                    Some(SpacingHint {
                        spacing_mm: spacing,
                        source: Some("field_registration".into()),
                        declared_by: vec![surface_name.clone()],
                    })
                }
            } else {
                None
            };
            let grid =
                render3d::sampling_grid(crop, &request.quality, hint.as_ref()).map_err(contract)?;
            let points = grid.points();
            let mut sampling = grid.record.clone();
            if surface_name == "model_boundary" {
                let (mut vals, evaluated) = {
                    let live = models.live_lock();
                    Self::guarded_status(
                        models.as_ref(),
                        &expected,
                        "the model changed before its 3d field was sampled",
                    )?;
                    let out = evaluate_model_blocks(models.as_ref(), &points, &expected)
                        .map_err(|e| AgentError::refused(e.to_string()))?;
                    drop(live);
                    Self::guarded_status(
                        models.as_ref(),
                        &expected,
                        "the model changed while its 3d field was sampled",
                    )?;
                    out
                };
                sampling["evaluation_blocks"] = evaluated["blocks"].clone();
                sampling["evaluation_block_points"] = evaluated["block_points"].clone();
                if request.region == "complement" {
                    let Some(base) = base_bbox.filter(|_| authoritative) else {
                        return Err(AgentError::contract(
                            "the complement region requires a known model extent",
                        ));
                    };
                    let iso = request.iso_value;
                    let spacing = if points.len() > 1 {
                        (0..3)
                            .map(|a| (points[1][a] - points[0][a]).abs())
                            .fold(0.0_f64, f64::max)
                    } else {
                        1.0
                    };
                    for (value, p) in vals.iter_mut().zip(&points) {
                        let outside = (0..3).any(|a| p[a] < base[0][a] || p[a] > base[1][a]);
                        *value = if outside {
                            iso + spacing.max(1e-9)
                        } else {
                            2.0 * iso - *value
                        };
                    }
                }
                let surface = json!({"name": "model_boundary", "region": request.region,
                    "source": "authoritative_implicit_model_evaluator",
                    "mode": evaluated.get("mode").cloned().unwrap_or(Value::Null),
                    "evaluator": evaluated.get("evaluator").cloned().unwrap_or(Value::Null),
                    "units": evaluated.get("units").cloned().unwrap_or_else(|| json!("mm")),
                    "iso_value": request.iso_value, "model_extent": extent_record});
                (
                    vals,
                    grid.shape,
                    crop,
                    sampling,
                    surface.as_object().cloned().unwrap_or_default(),
                )
            } else {
                let (vals, valid, registration) = {
                    let live = models.live_lock();
                    Self::guarded_status(
                        models.as_ref(),
                        &expected,
                        "the model changed before its 3d field was sampled",
                    )?;
                    let (sampler, record) =
                        registered_field_sampler(models.as_ref(), &surface_name, false)
                            .map_err(contract)?;
                    let (v, ok) = sampler.sample(&points, 0.0).map_err(contract)?;
                    drop(live);
                    let after = models.status_value()?;
                    (v, ok, (record, after))
                };
                if registration.0.get("content_id").and_then(Value::as_str)
                    != Some(expected.as_str())
                    || registration.1.get("content_id").and_then(Value::as_str)
                        != Some(expected.as_str())
                {
                    return Err(AgentError::refused(
                        "the model changed while its 3d field was sampled",
                    ));
                }
                if !valid.iter().all(|v| *v) {
                    return Err(AgentError::contract(
                        "render 3d crop reaches outside the surface field registration",
                    ));
                }
                let mut surface = surface_descriptor.as_object().cloned().unwrap_or_default();
                surface.insert("name".into(), json!(surface_name));
                let read_only = surface_descriptor
                    .get("read_only")
                    .is_some_and(crate::pyval::truthy);
                surface.insert(
                    "source".into(),
                    json!(if read_only {
                        "node_declared_derived_grid"
                    } else {
                        "authoritative_registered_model_parameter"
                    }),
                );
                surface.insert(
                    "parameter_transform_applied".into(),
                    surface_descriptor
                        .get("parameter_transform_applied")
                        .cloned()
                        .unwrap_or(json!(false)),
                );
                surface.insert("iso_value".into(), json!(request.iso_value));
                (vals, grid.shape, crop, sampling, surface)
            }
        };
        let sampled_status = models.status_value()?;
        if content_of(&before) != json!(expected) || content_of(&sampled_status) != json!(expected)
        {
            return Err(AgentError::refused(
                "the model changed while its 3d surface was sampled",
            ));
        }
        timing.push(("sampling", started.elapsed().as_secs_f64()));
        let policy = render3d::quality_policy(&request.quality);
        let started = Instant::now();
        let field3 = Field3 {
            shape,
            data: &values,
        };
        let (mesh, mut mesh_record) = render3d::extract_isosurface(
            &field3,
            extraction_bbox,
            request.iso_value,
            policy.max_triangles,
        )
        .map_err(contract)?;
        timing.push(("extraction", started.elapsed().as_secs_f64()));
        surface_record.insert(
            "sampled_value_range".into(),
            mesh_record["sampled_value_range"].clone(),
        );

        #[allow(clippy::cast_precision_loss)]
        let coverage_tolerance = (0..3)
            .map(|a| (extraction_bbox[1][a] - extraction_bbox[0][a]) / (shape[a] as f64 - 1.0))
            .fold(f64::NEG_INFINITY, f64::max);
        let mut attributes: Option<Vec<f64>> = None;
        let mut colour_spec: Option<ColourSpec> = None;
        let mut color_record: Option<Map<String, Value>> = None;
        let mut colour_sampler: Option<implexity_mesh::model_view::GridSampler> = None;
        if let Some(color_name) = &request.color_field {
            let Some(descriptor) = descriptor_of(color_name) else {
                return Err(AgentError::contract(
                    "the requested color field is not registered",
                ));
            };
            if native {
                let matches = descriptor.get("native_analysis_grid") == Some(&Value::Bool(true))
                    && descriptor["registration"]
                        .get("centering")
                        .and_then(Value::as_str)
                        == Some("cell")
                    && descriptor.get("shape") == surface_descriptor.get("shape")
                    && registration_arrays_match(
                        &descriptor["registration"],
                        &surface_descriptor["registration"],
                    );
                if !matches {
                    return Err(AgentError::contract(
                        "native/analysis color fields must declare and share the exact native cell grid of the surface field",
                    ));
                }
            }
            let categorical = descriptor.get("categorical").is_some_and(|c| !c.is_null());
            let (sampler, registration, locked_after) = {
                let live = models.live_lock();
                Self::guarded_status(
                    models.as_ref(),
                    &expected,
                    "the model changed before its 3d colours were sampled",
                )?;
                let (s, r) = registered_field_sampler(models.as_ref(), color_name, categorical)
                    .map_err(contract)?;
                drop(live);
                (s, r, models.status_value()?)
            };
            let (attrs, valid) = sampler
                .sample(&mesh.vertices, coverage_tolerance)
                .map_err(contract)?;
            if registration.get("content_id").and_then(Value::as_str) != Some(expected.as_str())
                || locked_after.get("content_id").and_then(Value::as_str) != Some(expected.as_str())
            {
                return Err(AgentError::refused(
                    "the model changed while its 3d colours were sampled",
                ));
            }
            if !valid.iter().all(|v| *v) {
                return Err(AgentError::contract(
                    "the color field does not cover the complete rendered surface",
                ));
            }
            let (spec, specification) =
                render3d::resolve_colour_mapping(&request, &descriptor).map_err(contract)?;
            let (_colours, mapping) = render3d::colour_vertices(&attrs, &spec).map_err(contract)?;
            let mut rec = descriptor.as_object().cloned().unwrap_or_default();
            rec.insert("name".into(), json!(color_name));
            let read_only = descriptor
                .get("read_only")
                .is_some_and(crate::pyval::truthy);
            rec.insert(
                "source".into(),
                json!(if native {
                    "declared_native_registered_model_parameter"
                } else if read_only {
                    "node_declared_derived_grid"
                } else {
                    "authoritative_registered_model_parameter"
                }),
            );
            rec.insert("color_specification".into(), json!(specification));
            rec.insert("coverage_tolerance_mm".into(), json!(coverage_tolerance));
            rec.insert(
                "coverage_tolerance_source".into(),
                json!("surface_sampling_spacing"),
            );
            for (k, v) in mapping.as_object().into_iter().flatten() {
                rec.insert(k.clone(), v.clone());
            }
            if native {
                sampling["color_sampling"] = json!({"field": color_name, "registration_matches_surface": true,
                    "mode": if categorical { "nearest_registered_native_cell" } else { "trilinear_registered_native_cell" }});
            }
            let display = rec
                .get("display_range")
                .and_then(|d| Some([d.get(0)?.as_f64()?, d.get(1)?.as_f64()?]));
            colour_spec = Some(match spec {
                ColourSpec::Ramp { palette, .. } => ColourSpec::Ramp {
                    palette,
                    value_range: display,
                },
                ColourSpec::Categorical(p, _) => ColourSpec::Categorical(p, display),
                s @ ColourSpec::Stops(_) => s,
            });
            attributes = Some(attrs);
            color_record = Some(rec);
            colour_sampler = Some(sampler);
        }

        let started = Instant::now();
        let (mesh, attributes) = render3d::clip_mesh(
            &mesh,
            attributes.as_deref(),
            request.clip.as_ref(),
            policy.max_triangles,
        )
        .map_err(contract)?;
        let vertex_colours = match (&colour_spec, &attributes) {
            (Some(spec), Some(a)) => Some(render3d::colour_vertices(a, spec).map_err(contract)?.0),
            _ => None,
        };
        let cap = match request
            .clip
            .as_ref()
            .and_then(|c| c.cap.as_ref().map(|cap| (c, cap)))
        {
            None => None,
            Some((clip, cap_req)) => {
                let inside = if cap_req.inside == "auto" {
                    if surface_name == "model_boundary" {
                        "below_iso"
                    } else {
                        "above_iso"
                    }
                    .to_owned()
                } else {
                    cap_req.inside.clone()
                };
                let mut cap_bounds = extraction_bbox;
                if surface_name == "model_boundary"
                    && authoritative
                    && let Some(model_box) = base_bbox
                {
                    cap_bounds = [
                        std::array::from_fn(|a| cap_bounds[0][a].max(model_box[0][a])),
                        std::array::from_fn(|a| cap_bounds[1][a].min(model_box[1][a])),
                    ];
                }
                let solid: CapColour<'_> = if let (Some(sampler), Some(spec)) =
                    (&colour_sampler, &colour_spec)
                {
                    let spec = spec.clone();
                    Box::new(move |pts: &[[f64; 3]]| {
                        let (vals, valid) = sampler
                            .sample(pts, coverage_tolerance)
                            .map_err(|e| implexity_render::RenderError::Invalid(e.to_string()))?;
                        let (c, _) = render3d::colour_vertices(&vals, &spec)?;
                        Ok((c, valid))
                    })
                } else {
                    let rgb = request.surface_color_rgb.unwrap_or(render3d::BASE_COLOUR);
                    Box::new(move |pts: &[[f64; 3]]| {
                        Ok((vec![rgb; pts.len()], vec![true; pts.len()]))
                    })
                };
                let mut cap = render3d::grid_section_cap(
                    &field3,
                    extraction_bbox,
                    request.iso_value,
                    &inside,
                    cap_bounds,
                    clip,
                    solid,
                    cap_req.complement_color_rgb,
                )
                .map_err(contract)?;
                cap.record["inside_rule_source"] = json!(if cap_req.inside != "auto" {
                    "request"
                } else if surface_name == "model_boundary" {
                    "implicit_negative_interior"
                } else {
                    "registered_field_values_above_iso"
                });
                cap.record["complement_bounds_source"] =
                    json!(if surface_name == "model_boundary" && authoritative {
                        "render_crop_and_known_model_aabb"
                    } else {
                        "render_crop"
                    });
                Some(cap)
            }
        };
        let (camera_bounds, camera_source) =
            if matches!(request.camera, CameraRequest::Preset { .. }) {
                let mut lo = [f64::INFINITY; 3];
                let mut hi = [f64::NEG_INFINITY; 3];
                for v in &mesh.vertices {
                    for a in 0..3 {
                        lo[a] = lo[a].min(v[a]);
                        hi[a] = hi[a].max(v[a]);
                    }
                }
                let span: [f64; 3] = std::array::from_fn(|a| (hi[a] - lo[a]).max(1e-9));
                (
                    [
                        std::array::from_fn(|a| lo[a] - 0.04 * span[a]),
                        std::array::from_fn(|a| hi[a] + 0.04 * span[a]),
                    ],
                    "clipped_surface_bounds",
                )
            } else {
                (crop, "render_crop")
            };
        let mut camera = render3d::resolve_camera(
            &request.camera,
            camera_bounds,
            request.width_px,
            request.height_px,
        )
        .map_err(contract)?;
        camera.record["framing_bounds_mm"] = json!(camera_bounds);
        camera.record["framing_bounds_source"] = json!(camera_source);
        let categorical_palette = match (&colour_spec, &color_record) {
            (Some(ColourSpec::Categorical(p, _)), Some(r))
                if r.get("palette") == Some(&json!("categorical")) =>
            {
                Some(p.clone())
            }
            _ => None,
        };
        let colouring = match (&categorical_palette, &vertex_colours, &attributes) {
            (Some(p), _, Some(a)) => VertexColouring::Categorical {
                values: a,
                palette: p,
            },
            (None, Some(c), _) => VertexColouring::Colours(c),
            _ => VertexColouring::Uniform,
        };
        let opts = RasterOptions {
            width: request.width_px,
            height: request.height_px,
            camera: &camera,
            colouring,
            background: &request.background,
            supersample: request.antialias,
            base_colour: request.surface_color_rgb,
            cap: cap.as_ref(),
        };
        let (rgb, raster_record) =
            render3d::rasterize(&RenderMesh { ..mesh.clone() }, &opts).map_err(contract)?;
        timing.push(("clip_and_raster", started.elapsed().as_secs_f64()));
        if let Some(c) = request.surface_color_rgb {
            surface_record.insert("display_color_rgb".into(), json!(c));
        }
        let after = models.status_value()?;
        if content_of(&before) != json!(expected) || content_of(&after) != json!(expected) {
            return Err(AgentError::refused(
                "the model changed while its 3d view was rendered",
            ));
        }
        let png = rgb.to_png();
        if !png.starts_with(b"\x89PNG\r\n\x1a\n") || png.len() > render3d::MAX_PNG_BYTES {
            return Err(AgentError::refused(
                "the bounded 3d renderer produced an invalid PNG",
            ));
        }
        mesh_record["vertices_after_clip"] = json!(mesh.vertices.len());
        mesh_record["triangles_after_clip"] = json!(mesh.faces.len());
        mesh_record["clip"] = request
            .clip
            .as_ref()
            .map_or(Value::Null, render3d::ClipPlane::to_json);
        mesh_record["max_triangles"] = json!(policy.max_triangles);
        sampling["bounds_source"] = json!(bounds_source);
        sampling["base_bounds_authoritative"] = json!(authoritative);
        sampling["source_representation"] = json!(if native {
            "declared_native_registered_model_parameter"
        } else if request.source["kind"] == "optimization_epoch" {
            "detached_optimization_epoch_bounded_resample"
        } else {
            "current_model_bounded_resample"
        });
        sampling["registered_native_values_used"] = json!(native);
        if bounds_source == "known_model_aabb_with_sampling_halo" {
            sampling["sampling_halo_fraction_per_axis"] = json!(0.025);
        }
        let bg_rgb = if request.background == "white" {
            json!([255, 255, 255])
        } else {
            json!([17, 25, 38])
        };
        Ok(json!({
            "schema": "implexity-rendered-3d/1", "kind": "render_3d", "truth_status": truth_status,
            "render_status": if native && request.source["kind"] == "optimization_epoch" {
                "native_registered_saved_epoch_render_only"
            } else if native { "native_registered_current_model_render_only" } else { "sampled_isosurface_preview" },
            "representation_truth": {
                "source_identity_bound": true, "registered_native_grid_used": native,
                "native_values_preserved_without_resampling": native,
                "surface_is_sampled_approximation": !native, "surface_is_exact_geometry": false,
                "surface_representation": if native { "native_lattice_piecewise_linear_isosurface" }
                                          else { "bounded_resampled_piecewise_linear_isosurface" },
                "render_only": true, "authoritative_for_acceptance_or_export": false,
            },
            "source": source_identity, "surface": surface_record, "color": color_record,
            "view": {"camera": camera.record, "crop_mm": crop, "width_px": request.width_px,
                     "height_px": request.height_px, "antialias": request.antialias,
                     "background": request.background},
            "sampling": sampling, "mesh": mesh_record, "timing_s": time_record(&timing),
            "presentation_smoothing": {"mode": "none", "geometry_moved": false, "normal_interpolation_only": true},
            "presentation": {"background": request.background, "background_rgb": bg_rgb},
            "raster": raster_record,
            "image": inline_image(&png, request.width_px, request.height_px),
        }))
    }
}

impl AgentManager {
    pub(crate) fn export_stl(&self, payload: &Map<String, Value>) -> AgentResult<Value> {
        let request =
            implexity_mesh::stl_export::normalise_request(&Value::Object(payload.clone()))
                .map_err(contract)?;
        let source = request["source"].clone();
        let (view, identity, truth_status, expected): (
            std::sync::Arc<dyn AgentModel>,
            Value,
            String,
            String,
        ) = if source["kind"] == "optimization_epoch" {
            let job_id = py_str(&source["job_id"]);
            let epoch = source["epoch"].as_i64().unwrap_or(0);
            let exported = match self.host.call(HostOp::ExportEpoch(job_id.clone(), epoch)) {
                Ok(v) => v,
                Err(AgentError::KeyError(_)) => {
                    return Err(AgentError::contract(format!(
                        "optimization job {job_id} has no published epoch {epoch}"
                    )));
                }
                Err(AgentError::Contract(m)) => return Err(AgentError::Contract(m)),
                Err(e) => return Err(e),
            };
            let view = self
                .host
                .detached_model(&exported["document"])
                .map_err(|e| {
                    AgentError::refused(format!(
                        "the exported epoch document does not build: {}",
                        e.message()
                    ))
                })?;
            let status = view
                .status()
                .map_err(|e| AgentError::refused(e.to_string()))?;
            if exported.get("content_id") != Some(&json!(status.content_id)) {
                return Err(AgentError::refused(
                    "the exported epoch document identity drifted",
                ));
            }
            let get = |k: &str| exported.get(k).cloned().unwrap_or(Value::Null);
            let identity = json!({
                "kind": "optimization_epoch", "provenance_kind": "detached_optimization_epoch",
                "structure_id": status.structure_id, "content_id": status.content_id,
                "job_id": get("job_id"), "epoch": get("epoch"), "solve_id": get("solve_id"),
                "design_state_id": get("design_state_id"),
                "checkpoint_sha256": exported.get("checkpoint").and_then(|c| c.get("sha256")).cloned().unwrap_or(Value::Null),
                "document_sha256": get("document_sha256"),
                "result_authority": get("result_authority"),
                "canonical_eligible": get("canonical_eligible"),
                "observational_only": get("observational_only"),
                "live_model_mutated": false,
            });
            let authority = exported
                .get("result_authority")
                .filter(|v| crate::pyval::truthy(v))
                .map_or_else(|| "undeclared".to_owned(), py_str);
            (
                view,
                identity,
                format!("optimization_epoch_{authority}"),
                status.content_id,
            )
        } else {
            let view = self.require_model()?;
            let before = view.status_value()?;
            if before.get("loaded") != Some(&Value::Bool(true)) {
                return Err(AgentError::contract("export_stl requires a loaded model"));
            }
            let (identity, truth) = self.render_source(&source, &before)?;
            let expected = identity.get("content_id").map(py_str).unwrap_or_default();
            (view, Value::Object(identity), truth, expected)
        };
        implexity_mesh::stl_export::export(
            view.as_ref(),
            &request,
            &identity,
            &truth_status,
            &expected,
        )
        .map_err(|e| match e {
            implexity_mesh::MeshError::Stale(m) => AgentError::refused(m),
            other => AgentError::contract(other.to_string()),
        })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn inspect_renderables(&self, payload: &Map<String, Value>) -> AgentResult<Value> {
        let models = self.require_model()?;
        let status = models.status_value()?;
        if status.get("loaded") != Some(&Value::Bool(true)) {
            return Err(AgentError::contract(
                "render discovery requires a loaded model",
            ));
        }
        let problem = self.current_problem();
        let registered = models.registered_fields().map_err(contract)?;
        let mut section_fields = vec![json!({"field": "model_boundary", "label": "Model boundary",
                                             "units": "mm", "suggested_palette": "material"})];
        section_fields.extend(registered.iter().cloned());
        let mut quality = Map::new();
        for (name, p) in render3d::QUALITY {
            quality.insert(
                name.into(),
                json!({"max_axis_samples": p.max_axis_samples, "max_samples": p.max_samples,
                       "max_triangles": p.max_triangles}),
            );
        }
        for (name, refinement) in render3d::GEOMETRY_QUALITIES {
            quality.insert(
                name.into(),
                json!({"spacing": "source_declared_resolution", "refinement": refinement,
                       "spacing_sources": ["node_render_sampling_spacing_mm_hook", "registered_field_spacing"],
                       "analytic_longest_axis_samples": render3d::GEOMETRY_ANALYTIC_AXIS_SAMPLES * refinement,
                       "max_samples": render3d::GEOMETRY_MAX_SAMPLES,
                       "max_triangles": render3d::GEOMETRY_MAX_TRIANGLES}),
            );
        }
        let mut natives: Vec<&str> = render3d::NATIVE_RESULT_QUALITIES.to_vec();
        natives.sort_unstable();
        for name in natives {
            quality.insert(
                name.into(),
                json!({"registered_source_required": true,
                       "allowed_sources": ["result_artifact", "declared_current_model_field"],
                       "current_model_declaration": "native_analysis_grid=true",
                       "source_values_resampled": false,
                       "max_source_samples": render3d::NATIVE_RESULT_MAX_SOURCE_SAMPLES,
                       "max_working_samples": render3d::NATIVE_RESULT_MAX_WORKING_SAMPLES,
                       "max_triangles": render3d::NATIVE_RESULT_MAX_TRIANGLES,
                       "effective_quality": "native"}),
            );
        }
        let semantics = implexity_render::rendering::coordinate_history_semantics();
        let mut result = json!({
            "schema": "implexity-render-catalogue/1",
            "model": {"structure_id": status.get("structure_id"), "content_id": status.get("content_id")},
            "section_rendering": {
                "planes": ["x", "y", "z"], "position_range": [0.0, 1.0],
                "fit_modes": {
                    "fixed_pixels": {"request": ["width_px", "height_px"], "default_dimensions_px": [320, 320],
                                     "dimension_range_px": [64, 384]},
                    "physical_aspect": {"request": {"fit": "physical_aspect", "max_dimension_px": 384},
                                        "max_dimension_range_px": [64, 384], "default_max_dimension_px": 384,
                                        "equal_physical_scale": true},
                },
                "max_pixels": RENDER_SECTION_MAX_PIXELS,
                "crop": {"supported": true, "request_field": "bbox_mm", "units": "mm"},
                "background": {"request_field": "background", "default": "dark", "allowed": ["dark", "white"]},
            },
            "section_fields": section_fields,
            "three_dimensional_rendering": {
                "surface_fields": section_fields,
                "color_fields": registered,
                "camera_presets": render3d::CAMERA_PRESETS,
                "explicit_camera_fields": ["eye_mm", "target_mm", "up", "fov_deg"],
                "quality": quality,
                "clip": {"supported": true, "request_field": "clip",
                         "section_cap": {"request_field": "clip.cap", "inside": ["auto", "below_iso", "above_iso"],
                                         "complement_field": "complement_color_rgb"}},
                "explicit_coloring": {"uniform": "surface_color_rgb", "piecewise_linear": "color_stops",
                                      "threshold_categories": "color_categories", "color_units": "display_srgb_0_255"},
                "crop": {"supported": true, "request_field": "crop_mm", "units": "mm"},
                "presentation_smoothing": {"default": "none", "allowed": ["none"]},
                "background": {"request_field": "background", "default": "dark", "allowed": ["dark", "white"]},
                "max_pixels": render3d::MAX_IMAGE_PIXELS,
                "dimension_range_px": [render3d::MIN_IMAGE_SIDE_PX, render3d::MAX_IMAGE_SIDE_PX],
                "antialias": {"request_field": "antialias", "default": 1, "allowed": render3d::ANTIALIAS_LEVELS,
                              "filter": "box_mean_in_linear_light", "max_raster_pixels": render3d::MAX_RASTER_PIXELS},
                "max_tile_pixel_tests": render3d::MAX_RASTER_TILE_SAMPLES,
            },
        });
        if let (Some(m), Value::Object(collections)) = (
            result.as_object_mut(),
            implexity_render::rendering::declared_collections(&problem),
        ) {
            for (k, v) in collections {
                m.insert(k, v);
            }
            m.insert(
                "optimization_history_rendering".into(),
                json!({"background": {"request_field": "background", "default": "dark", "allowed": ["dark", "white"]},
                       "dimension_range_px": {"width": [256, 768], "height": [192, 512]},
                       "coordinate_diagnostics": semantics}),
            );
            m.insert("history".into(), Value::Null);
            let derived = match models.geometry_model().and_then(|g| g.root()) {
                Some(root) => implexity_render::viewer_scene::derived_catalogue(
                    &implexity_render::model_fields::RootDerived(root),
                )
                .map_err(contract)?,
                None => Vec::new(),
            };
            m.insert(
                "native_viewer_rendering".into(),
                json!({"action": "render_viewer_snapshot",
                       "scene_schema": implexity_render::viewer_scene::SCHEMA,
                       "renderer": "native_webgl_raymarcher", "identity_required": true,
                       "capture_backends": {"browser": "native_webgl_raymarcher", "egl_offscreen": "native_shared_shader_egl"},
                       "backend_default": "browser",
                       "surfaces": ["solid", "complement"],
                       "projections": ["orthographic", "perspective"],
                       "section_fill": {"request_field": "clip.cap.complement_color_rgb", "color_units": "display_srgb_0_1"},
                       "derived_fields": derived, "registered_fields": registered,
                       "field_sources": "current_authoritative_model_only",
                       "arbitrary_saved_epoch_selection": false, "no_renderer_fallback": true}),
            );
            if let Some(job_id) = payload.get("job_id").filter(|v| crate::pyval::truthy(v)) {
                let info = self.host.call(HostOp::JobInfo(py_str(job_id)))?;
                let rows: Vec<Value> = info
                    .get("history")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                m.insert(
                    "history".into(),
                    json!({"job_id": job_id, "status": info.get("status"), "rows": rows.len(),
                           "series": implexity_render::rendering::available_history_series(&rows),
                           "coordinate_diagnostics": implexity_render::rendering::coordinate_history_semantics()}),
                );
            }
        }
        Ok(result)
    }

    pub(crate) fn render_optimization_history(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        let job_id = py_str(field(p, "job_id")?);
        let info = self.host.call(HostOp::JobInfo(job_id.clone()))?;
        let mut rows: Vec<Value> = info
            .get("history")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|r| r.is_object()).cloned().collect())
            .unwrap_or_default();
        if rows.is_empty() {
            return Err(AgentError::contract(
                "the optimization job has no renderable history",
            ));
        }
        let iteration = |index: usize, row: &Value| -> f64 {
            row.get("i")
                .or_else(|| row.get("iteration"))
                .and_then(Value::as_f64)
                .unwrap_or_else(|| {
                    #[allow(clippy::cast_precision_loss)]
                    let i = index as f64;
                    i
                })
        };
        let through = p
            .get("through_iteration")
            .filter(|v| !v.is_null())
            .and_then(Value::as_i64);
        if let Some(t) = through {
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let kept: Vec<Value> = rows
                .iter()
                .enumerate()
                .filter(|(i, r)| iteration(*i, r).trunc() as i64 <= t)
                .map(|(_, r)| r.clone())
                .collect();
            rows = kept;
            if rows.is_empty() {
                return Err(AgentError::contract(
                    "the requested optimization-history prefix has no rows",
                ));
            }
        }
        let available = implexity_render::rendering::available_history_series(&rows);
        let mut requested: Vec<String> = match p.get("series").filter(|v| crate::pyval::truthy(v)) {
            Some(Value::Array(a)) => a.iter().map(py_str).collect(),
            _ => ["objective", "gradient_norm"]
                .iter()
                .filter(|x| available.iter().any(|a| a == *x))
                .map(|x| (*x).to_owned())
                .collect(),
        };
        if requested.is_empty() {
            requested = available.iter().take(2).cloned().collect();
        }
        let mut missing: Vec<&String> = requested
            .iter()
            .filter(|r| !available.contains(r))
            .collect();
        missing.sort();
        missing.dedup();
        if !missing.is_empty() {
            return Err(AgentError::contract(format!(
                "unknown optimization history series {}",
                implexity_core::pyobj::list_repr(&missing)
            )));
        }
        let width = p.get("width_px").and_then(Value::as_i64).unwrap_or(640);
        let height = p.get("height_px").and_then(Value::as_i64).unwrap_or(360);
        if width * height > 300_000 {
            return Err(AgentError::contract(
                "optimization history raster exceeds 300000 pixels",
            ));
        }
        let bg = p
            .get("background")
            .cloned()
            .unwrap_or_else(|| json!("dark"));
        let Some(bg) = bg
            .as_str()
            .filter(|b| *b == "dark" || *b == "white")
            .map(str::to_owned)
        else {
            return Err(AgentError::contract(
                "optimization history background must be dark or white",
            ));
        };
        let events: Vec<ChartEvent> = p
            .get("events")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|e| ChartEvent {
                        iteration: e.get("iteration").and_then(Value::as_i64).unwrap_or(0),
                        label: e.get("label").map(py_str).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let x: Vec<f64> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| iteration(i, r))
            .collect();
        let lines: Vec<(String, Vec<f64>)> = requested
            .iter()
            .map(|name| {
                (
                    name.clone(),
                    implexity_render::rendering::history_values(&rows, name),
                )
            })
            .collect();
        let (w, h) = (
            usize::try_from(width).unwrap_or(0),
            usize::try_from(height).unwrap_or(0),
        );
        let (img, rendered, markers) = raster::history_chart_rgb(
            &x,
            &lines,
            &events,
            w,
            h,
            "OPTIMIZATION HISTORY",
            background(&bg),
        );
        let png = img.to_png();
        #[allow(clippy::cast_possible_truncation)]
        let (first, last) = (
            x.first().copied().unwrap_or(0.0).trunc() as i64,
            x.last().copied().unwrap_or(0.0).trunc() as i64,
        );
        let bg_rgb = if bg == "white" {
            json!([255, 255, 255])
        } else {
            json!([10, 16, 25])
        };
        Ok(json!({
            "schema": "implexity-rendered-optimization-history/1", "kind": "render_optimization_history",
            "truth_status": "history_record",
            "source": {"job_id": job_id, "status": info.get("status"), "history_rows": rows.len(),
                       "first_iteration": first, "last_iteration": last, "through_iteration": through},
            "series": rendered.iter().map(|(name, colour, lo, hi)| json!({"name": name, "colour_rgb": colour, "value_range": [lo, hi]})).collect::<Vec<_>>(),
            "coordinate_diagnostics": implexity_render::rendering::coordinate_history_semantics(),
            "events": markers.iter().map(|m| json!({"iteration": m.iteration, "label": m.label})).collect::<Vec<_>>(),
            "presentation": {"background": bg, "background_rgb": bg_rgb},
            "image": inline_image(&png, w, h),
        }))
    }

    pub(crate) fn prepare_viewer_scene(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        let models = self.require_model()?;
        let scene = field(p, "scene")?;
        let expected = py_str(field(p, "expected_content_id")?);
        let root = models.geometry_model().and_then(|g| g.root());
        let derived = root.map(implexity_render::model_fields::RootDerived);
        implexity_render::viewer_scene::prepare_scene(
            models.as_ref(),
            derived
                .as_ref()
                .map(|d| d as &dyn implexity_render::viewer_scene::DerivedFieldSource),
            scene,
            &expected,
        )
        .map_err(contract)
    }

    pub(crate) fn render_viewer_snapshot(&self, p: &Map<String, Value>) -> AgentResult<Value> {
        crate::capture::capture(self, &Value::Object(p.clone()))
    }
}
