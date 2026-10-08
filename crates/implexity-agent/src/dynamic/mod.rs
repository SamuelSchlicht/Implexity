// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



pub mod schema;
pub mod select;
pub mod view;

use std::collections::BTreeMap;

use base64::Engine as _;
use implexity_mesh::raster::RgbImage;
use implexity_render::dynamic::chart::{ChartOptions, Series, kymograph, line_chart, phase_portrait};
use implexity_render::dynamic::compose::sheet;
use implexity_render::dynamic::encode::{Animation, AnimationFormat};
use implexity_render::dynamic::grid::{GridField, Reduce};
use implexity_render::dynamic::plane::Theme;
use implexity_runtime::dynamic_frames::analysis::{
    Window as SpectrumWindow, amplitude_spectrum, decimate_minmax, period_from_crossings, phase_average,
    phase_of, pulse_measures, spectral_peaks, statistics,
};
use implexity_runtime::dynamic_frames::catalogue::{Catalogue, StoreId, is_job_id};
use implexity_runtime::dynamic_frames::manifest::FieldKind;
use implexity_runtime::dynamic_frames::store::DynamicStore;
use serde_json::{Map, Value, json};

use crate::contracts::{EXTENSION, contracts};
use crate::error::{AgentError, AgentResult};
use select::{CycleSel, Selection};
use view::{FrameValues, Mode, Precomputed, Renderer, Stored, View};

pub const ACTIONS: [&str; 5] = [
    "inspect_dynamic_results",
    "read_time_series",
    "render_dynamic_frames",
    "render_cycle_animation",
    "export_animation",
];

pub const RENDER_SCHEMA: &str = "implexity-dynamic-render/1";
pub const MAX_INLINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_INLINE_PIXELS: usize = 4 * 1024 * 1024;
pub const MAX_EXPORT_BYTES: usize = 48 * 1024 * 1024;
pub const MAX_TILES: usize = 24;
pub const MAX_KYMOGRAPH_ROWS: usize = 2048;

#[must_use]
pub fn is_action(name: &str) -> bool {
    ACTIONS.contains(&name)
}

fn contract(e: impl std::fmt::Display) -> AgentError {
    AgentError::contract(e.to_string())
}

fn failed(e: impl std::fmt::Display) -> AgentError {
    AgentError::failed(e.to_string())
}



pub fn validate(action: &str, p: &Map<String, Value>) -> AgentResult<()> {
    let c = contracts()?;
    let contract_of =
        c.get(action).ok_or_else(|| AgentError::contract(format!("unknown dynamic action {action}")))?;
    schema::validate(&contract_of.input_schema, &Value::Object(p.clone()), "payload")
        .map_err(AgentError::contract)?;
    match action {
        "inspect_dynamic_results" if p.contains_key("job_id") && p.contains_key("store") => {
            return Err(AgentError::contract("inspect_dynamic_results takes job_id or store, not both"));
        }
        "render_dynamic_frames" => {
            if p.contains_key("store") == p.contains_key("compare_stores") {
                return Err(AgentError::contract(
                    "render_dynamic_frames needs exactly one of store or compare_stores",
                ));
            }
            let v = View::from_payload(p, (480, 360))?;
            if v.mode == Mode::Kymograph && p.contains_key("compare_stores") {
                return Err(AgentError::contract("a kymograph renders one store"));
            }
            if p.get("renderer").and_then(Value::as_str) == Some("gui_panel")
                && (v.mode == Mode::Kymograph
                    || p.contains_key("compare_stores")
                    || p.contains_key("value_range")
                    || v.line.is_some()
                    || p.get("iso").and_then(|i| i.get("value")).is_some())
            {
                return Err(AgentError::contract(
                    "renderer gui_panel shows one store's section, iso-surface or phase average as the GUI panel does (no kymographs, comparisons, explicit value ranges or iso values)",
                ));
            }
            Selection::parse(p.get("frames"), 8)?;
        }
        "render_cycle_animation" | "export_animation" => {
            let v = View::from_payload(p, (480, 360))?;
            if v.mode == Mode::Kymograph {
                return Err(AgentError::contract("a kymograph is a still image; use render_dynamic_frames"));
            }
        }
        _ => {}
    }
    Ok(())
}



pub fn dispatch(cat: &Catalogue, action: &str, p: &Map<String, Value>) -> AgentResult<Value> {
    dispatch_with_origin(cat, None, action, p)
}



pub fn dispatch_with_origin(
    cat: &Catalogue,
    origin: Option<&str>,
    action: &str,
    p: &Map<String, Value>,
) -> AgentResult<Value> {
    match action {
        "inspect_dynamic_results" => inspect(cat, p),
        "read_time_series" => read_time_series(cat, p),
        "render_dynamic_frames" if p.get("renderer").and_then(Value::as_str) == Some("gui_panel") => {
            render_panel(cat, origin, p)
        }
        "render_dynamic_frames" => render_frames(cat, p),
        "render_cycle_animation" => render_animation(cat, p, false),
        "export_animation" => render_animation(cat, p, true),
        other => Err(AgentError::refused(format!("unimplemented dynamic action {other}"))),
    }
}

fn open(cat: &Catalogue, id: &str) -> AgentResult<(StoreId, DynamicStore)> {
    let sid = StoreId::parse(id).map_err(contract)?;
    let store = cat.open(&sid).map_err(contract)?;
    Ok((sid, store))
}

#[must_use]
pub fn inline_image(bytes: &[u8], mime: &str, width: usize, height: usize) -> Value {
    let mut v = json!({"schema": "implexity-inline-image/1", "mime_type": mime,
                       "data_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
                       "bytes": bytes.len(), "sha256": implexity_io::digest::sha256_hex(bytes),
                       "width_px": width, "height_px": height});
    if mime != "image/png" {
        v["extension"] = json!(EXTENSION);
    }
    v
}

fn png_record(img: &RgbImage) -> AgentResult<Value> {
    if img.width * img.height > MAX_INLINE_PIXELS {
        return Err(AgentError::contract(format!(
            "the image would have {}x{} pixels (at most {MAX_INLINE_PIXELS}); request fewer frames or smaller tiles",
            img.width, img.height
        )));
    }
    let png = img.to_png();
    if png.len() > MAX_INLINE_BYTES {
        return Err(AgentError::refused("the rendered PNG exceeds the inline image bound"));
    }
    Ok(inline_image(&png, "image/png", img.width, img.height))
}

fn inspect(cat: &Catalogue, p: &Map<String, Value>) -> AgentResult<Value> {
    let limit = p.get("limit").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(50);
    let Some(id) = p.get("store").and_then(Value::as_str) else {
        let job = p.get("job_id").and_then(Value::as_str).filter(|j| is_job_id(j));
        let mut listing = cat.list(job, limit);
        listing["kind"] = json!("inspect_dynamic_results");
        if listing["count"] == 0 {
            listing["note"] = json!(
                "no dynamic results are captured yet: dynamic providers write stores during evaluations and optimisation jobs"
            );
        }
        return Ok(listing);
    };
    let (sid, store) = open(cat, id)?;
    let m = store.manifest();
    let by_kind = |k: FieldKind| -> Vec<&str> {
        m.fields.iter().filter(|f| f.kind == k).map(|f| f.name.as_str()).collect()
    };
    let cycles: Vec<Value> =
        select::cycles(&store).iter().map(|(c, v)| json!({"cycle": c, "frames": v.len()})).collect();
    let table = store.series().map_err(failed)?;
    let series: Map<String, Value> =
        table.names.iter().zip(&table.columns).map(|(n, col)| (n.clone(), statistics(col))).collect();
    let mut out = store.describe();
    out["schema"] = json!("implexity-dynamic-result-store/1");
    out["extension"] = json!(EXTENSION);
    out["kind"] = json!("inspect_dynamic_results");
    out["store"] = json!(sid.as_string());
    out["retained_cycles"] = json!(cycles);
    out["series_statistics"] = Value::Object(series);
    out["series_samples"] = json!(table.t.len());

    let mesh_bounds: Map<String, Value> = m
        .meshes
        .iter()
        .filter_map(|x| {
            let d = store.mesh(&x.name).ok()?;
            let k = x.cell.dims();
            let mut lo = vec![f64::INFINITY; k];
            let mut hi = vec![f64::NEG_INFINITY; k];
            for p in d.points.chunks_exact(k) {
                for a in 0..k {
                    lo[a] = lo[a].min(p[a]);
                    hi[a] = hi[a].max(p[a]);
                }
            }
            Some((x.name.clone(), json!([lo, hi])))
        })
        .collect();
    out["renderable"] = json!({
        "scalar": by_kind(FieldKind::Scalar), "vector": by_kind(FieldKind::Vector),
        "displacement": by_kind(FieldKind::Displacement), "occupancy": by_kind(FieldKind::Occupancy),
        "three_dimensional": m.fields.iter().filter(|f| m.location(&f.grid).is_some_and(|l| l.dims() == 3)).map(|f| f.name.as_str()).collect::<Vec<_>>(),
        "on_meshes": m.fields.iter().filter(|f| m.location(&f.grid).is_some_and(|l| l.mesh().is_some())).map(|f| f.name.as_str()).collect::<Vec<_>>(),
        "mesh_bounds": mesh_bounds,
        "mesh_attributes": m.meshes.iter().flat_map(|x| x.attributes.iter().map(|a| a.name.as_str())).collect::<Vec<_>>(),
        "cell_grids": m.grids.iter().flat_map(|n| m.grids.iter().filter(|c| view::is_cell_grid(n, c)).map(|c| json!({"nodes": n.name, "cells": c.name}))).collect::<Vec<_>>(),
        "derived": m.fields.iter().filter(|f| f.kind.is_vector() && m.grid(&f.grid).is_some()).flat_map(|f| ["curl", "q_criterion", "divergence"].map(|op| format!("{op}:{}", f.name))).collect::<Vec<_>>(),
    });

    let offset = store.frames().len().saturating_sub(2048);
    out["frames_index"] = json!(
        store.frames()[offset..].iter().map(|f| json!([f.seq, f.t, f.phase, f.cycle])).collect::<Vec<_>>()
    );
    out["frames_index_offset"] = json!(offset);
    out["viewer_url"] = json!(format!("/viewer/rust_ext/dynamic_results.html?store={}", sid.as_string()));
    out["frames_index_columns"] = json!(["seq", "t", "phase", "cycle"]);
    Ok(out)
}

fn window_indices(t: &[f64], p: &Map<String, Value>, period: Option<f64>) -> AgentResult<(usize, usize)> {
    let last = t.last().copied().unwrap_or(0.0);
    let mut lo = p.get("t_start").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY);
    let hi = p.get("t_end").and_then(Value::as_f64).unwrap_or(f64::INFINITY);
    if let Some(n) = p.get("last_cycles").and_then(Value::as_u64) {
        let period = period.ok_or_else(|| AgentError::contract("last_cycles needs a store with a period"))?;
        lo = lo.max(last - n as f64 * period);
    }
    let a = t.partition_point(|x| *x < lo);
    let b = t.partition_point(|x| *x <= hi);
    if b <= a {
        return Err(AgentError::contract("the time window holds no samples"));
    }
    Ok((a, b))
}

fn uniform(t: &[f64], x: &[f64]) -> (Vec<f64>, f64) {
    let mut d: Vec<f64> = t.windows(2).map(|w| w[1] - w[0]).filter(|d| *d > 0.0).collect();
    d.sort_by(f64::total_cmp);
    let dt = d.get(d.len() / 2).copied().unwrap_or(1.0);
    let span = t.last().copied().unwrap_or(0.0) - t.first().copied().unwrap_or(0.0);
    let regular = d.first().is_some_and(|a| (d[d.len() - 1] - a).abs() <= 1e-6 * dt);
    if regular {
        return (x.to_vec(), dt);
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = ((span / dt).floor() as usize + 1).min(1 << 22);
    let mut out = Vec::with_capacity(n);
    let mut j = 0;
    for k in 0..n {
        let tk = t[0] + k as f64 * dt;
        while j + 2 < t.len() && t[j + 1] < tk {
            j += 1;
        }
        let (t0, t1) = (t[j], t[(j + 1).min(t.len() - 1)]);
        let s = if t1 > t0 { ((tk - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
        out.push(x[j] + s * (x[(j + 1).min(t.len() - 1)] - x[j]));
    }
    (out, dt)
}

type SpectrumRows = (Vec<f64>, Vec<f64>, Vec<(f64, f64)>);

#[allow(clippy::too_many_lines)]
fn read_time_series(cat: &Catalogue, p: &Map<String, Value>) -> AgentResult<Value> {
    let (sid, store) = open(cat, p.get("store").and_then(Value::as_str).unwrap_or_default())?;
    let m = store.manifest();
    let table = store.series().map_err(failed)?;
    if table.t.is_empty() {
        return Err(AgentError::refused("the store holds no series samples"));
    }
    let names: Vec<String> = match p.get("series").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        None => table.names.iter().take(16).cloned().collect(),
    };
    for n in &names {
        if table.column(n).is_none() {
            return Err(AgentError::contract(format!(
                "the store has no series {n:?}; it has {}",
                table.names.join(", ")
            )));
        }
    }
    let (a, b) = window_indices(&table.t, p, store.period())?;
    let t = &table.t[a..b];
    let max_points =
        p.get("max_points").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(512);
    let mut series = Map::new();
    let mut decimated: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for n in &names {
        let col = &table.column(n).unwrap_or_default()[a..b];
        let (dt_, dx) = decimate_minmax(t, col, max_points);
        let spec = m.series.iter().find(|s| &s.name == n);
        series.insert(
            n.clone(),
            json!({"label": spec.map(|s| s.label.clone()), "unit": spec.map(|s| s.unit.clone()),
                   "role": spec.map(|s| s.role.clone()), "statistics": statistics(col),
                   "t": dt_, "values": dx, "decimated": dt_.len() < col.len()}),
        );
        decimated.insert(n.clone(), (dt_, dx));
    }
    let mut out = json!({
        "schema": "implexity-dynamic-time-series/1", "extension": EXTENSION, "kind": "read_time_series",
        "store": sid.as_string(), "time_unit": m.time.unit, "period": store.period(),
        "window": {"t_start": t[0], "t_end": t[t.len() - 1], "samples": t.len()},
        "series": series, "truth_status": "stored_provider_series",
    });
    let mut spectra: BTreeMap<String, SpectrumRows> = BTreeMap::new();
    if let Some(sp) = p.get("spectrum").and_then(Value::as_object) {
        let window = if sp.get("window").and_then(Value::as_str) == Some("rectangular") {
            SpectrumWindow::Rectangular
        } else {
            SpectrumWindow::Hann
        };
        let npeaks =
            sp.get("peaks").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(5);
        let mut rec = Map::new();
        for n in &names {
            let col = &table.column(n).unwrap_or_default()[a..b];
            let (x, dt) = uniform(t, col);
            let s = amplitude_spectrum(&x, dt, window).map_err(contract)?;
            let peaks = spectral_peaks(&s, npeaks);
            let (df, da) = decimate_minmax(&s.frequency, &s.amplitude, max_points);
            rec.insert(n.clone(), json!({
                "resolution": s.resolution, "sample_step": dt, "window": if window == SpectrumWindow::Hann { "hann" } else { "rectangular" },
                "peaks": peaks.iter().map(|(f, a)| json!({"frequency": f, "amplitude": a})).collect::<Vec<_>>(),
                "dominant_frequency": peaks.first().map(|x| x.0),
                "frequency": df, "amplitude": da,
            }));
            spectra.insert(n.clone(), (s.frequency, s.amplitude, peaks));
        }
        out["spectrum"] = Value::Object(rec);
    }
    if let Some(pr) = p.get("period").and_then(Value::as_object) {
        let n = pr.get("series").and_then(Value::as_str).unwrap_or_default();
        let col =
            table.column(n).ok_or_else(|| AgentError::contract(format!("the store has no series {n:?}")))?;
        let est = period_from_crossings(t, &col[a..b], pr.get("level").and_then(Value::as_f64), 64)
            .map_err(contract)?;
        out["period_estimate"] = json!({"series": n, "period": est.period, "frequency": 1.0 / est.period,
            "jitter": est.jitter, "intervals": est.intervals, "last_upcrossing": est.origin,
            "recorded_period": store.period()});
    }
    if let Some(pu) = p.get("pulse").and_then(Value::as_object) {
        let n = pu.get("series").and_then(Value::as_str).unwrap_or_default();
        let col =
            table.column(n).ok_or_else(|| AgentError::contract(format!("the store has no series {n:?}")))?;
        let threshold = pu.get("threshold").and_then(Value::as_f64).unwrap_or(0.0);
        let mut rec = pulse_measures(t, &col[a..b], threshold);
        rec["series"] = json!(n);
        out["pulse"] = rec;
    }
    if let Some(ch) = p.get("chart").and_then(Value::as_object) {
        let kind = ch.get("kind").and_then(Value::as_str).unwrap_or("time_series");
        let mut o = ChartOptions::new("", "", "");
        o.width =
            ch.get("width_px").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(720);
        o.height =
            ch.get("height_px").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(360);
        o.theme =
            Theme::parse(ch.get("background").and_then(Value::as_str).unwrap_or("dark")).map_err(contract)?;
        o.log_y = ch.get("log_y").and_then(Value::as_bool).unwrap_or(false);
        let unit =
            |n: &str| m.series.iter().find(|s| s.name == n).map_or_else(String::new, |s| s.unit.clone());
        let img = match kind {
            "spectrum" => {
                if spectra.is_empty() {
                    return Err(AgentError::contract("a spectrum chart needs the spectrum option"));
                }
                o.title = format!("amplitude spectrum  {}", sid.as_string());
                o.x_label = format!("frequency [1/{}]", m.time.unit);
                o.y_label = "amplitude".into();
                o.markers = spectra
                    .values()
                    .flat_map(|(_, _, pk)| {
                        pk.iter().take(3).map(|(f, a)| (*f, *a, implexity_render::dynamic::draw::fmt_num(*f)))
                    })
                    .collect();
                let rows: Vec<Series<'_>> = spectra
                    .iter()
                    .map(|(n, (f, a, _))| Series { label: n.clone(), x: &f[1..], y: &a[1..] })
                    .collect();
                line_chart(&rows, &o).map_err(contract)?
            }
            "phase_portrait" => {
                let x = ch
                    .get("x")
                    .and_then(Value::as_str)
                    .or(names.first().map(String::as_str))
                    .unwrap_or_default()
                    .to_owned();
                let xcol = &table
                    .column(&x)
                    .ok_or_else(|| AgentError::contract(format!("the store has no series {x:?}")))?
                    [a..b];
                let (ycol, ylabel): (Vec<f64>, String) = if ch
                    .get("derivative")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || !ch.contains_key("y")
                {
                    let n = t.len();
                    let d = (0..n)
                        .map(|j| {
                            let (i0, i1) = (j.saturating_sub(1), (j + 1).min(n - 1));
                            if t[i1] > t[i0] { (xcol[i1] - xcol[i0]) / (t[i1] - t[i0]) } else { f64::NAN }
                        })
                        .collect();
                    (d, format!("d{x}/dt"))
                } else {
                    let y = ch.get("y").and_then(Value::as_str).unwrap_or_default();
                    (
                        table
                            .column(y)
                            .ok_or_else(|| AgentError::contract(format!("the store has no series {y:?}")))?
                            [a..b]
                            .to_vec(),
                        format!("{y} [{}]", unit(y)),
                    )
                };
                let phases: Option<Vec<f64>> = store
                    .period()
                    .map(|per| t.iter().map(|tt| phase_of(*tt, per, store.phase_origin())).collect());
                o.title = format!("phase portrait  {}", sid.as_string());
                o.x_label = format!("{x} [{}]", unit(&x));
                o.y_label = ylabel;
                phase_portrait(xcol, &ycol, phases.as_deref(), &o).map_err(contract)?
            }
            _ => {
                o.title = format!("time series  {}", sid.as_string());
                o.x_label = format!("t [{}]", m.time.unit);
                o.y_label = names.iter().map(|n| format!("{n} [{}]", unit(n))).collect::<Vec<_>>().join(", ");
                o.cursor_x = ch.get("cursor_t").and_then(Value::as_f64);
                let rows: Vec<Series<'_>> =
                    decimated.iter().map(|(n, (tt, xx))| Series { label: n.clone(), x: tt, y: xx }).collect();
                line_chart(&rows, &o).map_err(contract)?
            }
        };
        out["image"] = png_record(&img)?;
    }
    Ok(out)
}

fn needed_fields(store: &DynamicStore, v: &View) -> AgentResult<Vec<String>> {
    let v = &view::with_defaults(store.manifest(), v.clone())?;
    let mut names = vec![v.field.clone()];
    names.extend(v.occupancy.clone());
    if let Some(f) = &v.flow {
        names.push(f.field.clone());
    }
    if let Some(d) = &v.deformation {
        names.extend(Some(d.field.clone()).filter(|f| !f.is_empty()));
        names.extend(d.mask.clone());
        names.extend(d.colour.clone());
    }
    let mut out = names
        .iter()
        .map(|n| view::resolve(store.manifest(), n).map(|r| r.stored))
        .collect::<AgentResult<Vec<_>>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

fn averaged(store: &DynamicStore, view: &View) -> AgentResult<Vec<(Precomputed, String, usize)>> {
    let mut per_field = BTreeMap::new();
    let mut counts = Vec::new();
    let mut statics = Vec::new();
    for name in needed_fields(store, view)? {
        if store.manifest().field(&name).is_none() {

            statics.push((name.clone(), store.read_attribute(&name).map_err(failed)?));
            continue;
        }
        let avg = phase_average(store, &name, view.phase_bins, None, None).map_err(contract)?;
        counts.clone_from(&avg.counts);
        per_field.insert(name, avg.mean);
    }
    let bins = view.phase_bins;
    Ok((0..bins)
        .filter(|&b| counts.get(b).is_some_and(|c| *c > 0))
        .map(|b| {
            let values = per_field
                .iter()
                .filter_map(|(n, means)| means[b].clone().map(|v| (n.clone(), v)))
                .chain(statics.iter().cloned())
                .collect();
            let caption = format!(
                "phase {:.3}-{:.3} ({} frames)",
                b as f64 / bins as f64,
                (b + 1) as f64 / bins as f64,
                counts[b]
            );
            (Precomputed(values), caption, counts[b])
        })
        .collect())
}

fn frame_row(sid: &StoreId, f: &implexity_runtime::dynamic_frames::store::FrameEntry) -> Value {
    json!({"store": sid.as_string(), "seq": f.seq, "t": f.t, "phase": f.phase, "cycle": f.cycle})
}

fn kymograph_image(
    sid: &StoreId,
    store: &DynamicStore,
    v: &View,
    p: &Map<String, Value>,
) -> AgentResult<(RgbImage, Value, Value)> {
    let m = store.manifest();
    let spec = view::resolve(m, &v.field)?;
    let fi = m.field(&spec.stored).map(|(i, _)| i).ok_or_else(|| failed("stored field missing"))?;
    let Some(grid) = spec.place.grid() else {
        return Err(AgentError::contract(format!(
            "a kymograph samples a field on a grid; {} lives on a mesh",
            v.field
        )));
    };
    let comps = spec.components;
    let reduce = match v.component {
        Some(c) if comps > 1 && c < comps => Reduce::Component(c),
        _ if comps > 1 => Reduce::Magnitude,
        _ => Reduce::Component(0),
    };
    let (lo, hi) = grid.bounds();
    let (from, to) = match &v.line {
        Some((a, b)) if a.len() == grid.dims() => (a.clone(), b.clone()),
        Some(_) => {
            return Err(AgentError::contract(format!(
                "the kymograph line needs {} coordinates per point",
                grid.dims()
            )));
        }
        None => {
            let mid: Vec<f64> = lo.iter().zip(&hi).map(|(a, b)| 0.5 * (a + b)).collect();
            let mut a = mid.clone();
            let mut b = mid;
            a[0] = lo[0];
            b[0] = hi[0];
            (a, b)
        }
    };
    let frames: Vec<&implexity_runtime::dynamic_frames::store::FrameEntry> = if p.contains_key("frames") {
        let (sel, cyc) = Selection::parse(p.get("frames"), 8)?;
        select::select(store, &sel, cyc)?
    } else {
        store.frames().iter().collect()
    };
    let step = frames.len().div_ceil(MAX_KYMOGRAPH_ROWS).max(1);
    let mut rows = Vec::new();
    let mut times = Vec::new();
    for f in frames.iter().step_by(step) {
        let mut g = GridField::new(
            grid.shape.clone(),
            grid.spacing.clone(),
            grid.origin.clone(),
            spec.stored_components,
            store.read_field(f, fi).map_err(failed)?,
        )
        .map_err(contract)?;
        if let Some(op) = spec.derived {
            g = g.derive(op).map_err(contract)?;
        }
        rows.push(g.line(&from, &to, v.samples, reduce));
        times.push(f.t);
    }
    let map =
        v.colormap.unwrap_or_else(|| implexity_render::dynamic::colormap::Colormap::for_hint(spec.palette));
    let (a, b) = v.value_range.unwrap_or_else(|| {
        let finite = rows.iter().flatten().filter(|x| x.is_finite());
        let (l, h) = finite.fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), x| (l.min(*x), h.max(*x)));
        if l > h { (0.0, 1.0) } else { (l, h) }
    });
    let scale = if map == implexity_render::dynamic::colormap::Colormap::CoolWarm && v.value_range.is_none() {
        implexity_render::dynamic::colormap::Scale::symmetric(map, a, b)
    } else {
        implexity_render::dynamic::colormap::Scale::new(map, a, b)
    };
    let length = from.iter().zip(&to).map(|(x, y)| (y - x) * (y - x)).sum::<f64>().sqrt();
    let mut o = ChartOptions::new(
        &format!("{} [{}] space-time  {}", spec.label, spec.unit, sid.as_string()),
        &format!("distance along the line [{}]", grid.unit),
        &format!("t [{}] (down)", m.time.unit),
    );
    o.width = v.width;
    o.height = v.height;
    o.theme = v.theme;
    let img = kymograph(&rows, &times, (0.0, length), &scale, &o).map_err(contract)?;
    let record = json!({"from": from, "to": to, "samples": v.samples, "rows": rows.len(), "row_stride": step,
                        "t_range": [times.first(), times.last()], "display_range": [scale.lo, scale.hi], "colormap": scale.map.name()});
    let field = json!({"name": v.field, "label": spec.label, "unit": spec.unit, "kind": spec.kind.as_str()});
    Ok((img, record, field))
}

#[allow(clippy::too_many_lines)]
fn render_frames(cat: &Catalogue, p: &Map<String, Value>) -> AgentResult<Value> {
    let view = View::from_payload(p, (480, 360))?;
    let ids: Vec<String> =
        match (p.get("store").and_then(Value::as_str), p.get("compare_stores").and_then(Value::as_array)) {
            (Some(s), _) => vec![s.to_owned()],
            (None, Some(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            _ => return Err(AgentError::contract("render_dynamic_frames needs store or compare_stores")),
        };
    let theme = view.theme;
    let mut base = json!({"schema": RENDER_SCHEMA, "extension": EXTENSION, "kind": "render_dynamic_frames",
                          "truth_status": "stored_provider_frames", "render_status": "rendered_from_store",
                          "view": view.record()});
    if view.mode == Mode::Kymograph {
        let (sid, store) = open(cat, &ids[0])?;
        let (img, record, field) = kymograph_image(&sid, &store, &view, p)?;
        base["store"] = json!(sid.as_string());
        base["kymograph"] = record;
        base["field"] = field;
        base["image"] = png_record(&img)?;
        return Ok(base);
    }
    let mut columns: Vec<Vec<(RgbImage, String)>> = Vec::new();
    let mut frames_rec = Vec::new();
    let mut field_rec = Value::Null;
    for id in &ids {
        let (sid, store) = open(cat, id)?;
        let mut tiles = Vec::new();
        if view.mode == Mode::PhaseAverage {
            let bins = averaged(&store, &view)?;
            let values: Vec<&dyn FrameValues> = bins.iter().map(|(v, _, _)| v as &dyn FrameValues).collect();
            let r = Renderer::new(&store, &sid.as_string(), view.clone(), &values)?;
            for (v, caption, count) in &bins {
                tiles.push((
                    r.render(v, caption).map_err(|e| AgentError::contract(e.to_string()))?,
                    caption.clone(),
                ));
                frames_rec
                    .push(json!({"store": sid.as_string(), "phase_bin": caption, "frames_averaged": count}));
            }
            field_rec = r.field_record();
        } else {
            let (sel, cycle) = Selection::parse(p.get("frames"), if ids.len() > 1 { 1 } else { 8 })?;
            let frames = select::select(&store, &sel, cycle)?;
            let stored: Vec<Stored<'_>> = frames.iter().map(|f| Stored { store: &store, frame: f }).collect();
            let values: Vec<&dyn FrameValues> = stored.iter().map(|s| s as &dyn FrameValues).collect();
            let r = Renderer::new(&store, &sid.as_string(), view.clone(), &values)?;
            for (s, f) in stored.iter().zip(&frames) {
                let caption = r.caption(f);
                tiles.push((
                    r.render(s, &caption)?,
                    if ids.len() > 1 { format!("{} {caption}", sid.as_string()) } else { caption },
                ));
                frames_rec.push(frame_row(&sid, f));
            }
            field_rec = r.field_record();
        }
        columns.push(tiles);
    }
    let rows = columns.iter().map(Vec::len).min().unwrap_or(0);
    let mut tiles = Vec::new();
    for k in 0..rows {
        for col in &columns {
            tiles.push(col[k].clone());
        }
    }
    if tiles.len() > MAX_TILES {
        return Err(AgentError::contract(format!("{} tiles requested; at most {MAX_TILES}", tiles.len())));
    }
    let ncols = if ids.len() > 1 {
        ids.len()
    } else {
        p.get("columns")
            .and_then(Value::as_u64)
            .and_then(|x| usize::try_from(x).ok())
            .unwrap_or_else(|| tiles.len().min(4))
    };
    let img =
        if tiles.len() == 1 { tiles[0].0.clone() } else { sheet(&tiles, ncols, theme).map_err(contract)? };
    if ids.len() > 1 {
        base["stores"] = json!(ids);
    } else {
        base["store"] = json!(ids[0]);
    }
    base["field"] = field_rec;
    base["frames"] = json!(frames_rec);
    base["sheet"] = json!({"tiles": tiles.len(), "columns": ncols.min(tiles.len()), "tile_px": [tiles.first().map(|t| t.0.width), tiles.first().map(|t| t.0.height)]});
    base["image"] = png_record(&img)?;
    Ok(base)
}

fn render_panel(cat: &Catalogue, origin: Option<&str>, p: &Map<String, Value>) -> AgentResult<Value> {
    use implexity_render::capture::CaptureError;
    use implexity_render::capture::panel::{capture_panel, panel_link};
    let Some(origin) = origin.filter(|o| !o.is_empty()) else {
        return Err(AgentError::refused(
            "the dynamic panel capture needs this service's loopback viewer origin (serve on a loopback address)",
        ));
    };
    let view = View::from_payload(p, (1280, 900))?;
    let id = p.get("store").and_then(Value::as_str).unwrap_or_default();
    let (sid, store) = open(cat, id)?;

    let (sel, cycle) = Selection::parse(p.get("frames"), 1)?;
    let frames = select::select(&store, &sel, cycle)?;
    let first = frames[0];
    let index = store.frames().iter().position(|f| f.seq == first.seq).unwrap_or(0);
    Renderer::new(&store, &sid.as_string(), view.clone(), &[])?;
    let mut pairs: Vec<(&str, String)> = vec![("store", sid.as_string()), ("field", view.field.clone())];
    let text = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_owned);
    for k in ["component", "mode", "colormap", "range", "background", "plane", "occupancy"] {
        if let Some(v) = text(k) {
            pairs.push((k, v));
        }
    }
    if let Some(x) = p.get("position").and_then(Value::as_f64) {
        pairs.push(("position", x.to_string()));
    }
    if view.fill_occupancy {
        pairs.push(("fill_occupancy", "1".into()));
    }
    if let Some(f) = &view.flow {
        pairs.push(("flow_style", if f.lic { "lic" } else { "streamlines" }.into()));
        pairs.push(("flow_field", f.field.clone()));
    }
    if let Some(d) = &view.deformation {
        pairs.push(("deform_field", d.field.clone()));
        pairs.push(("deform_scale", d.scale.to_string()));
        if let Some(m) = &d.mask {
            pairs.push(("deform_mask", m.clone()));
        }
        if d.mask_threshold.to_bits() != 0.5_f64.to_bits() {
            pairs.push(("deform_mask_threshold", d.mask_threshold.to_string()));
        }
        if let Some(c) = &d.colour {
            pairs.push(("deform_colour", c.clone()));
        }
    }
    if let Some(f) = &view.iso_field {
        pairs.push(("iso_field", f.clone()));
    }
    if view.clip.is_some() {
        pairs.push(("clip", "1".into()));
    }
    pairs.push(("cycle", first.cycle.map_or_else(|| "all".to_owned(), |c| c.to_string())));
    pairs.push(("index", index.to_string()));
    let link = panel_link(&pairs).map_err(|e| match e {
        CaptureError::Contract(m) | CaptureError::Failed(m) => AgentError::contract(m),
    })?;
    #[allow(clippy::cast_possible_truncation)]
    let (w, h) = (view.width.clamp(640, 1600) as u32, view.height.clamp(480, 1200) as u32);
    let (png, record) = capture_panel(origin, &link, &sid.as_string(), w, h).map_err(|e| match e {
        CaptureError::Contract(m) => AgentError::refused(m),
        CaptureError::Failed(m) => {
            AgentError::failed(format!("dynamic panel capture failed (no renderer substitution): {m}"))
        }
    })?;
    let (pw, ph) =
        implexity_render::capture::png_size(&png).map_or((0, 0), |(a, b)| (a as usize, b as usize));
    if png.len() > MAX_INLINE_BYTES || pw * ph > MAX_INLINE_PIXELS {
        return Err(AgentError::refused("the panel capture exceeds the inline image bound"));
    }
    Ok(json!({
        "schema": RENDER_SCHEMA, "extension": EXTENSION, "kind": "render_dynamic_frames",
        "renderer": "native_gui_panel_capture", "truth_status": "stored_provider_frames",
        "store": sid.as_string(), "view": view.record(), "frames": [frame_row(&sid, first)],
        "capture": record, "viewer_url": link,
        "image": inline_image(&png, "image/png", pw, ph),
    }))
}

fn even(img: RgbImage, bg: [u8; 3]) -> RgbImage {
    if img.width.is_multiple_of(2) && img.height.is_multiple_of(2) {
        return img;
    }
    let (w, h) = (img.width + img.width % 2, img.height + img.height % 2);
    let mut out = RgbImage::filled(w, h, bg);
    for r in 0..img.height {
        let (a, b) = (r * img.width * 3, (r + 1) * img.width * 3);
        out.data[r * w * 3..r * w * 3 + img.width * 3].copy_from_slice(&img.data[a..b]);
    }
    out
}



pub fn stream_animation(
    cat: &Catalogue,
    p: &Map<String, Value>,
    sink: &mut dyn FnMut(RgbImage) -> AgentResult<()>,
) -> AgentResult<(Value, Vec<Value>, usize)> {
    let view = View::from_payload(p, (480, 360))?;
    let (sid, store) = open(cat, p.get("store").and_then(Value::as_str).unwrap_or_default())?;
    let mut rows = Vec::new();
    if view.mode == Mode::PhaseAverage {
        let bins = averaged(&store, &view)?;
        let values: Vec<&dyn FrameValues> = bins.iter().map(|(v, _, _)| v as &dyn FrameValues).collect();
        let r = Renderer::new(&store, &sid.as_string(), view, &values)?;
        for (v, caption, count) in &bins {
            sink(r.render(v, caption)?)?;
            rows.push(json!({"phase_bin": caption, "frames_averaged": count}));
        }
        let n = bins.len();
        return Ok((r.field_record(), rows, n));
    }
    let cycles = p.get("cycles").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(1);
    let per =
        p.get("frames_per_cycle").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).unwrap_or(24);
    let (frames, distinct) = select::animation(&store, CycleSel::parse(p.get("cycle")), cycles, per)?;

    let mut unique: Vec<&implexity_runtime::dynamic_frames::store::FrameEntry> = frames.clone();
    unique.sort_by_key(|f| f.seq);
    unique.dedup_by_key(|f| f.seq);
    let stored: Vec<Stored<'_>> = unique.iter().map(|f| Stored { store: &store, frame: f }).collect();
    let values: Vec<&dyn FrameValues> =
        if view.range_frames { stored.iter().map(|s| s as &dyn FrameValues).collect() } else { Vec::new() };
    let r = Renderer::new(&store, &sid.as_string(), view, &values)?;
    for f in &frames {
        let caption = r.caption(f);
        sink(r.render(&Stored { store: &store, frame: f }, &caption)?)?;
        rows.push(frame_row(&sid, f));
    }
    Ok((r.field_record(), rows, distinct))
}

fn render_animation(cat: &Catalogue, p: &Map<String, Value>, export: bool) -> AgentResult<Value> {
    let default_format = if export { "mp4" } else { "webp" };
    let format = AnimationFormat::parse(p.get("format").and_then(Value::as_str).unwrap_or(default_format))
        .map_err(contract)?;
    let fps = p.get("fps").and_then(Value::as_u64).and_then(|x| u32::try_from(x).ok()).unwrap_or(12);
    let limit = if export {
        p.get("max_bytes")
            .and_then(Value::as_u64)
            .and_then(|x| usize::try_from(x).ok())
            .unwrap_or(32 << 20)
            .min(MAX_EXPORT_BYTES)
    } else {
        MAX_INLINE_BYTES
    };
    let theme =
        Theme::parse(p.get("background").and_then(Value::as_str).unwrap_or("dark")).map_err(contract)?;
    let mut anim: Option<Animation> = None;
    let mut first_size = (0, 0);
    let mut sink = |img: RgbImage| -> AgentResult<()> {
        let img = if format == AnimationFormat::Mp4 { even(img, theme.background()) } else { img };
        if anim.is_none() {
            first_size = (img.width, img.height);
            anim = Some(Animation::new(format, img.width, img.height, fps, limit).map_err(contract)?);
        }
        anim.as_mut()
            .ok_or_else(|| failed("no animation"))?
            .push(&img)
            .map_err(|e| AgentError::refused(e.to_string()))
    };
    let (field, rows, distinct) = stream_animation(cat, p, &mut sink)?;
    let anim = anim.ok_or_else(|| AgentError::refused("no frames to animate"))?;
    let frames = anim.frames();
    let bytes = anim.finish().map_err(|e| AgentError::refused(e.to_string()))?;
    let (w, h) = first_size;
    if !export && w * h > MAX_INLINE_PIXELS {
        return Err(AgentError::contract("the animation frames exceed the inline pixel bound"));
    }
    let store = p.get("store").and_then(Value::as_str).unwrap_or_default();
    let view = View::from_payload(p, (480, 360))?;
    let mut out = json!({
        "schema": RENDER_SCHEMA, "extension": EXTENSION,
        "kind": if export { "export_animation" } else { "render_cycle_animation" },
        "truth_status": "stored_provider_frames", "store": store, "field": field, "view": view.record(),
        "animation": {"format": format.extension(), "mime_type": format.mime(), "fps": fps, "frames": frames,
                      "distinct_stored_frames": distinct, "width_px": w, "height_px": h, "bytes": bytes.len(),
                      "sha256": implexity_io::digest::sha256_hex(&bytes),
                      "codec_note": match format {
                          AnimationFormat::Mp4 => "H.264 constrained baseline with lossless PCM macroblocks (pure-Rust encoder): standard and playable, about 1.5 bytes per pixel and frame",
                          AnimationFormat::Webp => "lossless animated WebP",
                          AnimationFormat::Gif => "animated GIF, 256 colours per frame (NeuQuant)",
                          AnimationFormat::PngSequence => "ZIP of lossless PNG frames",
                      }},
        "frames": rows,
    });
    if export {
        let safe: String = store
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        let name = format!(
            "{safe}_{}.{}",
            p.get("field").and_then(Value::as_str).unwrap_or("field").replace(':', "_"),
            format.extension()
        );
        let file = json!({"schema": "implexity-inline-file/1", "filename": name, "mime_type": format.mime(),
                          "bytes": bytes.len(), "sha256": implexity_io::digest::sha256_hex(&bytes),
                          "data_base64": base64::engine::general_purpose::STANDARD.encode(&bytes)});
        out["delivery"] = json!({"mode": "inline_base64", "files": [file], "total_bytes": bytes.len()});
    } else {
        let mut image = inline_image(&bytes, format.mime(), w, h);
        image["animated"] = json!(true);
        image["frames"] = json!(frames);
        out["image"] = image;
    }
    Ok(out)
}

