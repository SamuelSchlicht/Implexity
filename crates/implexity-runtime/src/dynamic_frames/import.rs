// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;

use implexity_core::contracts::FieldValue;
use ndarray::ArrayD;
use serde_json::{Map, Value, json};

use super::capture::{CaptureRoot, begin_in};
use super::manifest::{
    CELLS_SUFFIX, CellShape, FieldKind, FieldSpec, FrameManifest, GridSpec, MeshSpec, PaletteHint, Precision,
    Retention, SeriesSpec, TimeBase,
};
use super::store::StoreSummary;
use super::{DynamicError, DynamicResult, is_identifier};

pub const DIAGNOSTICS_KEY: &str = "dynamic_frames";

fn parse_frame_key(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix("frame_")?;
    let (digits, name) = rest.split_once('_')?;
    let k: usize = digits.parse().ok().filter(|k| *k < 1000)?;

    (k.to_string() == digits && is_identifier(name)).then_some((k, name))
}

fn text(v: Option<&Value>, default: &str) -> String {
    v.and_then(Value::as_str).map_or_else(|| default.to_owned(), str::to_owned)
}

fn nums(v: Option<&Value>, n: usize, default: f64) -> DynamicResult<Vec<f64>> {
    match v {
        None => Ok(vec![default; n]),
        Some(Value::Array(a)) if a.len() == n => a
            .iter()
            .map(|x| {
                x.as_f64()
                    .filter(|x| x.is_finite())
                    .ok_or_else(|| DynamicError::invalid("grid numbers must be finite"))
            })
            .collect(),
        Some(_) => Err(DynamicError::invalid(format!("grid spacing/origin must list {n} numbers"))),
    }
}

struct ImportedMesh<'a> {
    spec: MeshSpec,
    points: &'a ArrayD<f64>,
    cells: Vec<usize>,
    attributes: Vec<(String, &'a ArrayD<f64>)>,
}

fn declared_meshes<'a>(
    decl: &Map<String, Value>,
    fields: &'a BTreeMap<String, FieldValue>,
) -> DynamicResult<Vec<ImportedMesh<'a>>> {
    let mut out = Vec::new();
    let Some(meshes) = decl.get("meshes").and_then(Value::as_object) else { return Ok(out) };
    let array = |key: &str| -> DynamicResult<&'a ArrayD<f64>> {
        match fields.get(key) {
            Some(FieldValue::Array(a)) => Ok(a),
            _ => Err(DynamicError::invalid(format!("the declared mesh array {key} is not a result array"))),
        }
    };
    for (name, d) in meshes {
        if !is_identifier(name) {
            return Err(DynamicError::invalid(format!("mesh name {name:?} is invalid")));
        }
        let d = d.as_object().cloned().unwrap_or_default();
        let cell = CellShape::parse(&text(d.get("cell"), "tetrahedron"))?;
        let points = array(&text(d.get("points"), &format!("mesh_{name}_points")))?;
        let raw = array(&text(d.get("cells"), &format!("mesh_{name}_cells")))?;
        let (dims, k) = (cell.dims(), cell.nodes_per_cell());
        if points.ndim() != 2 || points.shape()[1] != dims {
            return Err(DynamicError::invalid(format!("the points of mesh {name} must be [nodes, {dims}]")));
        }
        if raw.ndim() != 2 || raw.shape()[1] != k {
            return Err(DynamicError::invalid(format!("the cells of mesh {name} must be [cells, {k}]")));
        }
        let nodes = points.shape()[0];
        let mut cells = Vec::with_capacity(raw.len());
        for &x in raw {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = x as usize;

            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            let exact = n as f64 == x;
            if !(x >= 0.0 && exact && n < nodes) {
                return Err(DynamicError::invalid(format!("mesh {name} has an invalid node index {x}")));
            }
            cells.push(n);
        }
        let mut attributes = Vec::new();
        let mut specs = Vec::new();
        for (aname, a) in d.get("attributes").and_then(Value::as_object).cloned().unwrap_or_default() {
            let a = a.as_object().cloned().unwrap_or_default();
            let at_cells = match text(a.get("at"), "cells").as_str() {
                "cells" => true,
                "nodes" => false,
                other => {
                    return Err(DynamicError::invalid(format!(
                        "attribute {aname} of mesh {name}: at must be nodes or cells, not {other:?}"
                    )));
                }
            };
            let values = array(&text(a.get("field"), &format!("mesh_{name}_{aname}")))?;
            specs.push(FieldSpec {
                label: text(a.get("label"), &aname),
                unit: text(a.get("unit"), "1"),
                grid: if at_cells { format!("{name}{CELLS_SUFFIX}") } else { name.clone() },
                kind: FieldKind::parse(&text(a.get("kind"), "scalar"))?,
                palette: PaletteHint::parse(&text(a.get("palette"), "sequential"))?,
                name: aname.clone(),
            });
            attributes.push((aname, values));
        }
        out.push(ImportedMesh {
            spec: MeshSpec {
                name: name.clone(),
                cell,
                nodes,
                cells: raw.shape()[0],
                unit: text(d.get("unit"), "cell"),
                attributes: specs,
            },
            points,
            cells,
            attributes,
        });
    }
    Ok(out)
}



#[allow(clippy::too_many_lines)]
pub fn import_evaluation(
    root: &CaptureRoot,
    provider: &str,
    fields: &BTreeMap<String, FieldValue>,
    diagnostics: &Map<String, Value>,
    label: &str,
) -> DynamicResult<Option<StoreSummary>> {
    let decl = diagnostics.get(DIAGNOSTICS_KEY).and_then(Value::as_object).cloned().unwrap_or_default();
    let fdecl = decl.get("fields").and_then(Value::as_object).cloned().unwrap_or_default();
    let sdecl = decl.get("series").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut frames: BTreeMap<String, BTreeMap<usize, &ArrayD<f64>>> = BTreeMap::new();
    let mut series: BTreeMap<String, &ArrayD<f64>> = BTreeMap::new();
    for (key, value) in fields {
        let FieldValue::Array(a) = value else { continue };
        if let Some((k, name)) = parse_frame_key(key) {
            frames.entry(name.to_owned()).or_default().insert(k, a);
        } else if let Some(name) = key.strip_prefix("cycle_").filter(|n| is_identifier(n))
            && a.ndim() == 1
        {
            series.insert(name.to_owned(), a);
        }
    }
    if frames.is_empty() && series.is_empty() {
        return Ok(None);
    }
    let counts: std::collections::BTreeSet<usize> = frames.values().map(BTreeMap::len).collect();
    if counts.len() > 1 {
        return Err(DynamicError::invalid("every frame_<k>_<field> field needs the same frame count"));
    }
    let nframes = counts.into_iter().next().unwrap_or(0);
    if frames.values().any(|m| m.keys().copied().ne(0..nframes)) {
        return Err(DynamicError::invalid("frame indices must be 0..K-1 for every field"));
    }
    let meshes = declared_meshes(&decl, fields)?;
    let mut grids: Vec<GridSpec> = Vec::new();
    let mut specs: Vec<FieldSpec> = Vec::new();

    let mut skipped = Map::new();
    for (name, by_k) in &frames {
        let d = fdecl.get(name).and_then(Value::as_object).cloned().unwrap_or_default();
        let kind = FieldKind::parse(&text(d.get("kind"), "scalar"))?;
        let shape = by_k[&0].shape().to_vec();
        if by_k.values().any(|a| a.shape() != shape.as_slice()) {
            return Err(DynamicError::invalid(format!("the frames of {name} differ in shape")));
        }
        if let Some(mname) = d.get("mesh").and_then(Value::as_str) {
            let Some(mesh) = meshes.iter().find(|m| m.spec.name == mname) else {
                return Err(DynamicError::invalid(format!("field {name} names the undeclared mesh {mname}")));
            };
            let at_cells = match text(d.get("at"), "nodes").as_str() {
                "nodes" => false,
                "cells" => true,
                other => {
                    return Err(DynamicError::invalid(format!(
                        "field {name}: at must be nodes or cells, not {other:?}"
                    )));
                }
            };
            let sites = if at_cells { mesh.spec.cells } else { mesh.spec.nodes };
            let dims = mesh.spec.cell.dims();
            let expected: &[usize] = if kind.is_vector() { &[sites, dims] } else { &[sites] };
            if shape != expected {
                return Err(DynamicError::invalid(format!(
                    "the frames of {name} must be {expected:?} on mesh {mname}, not {shape:?}"
                )));
            }
            specs.push(FieldSpec {
                name: name.clone(),
                label: text(d.get("label"), name),
                unit: text(d.get("unit"), "1"),
                grid: if at_cells { format!("{mname}{CELLS_SUFFIX}") } else { mname.to_owned() },
                kind,
                palette: PaletteHint::parse(&text(d.get("palette"), "sequential"))?,
            });
            continue;
        }
        let grid_shape: Vec<usize> =
            if kind.is_vector() {
                match shape.split_last() {
                    Some((&c, rest)) if c == rest.len() && (2..=3).contains(&c) => rest.to_vec(),
                    _ => {
                        skipped.insert(name.clone(), json!(format!(
                        "shape {shape:?} is not a structured 2-D/3-D grid with a trailing component axis"
                    )));
                        continue;
                    }
                }
            } else {
                shape.clone()
            };
        if !(2..=3).contains(&grid_shape.len())
            || grid_shape.iter().product::<usize>() > super::manifest::MAX_GRID_CELLS
        {
            skipped.insert(name.clone(), json!(format!("shape {shape:?} is not a structured 2-D/3-D grid")));
            continue;
        }
        let g = d.get("grid").and_then(Value::as_object).cloned().unwrap_or_default();
        let gname = match g.get("name").and_then(Value::as_str) {
            Some(n) => n.to_owned(),
            None => {
                format!("grid_{}", grid_shape.iter().map(ToString::to_string).collect::<Vec<_>>().join("x"))
            }
        };
        let spec = GridSpec {
            name: gname.clone(),
            spacing: nums(g.get("spacing"), grid_shape.len(), 1.0)?,
            origin: nums(g.get("origin"), grid_shape.len(), 0.0)?,
            unit: text(g.get("unit"), "cell"),
            shape: grid_shape,
        };
        match grids.iter().find(|x| x.name == gname) {
            Some(existing) if *existing != spec => {
                return Err(DynamicError::invalid(format!(
                    "grid {gname} is declared twice with different geometry"
                )));
            }
            Some(_) => {}
            None => grids.push(spec),
        }
        specs.push(FieldSpec {
            name: name.clone(),
            label: text(d.get("label"), name),
            unit: text(d.get("unit"), "1"),
            grid: gname,
            kind,
            palette: PaletteHint::parse(&text(d.get("palette"), "sequential"))?,
        });
    }
    let series_specs: Vec<SeriesSpec> = series
        .keys()
        .map(|n| {
            let d = sdecl.get(n).and_then(Value::as_object).cloned().unwrap_or_default();
            SeriesSpec {
                name: n.clone(),
                label: text(d.get("label"), n),
                unit: text(d.get("unit"), "1"),
                role: text(d.get("role"), "response"),
            }
        })
        .collect();
    let lengths: std::collections::BTreeSet<usize> = series.values().map(|a| a.len()).collect();
    if lengths.len() > 1 {
        return Err(DynamicError::invalid("every cycle_<series> field needs the same length"));
    }
    let nsamples = lengths.into_iter().next().unwrap_or(0);
    let frames: BTreeMap<String, BTreeMap<usize, &ArrayD<f64>>> =
        frames.into_iter().filter(|(n, _)| !skipped.contains_key(n)).collect();

    let times: Option<Vec<f64>> =
        ["frame_times_s", "frame_times"].iter().find_map(|k| match fields.get(*k) {
            Some(FieldValue::Array(a))
                if a.ndim() == 1 && a.len() == nframes && a.iter().all(|t| t.is_finite()) =>
            {
                Some(a.iter().copied().collect())
            }
            _ => None,
        });
    if times.as_ref().is_some_and(|t| t.windows(2).any(|w| w[1] < w[0])) {
        return Err(DynamicError::invalid("frame times must not decrease"));
    }
    if frames.is_empty() && series.is_empty() {
        return Ok(None);
    }
    let period = match decl.get("period") {
        None | Some(Value::Null) => 1.0,
        Some(p) => p
            .as_f64()
            .filter(|p| p.is_finite() && *p > 0.0)
            .ok_or_else(|| DynamicError::invalid("dynamic_frames.period must be positive"))?,
    };
    let explicit_period = decl.get("period").is_some_and(|p| !p.is_null());
    let phases: Vec<f64> = match decl.get("frame_phases").and_then(Value::as_array) {
        Some(a) if a.len() == nframes => a
            .iter()
            .map(|p| {
                p.as_f64()
                    .filter(|p| (0.0..1.0).contains(p))
                    .ok_or_else(|| DynamicError::invalid("frame_phases must lie in [0, 1)"))
            })
            .collect::<DynamicResult<_>>()?,
        Some(_) => return Err(DynamicError::invalid("frame_phases needs one phase per frame")),
        None => match (&times, explicit_period) {
            (Some(t), true) => t.iter().map(|x| super::analysis::phase_of(*x, period, t[0])).collect(),
            _ => (0..nframes).map(|k| k as f64 / nframes as f64).collect(),
        },
    };
    if phases.windows(2).any(|w| w[1] < w[0]) {
        return Err(DynamicError::invalid("frame_phases must not decrease"));
    }
    let raw_bytes: usize =
        frames.values().map(|m| m.values().map(|a| a.len() * 4 + 128).sum::<usize>()).sum::<usize>()
            + nsamples * 8 * (series.len() + 1)
            + meshes
                .iter()
                .map(|m| {
                    8 * (m.points.len() + m.cells.len())
                        + m.attributes.iter().map(|(_, a)| 8 * a.len() + 128).sum::<usize>()
                        + 256
                })
                .sum::<usize>();
    let mut provenance = Map::new();
    provenance.insert("provider".into(), json!(provider));
    provenance.insert("source".into(), json!("evaluation_fields"));
    if !skipped.is_empty() {
        provenance.insert("skipped_fields".into(), Value::Object(skipped));
    }
    let mut manifest = FrameManifest {
        grids,
        fields: specs,
        series: series_specs,
        time: TimeBase {
            unit: text(
                decl.get("time_unit"),
                if explicit_period || times.is_some() { "s" } else { "period" },
            ),
            step: if nsamples > 0 { period / nsamples as f64 } else { period / nframes.max(1) as f64 },
            period: Some(period),
            phase_origin: times.as_ref().map_or(0.0, |t| t[0]),
        },
        retention: Retention {
            byte_limit: (raw_bytes as u64 * 2).max(super::manifest::MIN_BYTE_LIMIT),
            phase_bins: 0,
            retain_segments: 1,
            segment_length: None,
            precision: Precision::F64,
        },
        provenance,
        meshes: meshes.iter().map(|m| m.spec.clone()).collect(),
    };
    let mut w = begin_in(root, &mut manifest, label)?;
    for m in &meshes {
        let points: Vec<f64> = m.points.iter().copied().collect();
        let attrs: Vec<(String, Vec<f64>)> =
            m.attributes.iter().map(|(n, a)| (n.clone(), a.iter().copied().collect())).collect();
        let refs: Vec<(&str, &[f64])> = attrs.iter().map(|(n, v)| (n.as_str(), v.as_slice())).collect();
        w.write_mesh(&m.spec.name, &points, &m.cells, &refs)?;
    }
    let order: Vec<String> = w.manifest().fields.iter().map(|f| f.name.clone()).collect();
    let mut events: Vec<(f64, Option<usize>, Option<usize>)> = Vec::new();
    let t0 = times.as_ref().map_or(0.0, |t| t[0]);
    for (k, p) in phases.iter().enumerate() {
        events.push((times.as_ref().map_or(p * period, |t| t[k]), Some(k), None));
    }
    for j in 0..nsamples {
        events.push((t0 + j as f64 * period / nsamples as f64, None, Some(j)));
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.is_none().cmp(&b.1.is_none())));
    for (t, frame, sample) in events {
        if let Some(k) = frame {
            let data: Vec<(String, Vec<f64>)> =
                order.iter().map(|n| (n.clone(), frames[n][&k].iter().copied().collect())).collect();
            let refs: Vec<(&str, &[f64])> = data.iter().map(|(n, v)| (n.as_str(), v.as_slice())).collect();
            w.push_frame(t, Some(phases[k]), &refs)?;
        }
        if let Some(j) = sample {
            let row: Vec<f64> = w.manifest().series.iter().map(|s| series[&s.name][j]).collect();
            w.push_series(t, &row)?;
        }
    }
    w.finish(true).map(Some)
}

