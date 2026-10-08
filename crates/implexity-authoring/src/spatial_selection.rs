// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, VecDeque};

use serde_json::{Map, Value, json};

use implexity_geometry::linalg3::{inv, solve};

use crate::error::{AResult, AuthoringError};
use crate::field_interaction::GridGeometry;
use crate::geometry_holds::runs_json;
use crate::py::{
    Arr, get, jf, jfs, norm3, obj_mut, py_float, py_int, py_str, repr, row_norm3, setdefault_list,
    setdefault_obj, sha_unicode, token_hex, truthy,
};

pub const SELECTION_SCHEMA: &str = "implexity-spatial-selection/1";
pub const REGION_SCHEMA: &str = "implexity-cell-region/1";
pub const SELECTION_KINDS: [&str; 7] = ["click", "brush", "box", "lasso", "flood", "bounds", "indices"];
pub const SELECTION_ALGEBRA: [&str; 4] = ["replace", "add", "subtract", "intersect"];

const CLASS: &str = "SpatialSelectionError";

pub(crate) fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value(CLASS, message)
}

fn content_id(value: &Value) -> String {
    format!("seldef_{}", &sha_unicode(value)[..24])
}

#[must_use]
pub fn encode_runs(mask: &[bool]) -> Value {
    runs_json(&crate::geometry_holds::encode_runs(mask))
}


pub fn decode_runs(runs: Option<&Value>, shape: &[i64]) -> AResult<Vec<bool>> {
    if shape.len() != 3 || shape.iter().any(|v| *v < 1) {
        return Err(err("selection shape must contain three positive integers"));
    }
    let size = shape.iter().product::<i64>() as usize;
    let mut mask = vec![false; size];
    let runs = match runs {
        None | Some(Value::Null) => return Ok(mask),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(err("selected_runs must be a list")),
    };
    let mut previous_end = 0i64;
    for (pos, run) in runs.iter().enumerate() {
        let Some(pair) = run.as_array().filter(|p| p.len() == 2) else {
            return Err(err(format!("selected_runs[{pos}] must contain start and count")));
        };
        let start = py_int(&pair[0])?;
        let count = py_int(&pair[1])?;
        let end = start + count;
        if start < previous_end || count <= 0 || end > size as i64 {
            return Err(err("selected_runs must be positive, ordered, non-overlapping and in bounds"));
        }
        for v in &mut mask[start as usize..end as usize] {
            *v = true;
        }
        previous_end = end;
    }
    Ok(mask)
}

fn shape_of(value: &Value, default: Option<[usize; 3]>) -> AResult<Vec<i64>> {
    match get(value, "shape") {
        None => Ok(default.map(|d| d.iter().map(|v| *v as i64).collect()).unwrap_or_default()),
        Some(Value::Array(a)) => a.iter().map(py_int).collect(),
        Some(other) => {
            Err(crate::py::type_error(format!("'{}' object is not iterable", crate::py::type_name(other))))
        }
    }
}

fn count(mask: &[bool]) -> usize {
    mask.iter().filter(|v| **v).count()
}


pub fn normalize_spatial_selection(value: &Value) -> AResult<Value> {
    if !value.is_object() {
        return Err(err("cell selection must be an object"));
    }
    if let Some(s) = get(value, "schema").filter(|s| !s.is_null())
        && s.as_str() != Some(SELECTION_SCHEMA)
    {
        return Err(err(format!("unsupported selection schema: {}", repr(s))));
    }
    let shape = shape_of(value, None)?;
    if shape.len() != 3 || shape.iter().any(|v| *v < 1) {
        return Err(err("cell selection shape must contain three positive integers"));
    }
    let Some(grid_raw) = get(value, "grid").filter(|g| g.is_object()) else {
        return Err(err("cell selection requires an exact grid registration"));
    };
    let grid = GridGeometry::from_values(&shape, grid_raw)?;
    let selected = decode_runs(Some(get(value, "selected_runs").unwrap_or(&json!([]))), &shape)?;
    let protected = decode_runs(Some(get(value, "protected_runs").unwrap_or(&json!([]))), &shape)?;
    if selected.iter().zip(&protected).any(|(s, p)| *s && *p) {
        return Err(err("protected cells cannot appear in selected_runs"));
    }
    let selection_id = get(value, "id").map_or_else(String::new, py_str).trim().to_string();
    let field_id = get(value, "field_id").map_or_else(String::new, py_str).trim().to_string();
    if selection_id.is_empty() || field_id.is_empty() {
        return Err(err("cell selection id and field_id are required"));
    }
    let revision = py_int(get(value, "revision").unwrap_or(&json!(0)))?;
    if revision < 0 {
        return Err(err("cell selection revision must be non-negative"));
    }
    let threshold = py_float(get(value, "threshold").unwrap_or(&json!(0.5)))?;
    if !threshold.is_finite() {
        return Err(err("cell selection threshold must be finite"));
    }
    let obj_or_empty =
        |k: &str| get(value, k).filter(|v| truthy(v)).and_then(Value::as_object).cloned().unwrap_or_default();
    let counts = match get(value, "counts").filter(|v| truthy(v)) {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => json!({"selected": count(&selected)}),
    };
    let provenance: Vec<Value> = match get(value, "provenance") {
        Some(Value::Array(a)) => {
            a.iter().map(|x| Value::Object(x.as_object().cloned().unwrap_or_default())).collect()
        }
        _ => Vec::new(),
    };
    let mut out = Map::new();
    out.insert("schema".into(), json!(SELECTION_SCHEMA));
    out.insert("kind".into(), json!("cell_selection"));
    out.insert("id".into(), json!(selection_id));
    out.insert("revision".into(), json!(revision));
    out.insert("field_id".into(), json!(field_id));
    out.insert("field_identity".into(), Value::Object(obj_or_empty("field_identity")));
    out.insert("shape".into(), json!(shape));
    out.insert("grid".into(), grid.serialise());
    out.insert("threshold".into(), jf(threshold));
    out.insert("selected_runs".into(), encode_runs(&selected));
    out.insert("protected_runs".into(), encode_runs(&protected));
    out.insert("protected_by_reason".into(), Value::Object(obj_or_empty("protected_by_reason")));
    out.insert("counts".into(), counts);
    out.insert("snapped".into(), get(value, "snapped").cloned().unwrap_or(Value::Null));
    out.insert("provenance".into(), Value::Array(provenance));
    let identity: Map<String, Value> = out
        .iter()
        .filter(|(k, _)| !["definition_id", "counts", "snapped"].contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    out.insert("definition_id".into(), Value::from(content_id(&Value::Object(identity))));
    Ok(Value::Object(out))
}

fn vec3(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let bad = || err(format!("{name} must contain three finite values"));
    let a = Arr::from_opt(value).map_err(|_| bad())?;
    match a.vec3() {
        Some(v) if v.iter().all(|x| x.is_finite()) => Ok(v),
        _ => Err(bad()),
    }
}

type Frame = ([f64; 3], [f64; 3], [f64; 3], [f64; 3]);

fn camera_frame(camera: &Value) -> AResult<Frame> {
    let eye = vec3(crate::py::get_either(camera, "eye_mm", "eye"), "camera.eye_mm")?;
    let target = vec3(crate::py::get_either(camera, "target_mm", "target"), "camera.target_mm")?;
    let up_hint = vec3(Some(get(camera, "up").unwrap_or(&json!([0.0, 0.0, 1.0]))), "camera.up")?;
    let mut forward: [f64; 3] = std::array::from_fn(|a| target[a] - eye[a]);
    let fnorm = norm3(forward);
    if fnorm <= 1.0e-14 {
        return Err(err("camera eye and target must differ"));
    }
    forward = forward.map(|v| v / fnorm);
    let mut right = crate::py::cross3(forward, up_hint);
    let mut rnorm = norm3(right);
    if rnorm <= 1.0e-14 {
        let fallback = if forward[2].abs() > 0.9 { [0.0, 1.0, 0.0] } else { [0.0, 0.0, 1.0] };
        right = crate::py::cross3(forward, fallback);
        rnorm = norm3(right);
    }
    right = right.map(|v| v / rnorm);
    let up = crate::py::cross3(right, forward);
    Ok((eye, forward, right, up))
}

fn projection_of(camera: &Value) -> String {
    crate::py::lower_str(get(camera, "projection"), "perspective")
}

fn project(points: &[[f64; 3]], camera: &Value) -> AResult<(Vec<[f64; 2]>, Vec<f64>, Vec<bool>)> {
    let (eye, forward, right, up) = camera_frame(camera)?;
    let vp_raw = get(camera, "viewport_px")
        .or_else(|| get(camera, "viewport"))
        .cloned()
        .unwrap_or_else(|| json!([1.0, 1.0]));
    let vp =
        Arr::from_json(&vp_raw).map_err(|_| err("camera.viewport_px must contain two positive values"))?;
    if vp.shape != [2] || !vp.all_finite() || vp.data.iter().any(|v| *v <= 0.0) {
        return Err(err("camera.viewport_px must contain two positive values"));
    }
    let viewport = [vp.data[0], vp.data[1]];
    let aspect = viewport[0] / viewport[1];
    let projection = projection_of(camera);
    let mut screen = Vec::with_capacity(points.len());
    let mut depth = Vec::with_capacity(points.len());
    let mut valid = Vec::with_capacity(points.len());
    enum Proj {
        Ortho(f64),
        Persp(f64),
    }
    let proj = match projection.as_str() {
        "orthographic" => {
            let h = py_float(
                get(camera, "ortho_height_mm").or_else(|| get(camera, "height_mm")).unwrap_or(&json!(0.0)),
            )?;
            if !h.is_finite() || h <= 0.0 {
                return Err(err("orthographic selection requires a positive ortho_height_mm"));
            }
            Proj::Ortho(h)
        }
        "perspective" => {
            let fov =
                py_float(get(camera, "fov_deg").or_else(|| get(camera, "fov")).unwrap_or(&json!(48.13)))?;
            if !fov.is_finite() || !(0.1 < fov && fov < 179.0) {
                return Err(err("perspective fov_deg must be between 0.1 and 179"));
            }
            Proj::Persp((fov.to_radians() * 0.5).tan())
        }
        _ => return Err(err("camera.projection must be perspective or orthographic")),
    };
    for p in points {
        let rel: [f64; 3] = std::array::from_fn(|a| p[a] - eye[a]);
        let d = crate::py::dot3(rel, forward);
        let (nx, ny) = match proj {
            Proj::Ortho(h) => {
                (crate::py::dot3(rel, right) / (0.5 * h * aspect), crate::py::dot3(rel, up) / (0.5 * h))
            }
            Proj::Persp(t) => {
                let safe = if d > 1.0e-12 { d } else { f64::NAN };
                (crate::py::dot3(rel, right) / (safe * t * aspect), crate::py::dot3(rel, up) / (safe * t))
            }
        };
        let s = [(nx + 1.0) * 0.5 * viewport[0], (1.0 - ny) * 0.5 * viewport[1]];
        valid.push(s[0].is_finite() && s[1].is_finite() && d > 0.0);
        screen.push(s);
        depth.push(d);
    }
    Ok((screen, depth, valid))
}

fn inside_polygon(points: &[[f64; 2]], polygon: &Arr) -> AResult<Vec<bool>> {
    if polygon.ndim() != 2 || polygon.shape[1] != 2 || polygon.shape[0] < 3 || !polygon.all_finite() {
        return Err(err("lasso requires at least three finite screen points"));
    }
    let n = polygon.shape[0];
    let pt = |i: usize| (polygon.data[2 * i], polygon.data[2 * i + 1]);
    let mut inside = vec![false; points.len()];
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = pt(i);
        let (xj, yj) = pt(j);
        let dy = if yj - yi == 0.0 { 1.0e-30 } else { yj - yi };
        for (flag, p) in inside.iter_mut().zip(points) {
            let (x, y) = (p[0], p[1]);
            let crossing = ((yi > y) != (yj > y)) && (x < (xj - xi) * (y - yi) / dy + xi);
            *flag ^= crossing;
        }
        j = i;
    }
    Ok(inside)
}

fn component(mask: &[bool], shape: [usize; 3], seed: [usize; 3]) -> Vec<bool> {
    let flat = |c: [usize; 3]| (c[0] * shape[1] + c[1]) * shape[2] + c[2];
    let mut out = vec![false; mask.len()];
    let mut seed = seed;
    if !mask[flat(seed)] {
        let mut best: Option<(i64, [usize; 3])> = None;
        for i in 0..shape[0] {
            for j in 0..shape[1] {
                for k in 0..shape[2] {
                    if mask[flat([i, j, k])] {
                        let d =
                            [i as i64 - seed[0] as i64, j as i64 - seed[1] as i64, k as i64 - seed[2] as i64];
                        let d2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                        if best.is_none_or(|(b, _)| d2 < b) {
                            best = Some((d2, [i, j, k]));
                        }
                    }
                }
            }
        }
        let Some((_, s)) = best else { return out };
        seed = s;
    }
    let mut queue = VecDeque::from([seed]);
    out[flat(seed)] = true;
    while let Some(cell) = queue.pop_front() {
        for axis in 0..3 {
            for step in [-1i64, 1] {
                let v = cell[axis] as i64 + step;
                if v < 0 || v >= shape[axis] as i64 {
                    continue;
                }
                let mut nxt = cell;
                nxt[axis] = v as usize;
                let f = flat(nxt);
                if mask[f] && !out[f] {
                    out[f] = true;
                    queue.push_back(nxt);
                }
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct SpatialSelection {
    pub field_id: String,
    pub values: Vec<f64>,
    pub grid: GridGeometry,
    pub id: String,
    pub revision: i64,
    pub selected: Vec<bool>,
    pub protected_masks: BTreeMap<String, Vec<bool>>,
    pub protected: Vec<bool>,
    pub field_identity: Value,
    pub provenance: Vec<Value>,
    pub threshold: f64,
    pub last_counts: Value,
    pub last_snapped: Option<Value>,
}

impl SpatialSelection {

    pub fn new(
        field_id: &str,
        values: &[f64],
        grid: GridGeometry,
        selection_id: Option<&str>,
        revision: i64,
        selected: Option<&[bool]>,
        protected_masks: &BTreeMap<String, Vec<bool>>,
        field_identity: Option<&Value>,
        provenance: &[Value],
        threshold: f64,
    ) -> AResult<Self> {
        if values.len() != grid.size() || !values.iter().all(|v| v.is_finite()) {
            return Err(err("selection field values must be finite and match the exact grid"));
        }
        if revision < 0 {
            return Err(err("selection revision must be non-negative"));
        }
        let id = selection_id
            .filter(|s| !s.is_empty())
            .map_or_else(|| format!("selection_{}", token_hex(10)), ToString::to_string);
        let mut sel = match selected {
            None => vec![false; grid.size()],
            Some(s) => {
                if s.len() != grid.size() {
                    return Err(err("selection mask does not match the selection grid"));
                }
                s.to_vec()
            }
        };
        let mut masks = BTreeMap::new();
        for (reason, m) in protected_masks {
            if m.len() != values.len() {
                return Err(err(format!(
                    "protected mask {} does not match the selection grid",
                    repr(&Value::from(reason.clone()))
                )));
            }
            masks.insert(reason.clone(), m.clone());
        }
        let mut protected = vec![false; grid.size()];
        for m in masks.values() {
            protected.iter_mut().zip(m).for_each(|(p, v)| *p |= *v);
        }
        sel.iter_mut().zip(&protected).for_each(|(s, p)| {
            if *p {
                *s = false;
            }
        });
        if !threshold.is_finite() {
            return Err(err("selection threshold must be finite"));
        }
        let start = provenance.len().saturating_sub(100);
        let provenance: Vec<Value> = provenance[start..]
            .iter()
            .map(|x| Value::Object(x.as_object().cloned().unwrap_or_default()))
            .collect();
        let last_counts = json!({"selected": count(&sel), "candidate": 0, "affected": 0, "protected": 0,
            "protected_total": count(&protected), "protected_by_reason": {}});
        Ok(Self {
            field_id: field_id.to_string(),
            values: values.to_vec(),
            grid,
            id,
            revision,
            selected: sel,
            protected_masks: masks,
            protected,
            field_identity: field_identity
                .and_then(Value::as_object)
                .map_or_else(|| json!({}), |m| Value::Object(m.clone())),
            provenance,
            threshold,
            last_counts,
            last_snapped: None,
        })
    }


    pub fn from_mapping(
        value: &Value,
        values: &[f64],
        grid: GridGeometry,
        protected_masks: &BTreeMap<String, Vec<bool>>,
        field_identity: Option<&Value>,
    ) -> AResult<Self> {
        if !value.is_object() {
            return Err(err("selection must be an object"));
        }
        if let Some(s) = get(value, "schema").filter(|s| !s.is_null())
            && s.as_str() != Some(SELECTION_SCHEMA)
        {
            return Err(err(format!("unsupported selection schema: {}", repr(s))));
        }
        let shape = shape_of(value, Some(grid.shape))?;
        if shape.len() != 3 || (0..3).any(|a| shape[a] != grid.shape[a] as i64) {
            return Err(err("persisted selection shape no longer matches its field"));
        }
        let registration_id =
            get(value, "grid").filter(|g| g.is_object()).and_then(|g| get(g, "registration_id"));
        if let Some(rid) = registration_id.filter(|v| truthy(v))
            && grid.registration.to_wire().get("registration_id") != Some(rid)
        {
            return Err(err("persisted selection belongs to a different grid registration"));
        }
        let selected = decode_runs(Some(get(value, "selected_runs").unwrap_or(&json!([]))), &shape)?;
        let provenance = get(value, "provenance").and_then(Value::as_array).cloned().unwrap_or_default();
        let fid = field_identity.filter(|v| truthy(v)).or_else(|| get(value, "field_identity"));
        let mut result = Self::new(
            &get(value, "field_id").map_or_else(String::new, py_str),
            values,
            grid,
            get(value, "id").filter(|v| !v.is_null()).map(py_str).as_deref(),
            py_int(get(value, "revision").unwrap_or(&json!(0)))?,
            Some(&selected),
            protected_masks,
            fid,
            &provenance,
            py_float(get(value, "threshold").unwrap_or(&json!(0.5)))?,
        )?;
        if let Some(Value::Object(counts)) = get(value, "counts") {
            let mut restored = counts.clone();
            let size = result.grid.size() as i64;
            for name in ["selected", "candidate", "affected", "protected", "protected_total"] {
                let Some(v) = restored.get(name).cloned() else { continue };
                let c = py_int(&v)?;
                if c < 0 || c > size {
                    return Err(err(format!(
                        "selection count {} is out of bounds",
                        repr(&Value::from(name))
                    )));
                }
                restored.insert(name.into(), json!(c));
            }
            let sel_count = count(&result.selected) as i64;
            if restored.get("selected").map_or(Ok(sel_count), py_int)? != sel_count {
                return Err(err("selection selected count does not match selected_runs"));
            }
            let prot_count = count(&result.protected) as i64;
            if restored.get("protected_total").map_or(Ok(prot_count), py_int)? != prot_count {
                return Err(err("selection protected count does not match the authoritative masks"));
            }
            if let Some(m) = result.last_counts.as_object_mut() {
                for (k, v) in restored {
                    m.insert(k, v);
                }
            }
        }
        if let Some(s @ Value::Object(_)) = get(value, "snapped") {
            result.last_snapped = Some(s.clone());
        }
        Ok(result)
    }

    fn occupied(&self) -> Vec<bool> {
        self.values.iter().map(|v| *v >= self.threshold).collect()
    }

    fn dual_directions(&self) -> AResult<[[f64; 3]; 3]> {
        let m = self.grid.registration.matrix();
        let inverse = inv(&m).ok_or_else(|| crate::py::value_error("Singular matrix"))?;
        Ok(std::array::from_fn(|axis| {
            let d = inverse[axis];
            let n = norm3(d).max(1.0e-30);
            d.map(|v| v / n)
        }))
    }

    fn open_faces(occupied: &[bool], shape: [usize; 3], axis: usize) -> (Vec<bool>, Vec<bool>) {
        let n = occupied.len();
        let mut low = vec![false; n];
        let mut high = vec![false; n];
        for i in 0..shape[0] {
            for j in 0..shape[1] {
                for k in 0..shape[2] {
                    let c = [i, j, k];
                    let f = (i * shape[1] + j) * shape[2] + k;
                    let neighbour = |delta: i64| -> Option<usize> {
                        let v = c[axis] as i64 + delta;
                        if v < 0 || v >= shape[axis] as i64 {
                            return None;
                        }
                        let mut q = c;
                        q[axis] = v as usize;
                        Some((q[0] * shape[1] + q[1]) * shape[2] + q[2])
                    };
                    low[f] = occupied[f] && neighbour(-1).is_none_or(|g| !occupied[g]);
                    high[f] = occupied[f] && neighbour(1).is_none_or(|g| !occupied[g]);
                }
            }
        }
        (low, high)
    }

    fn surface(&self) -> Vec<bool> {
        let occupied = self.occupied();
        let mut boundary = vec![false; occupied.len()];
        for axis in 0..3 {
            let (low, high) = Self::open_faces(&occupied, self.grid.shape, axis);
            for i in 0..boundary.len() {
                boundary[i] |= low[i] | high[i];
            }
        }
        boundary
    }

    fn front_facing(
        &self,
        coords: &[[f64; 3]],
        camera: &Value,
        minimum_alignment: f64,
    ) -> AResult<Vec<bool>> {
        let occupied = self.occupied();
        let (eye, forward, _, _) = camera_frame(camera)?;
        let ortho = projection_of(camera) == "orthographic";
        let toward: Vec<[f64; 3]> = coords
            .iter()
            .map(|c| {
                if ortho {
                    forward.map(|v| -v)
                } else {
                    let t: [f64; 3] = std::array::from_fn(|a| eye[a] - c[a]);
                    let l = row_norm3(t).max(1.0e-30);
                    t.map(|v| v / l)
                }
            })
            .collect();
        let dirs = self.dual_directions()?;
        let mut facing = vec![false; occupied.len()];
        for axis in 0..3 {
            let d = dirs[axis];
            let (low, high) = Self::open_faces(&occupied, self.grid.shape, axis);
            for i in 0..facing.len() {
                let t = toward[i];
                let low_al = t[0] * -d[0] + t[1] * -d[1] + t[2] * -d[2];
                let high_al = t[0] * d[0] + t[1] * d[1] + t[2] * d[2];
                facing[i] |= low[i] && low_al >= minimum_alignment;
                facing[i] |= high[i] && high_al >= minimum_alignment;
            }
        }
        Ok(facing)
    }

    fn clip_mask(coords: &[[f64; 3]], clip: Option<&Value>) -> AResult<Vec<bool>> {
        let mut keep = vec![true; coords.len()];
        let Some(clip) = clip.filter(|c| c.is_object() && crate::py::py_bool(get(c, "active"))) else {
            return Ok(keep);
        };
        if crate::py::py_bool(get(clip, "hit_on_clip_cap")) {
            return Err(err("selection cannot begin from a section-plane cap"));
        }
        let Some(plane) = get(clip, "plane").filter(|p| p.is_object()) else {
            return Err(err("active clipping requires exact plane evidence"));
        };
        let normal = vec3(get(plane, "n"), "clip plane normal")?;
        let d = py_float(get(plane, "d").unwrap_or(&json!(0.0)))?;
        if !d.is_finite() {
            return Err(err("clip plane offset must be finite"));
        }
        for (k, c) in keep.iter_mut().zip(coords) {
            *k &= crate::py::dot3(*c, normal) - d <= 1.0e-9;
        }
        Ok(keep)
    }

    fn visible_from_camera(
        &self,
        candidate: &[bool],
        camera: &Value,
        occluder: Option<&[bool]>,
    ) -> AResult<Vec<bool>> {
        let (eye, _forward, right, up) = camera_frame(camera)?;
        let reg = &self.grid.registration;
        let central = reg.world_to_index(eye).map_err(|e| crate::py::value_error(e.to_string()))?;
        let ortho = projection_of(camera) == "orthographic";
        let mut occupied = self.occupied();
        if let Some(o) = occluder {
            occupied.iter_mut().zip(o).for_each(|(a, b)| *a &= *b);
        }
        let shape = self.grid.shape;
        let mut visible = vec![false; candidate.len()];
        for i in 0..shape[0] {
            for j in 0..shape[1] {
                for k in 0..shape[2] {
                    let f = self.grid.flat(i, j, k);
                    if !candidate[f] {
                        continue;
                    }
                    let raw = [i as i64, j as i64, k as i64];
                    let target = [i as f64, j as f64, k as f64];
                    let eye_index = if ortho {
                        let tw = reg.index_to_world(target);
                        let rel: [f64; 3] = std::array::from_fn(|a| tw[a] - eye[a]);
                        let r = crate::py::dot3(rel, right);
                        let u = crate::py::dot3(rel, up);
                        let origin: [f64; 3] = std::array::from_fn(|a| eye[a] + right[a] * r + up[a] * u);
                        reg.world_to_index(origin).map_err(|e| crate::py::value_error(e.to_string()))?
                    } else {
                        central
                    };
                    let delta: [f64; 3] = std::array::from_fn(|a| target[a] - eye_index[a]);
                    let lower = [-0.5; 3];
                    let upper: [f64; 3] = std::array::from_fn(|a| shape[a] as f64 - 0.5);
                    let (mut enter, mut leave) = (0.0f64, 1.0f64);
                    for a in 0..3 {
                        if delta[a].abs() <= 1.0e-15 {
                            if eye_index[a] < lower[a] || eye_index[a] > upper[a] {
                                enter = 1.0;
                                leave = 0.0;
                                break;
                            }
                            continue;
                        }
                        let ta = (lower[a] - eye_index[a]) / delta[a];
                        let tb = (upper[a] - eye_index[a]) / delta[a];
                        enter = enter.max(ta.min(tb));
                        leave = leave.min(ta.max(tb));
                    }
                    if leave < enter {
                        continue;
                    }
                    let mut t = enter.max(0.0) + 1.0e-12;
                    let point: [f64; 3] = std::array::from_fn(|a| eye_index[a] + t * delta[a]);
                    let mut cell: [i64; 3] = std::array::from_fn(|a| {
                        ((point[a] + 0.5).floor() as i64).max(0).min(shape[a] as i64 - 1)
                    });
                    let step: [i64; 3] = std::array::from_fn(|a| {
                        if delta[a] > 0.0 {
                            1
                        } else if delta[a] < 0.0 {
                            -1
                        } else {
                            0
                        }
                    });
                    let mut next_t = [f64::INFINITY; 3];
                    let mut increment = [f64::INFINITY; 3];
                    for a in 0..3 {
                        if step[a] == 0 {
                            continue;
                        }
                        let boundary = cell[a] as f64 + if step[a] > 0 { 0.5 } else { -0.5 };
                        next_t[a] = (boundary - eye_index[a]) / delta[a];
                        increment[a] = 1.0 / delta[a].abs();
                    }
                    let mut blocked = false;
                    while t <= leave.min(1.0) + 1.0e-12 {
                        if cell == raw {
                            break;
                        }
                        let inside = (0..3).all(|a| cell[a] >= 0 && cell[a] < shape[a] as i64);
                        if inside
                            && occupied[self.grid.flat(cell[0] as usize, cell[1] as usize, cell[2] as usize)]
                        {
                            blocked = true;
                            break;
                        }
                        let crossing =
                            next_t.iter().copied().fold(f64::INFINITY, |a, b| if b < a { b } else { a });
                        let axes: Vec<usize> = (0..3).filter(|a| next_t[*a] <= crossing + 1.0e-12).collect();
                        if axes.is_empty() || !crossing.is_finite() {
                            break;
                        }
                        for a in axes {
                            cell[a] += step[a];
                            next_t[a] += increment[a];
                        }
                        t = crossing;
                    }
                    if !blocked {
                        visible[f] = true;
                    }
                }
            }
        }
        Ok(visible)
    }

    fn view_mask(&self, coords: &[[f64; 3]], selector: &Value, surface: &[bool]) -> AResult<Vec<bool>> {
        let visibility = crate::py::lower_str(get(selector, "visibility"), "front");
        let visibility = match visibility.as_str() {
            "visible" => "front".to_string(),
            "xray" | "x-ray" => "through".to_string(),
            _ => visibility,
        };
        if visibility != "front" && visibility != "through" {
            return Err(err("visibility must be front or through"));
        }
        let surface_only = get(selector, "surface_only").is_none_or(truthy);
        let mut mask = if surface_only { surface.to_vec() } else { vec![true; surface.len()] };
        let clip = Self::clip_mask(coords, get(selector, "clip_evidence"))?;
        mask.iter_mut().zip(&clip).for_each(|(m, c)| *m &= *c);
        if visibility == "front" {
            let Some(camera) = get(selector, "camera").filter(|c| c.is_object()) else {
                return Err(err("front-visible selection requires exact camera evidence for occlusion"));
            };
            let mna = py_float(get(selector, "minimum_normal_alignment").unwrap_or(&json!(0.01)))?;
            let facing = self.front_facing(coords, camera, mna)?;
            mask.iter_mut().zip(&facing).for_each(|(m, f)| *m &= *f);
            let visible = self.visible_from_camera(&mask, camera, Some(&clip))?;
            mask.iter_mut().zip(&visible).for_each(|(m, v)| *m &= *v);
        }
        Ok(mask)
    }

    fn snap(&self, point: Option<&Value>, mode: &str, surface: &[bool]) -> AResult<([usize; 3], [f64; 3])> {
        let world = vec3(point, "selection point")?;
        let reg = &self.grid.registration;
        let idx = reg
            .nearest_index(world, false)
            .map_err(|_| err("selection point lies outside the registered field"))?;
        let mut index = [idx[0] as usize, idx[1] as usize, idx[2] as usize];
        if mode == "feature" && !surface[self.grid.flat(index[0], index[1], index[2])] {
            let coords = self.grid.coordinates();
            let mut best: Option<(f64, [usize; 3])> = None;
            let s = self.grid.shape;
            for i in 0..s[0] {
                for j in 0..s[1] {
                    for k in 0..s[2] {
                        let f = self.grid.flat(i, j, k);
                        if !surface[f] {
                            continue;
                        }
                        let d: [f64; 3] = std::array::from_fn(|a| coords[f][a] - world[a]);
                        let dist = row_norm3(d);
                        if best.is_none_or(|(b, _)| dist < b) {
                            best = Some((dist, [i, j, k]));
                        }
                    }
                }
            }
            let Some((_, b)) = best else {
                return Err(err("the active field has no selectable surface feature"));
            };
            index = b;
        }
        let snapped = match mode {
            "none" => world,
            "grid" => {
                let m = reg.matrix();
                let rhs: [f64; 3] = std::array::from_fn(|a| world[a] - reg.origin[a]);
                let sol = solve(&m, &rhs).ok_or_else(|| crate::py::value_error("Singular matrix"))?;
                let vertex: [f64; 3] =
                    std::array::from_fn(|a| sol[a].round_ties_even().max(0.0).min(self.grid.shape[a] as f64));
                std::array::from_fn(|r| reg.origin[r] + crate::field_interaction::matvec_row(&m[r], vertex))
            }
            _ => reg.index_to_world([index[0] as f64, index[1] as f64, index[2] as f64]),
        };
        Ok((index, snapped))
    }

    fn candidate(&self, selector: &Value) -> AResult<(Vec<bool>, Option<Value>)> {
        let kind = crate::py::lower_str(get(selector, "kind"), "click");
        if !SELECTION_KINDS.contains(&kind.as_str()) {
            return Err(err(format!("unsupported selection kind: {}", repr(&Value::from(kind)))));
        }
        let snap = crate::py::lower_str(get(selector, "snap"), "cell");
        if !["none", "cell", "grid", "feature"].contains(&snap.as_str()) {
            return Err(err("snap must be none, cell, grid or feature"));
        }
        let coords = self.grid.coordinates();
        let surface = self.surface();
        let view = self.view_mask(&coords, selector, &surface)?;
        let shape = self.grid.shape;
        let seed: Option<[usize; 3]>;
        let mut snapped: Option<Value> = None;
        let first_true = |m: &[bool]| -> Option<[usize; 3]> {
            m.iter()
                .position(|v| *v)
                .map(|f| [f / (shape[1] * shape[2]), (f / shape[2]) % shape[1], f % shape[2]])
        };
        let mut candidate;
        match kind.as_str() {
            "click" | "flood" => {
                let mut point = get(selector, "point_mm").cloned();
                if point.as_ref().is_none_or(Value::is_null)
                    && let Some(raw) = get(selector, "index").filter(|v| !v.is_null())
                {
                    let items: Vec<i64> = raw
                        .as_array()
                        .ok_or_else(|| crate::py::type_error("'int' object is not iterable"))?
                        .iter()
                        .map(py_int)
                        .collect::<AResult<_>>()?;
                    if items.len() != 3 || (0..3).any(|a| items[a] < 0 || items[a] >= shape[a] as i64) {
                        return Err(err("selection index lies outside the registered field"));
                    }
                    let w = self.grid.registration.index_to_world([
                        items[0] as f64,
                        items[1] as f64,
                        items[2] as f64,
                    ]);
                    point = Some(jfs(&w));
                }
                let (s, sw) = self.snap(point.as_ref(), &snap, &surface)?;
                snapped = Some(json!({"index": s, "point_mm": jfs(&sw), "mode": snap}));
                if !view[self.grid.flat(s[0], s[1], s[2])] {
                    return Err(err(
                        "the picked cell is not selectable in the current visibility and section mode",
                    ));
                }
                candidate = vec![false; view.len()];
                candidate[self.grid.flat(s[0], s[1], s[2])] = true;
                if kind == "flood" {
                    candidate = component(&view, shape, s);
                }
                seed = Some(s);
            }
            "brush" => {
                let raw = get(selector, "points_mm")
                    .cloned()
                    .unwrap_or_else(|| json!([get(selector, "point_mm").cloned().unwrap_or(Value::Null)]));
                let pts = Arr::from_json(&raw).map_err(|_| err("brush points_mm must have shape (n, 3)"))?;
                if pts.ndim() != 2 || pts.shape[1] != 3 || !pts.all_finite() {
                    return Err(err("brush points_mm must have shape (n, 3)"));
                }
                let radius = py_float(get(selector, "radius_mm").unwrap_or(&json!(1.0)))?;
                if !radius.is_finite() || radius <= 0.0 {
                    return Err(err("brush radius_mm must be positive"));
                }
                let mut distances = vec![f64::INFINITY; coords.len()];
                let mut snapped_points = Vec::new();
                let points: Vec<[f64; 3]> = pts.data.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
                for p in &points {
                    let (_, eff) = self.snap(Some(&jfs(p)), &snap, &surface)?;
                    snapped_points.push(jfs(&eff));
                    for (d, c) in distances.iter_mut().zip(&coords) {
                        let r: [f64; 3] = std::array::from_fn(|a| c[a] - eff[a]);
                        let v = row_norm3(r);
                        if v < *d || v.is_nan() {
                            *d = v;
                        }
                    }
                }
                candidate = distances.iter().zip(&view).map(|(d, v)| *d <= radius + 1.0e-12 && *v).collect();
                let last = points.last().copied().unwrap_or([f64::NAN; 3]);
                let (s, sw) = self.snap(Some(&jfs(&last)), &snap, &surface)?;
                seed = Some(s);
                snapped = Some(
                    json!({"index": s, "point_mm": jfs(&sw), "mode": snap, "path_points_mm": snapped_points}),
                );
            }
            "box" | "lasso" => {
                let Some(camera) = get(selector, "camera").filter(|c| c.is_object()) else {
                    return Err(err(format!("{kind} selection requires exact camera evidence")));
                };
                let (screen, _depth, valid) = project(&coords, camera)?;
                let inside: Vec<bool> = if kind == "box" {
                    let raw = get(selector, "screen_bounds_px").or_else(|| get(selector, "screen_points_px"));
                    let b = Arr::from_opt(raw)
                        .map_err(|_| err("box selection requires two finite screen corners"))?;
                    if b.shape != [2, 2] || !b.all_finite() {
                        return Err(err("box selection requires two finite screen corners"));
                    }
                    let lo = [b.data[0].min(b.data[2]), b.data[1].min(b.data[3])];
                    let hi = [b.data[0].max(b.data[2]), b.data[1].max(b.data[3])];
                    screen
                        .iter()
                        .zip(&valid)
                        .map(|(s, v)| *v && s[0] >= lo[0] && s[1] >= lo[1] && s[0] <= hi[0] && s[1] <= hi[1])
                        .collect()
                } else {
                    let poly = Arr::from_opt(get(selector, "screen_points_px"))
                        .map_err(|_| err("lasso requires at least three finite screen points"))?;
                    let ins = inside_polygon(&screen, &poly)?;
                    ins.iter().zip(&valid).map(|(a, b)| *a && *b).collect()
                };
                candidate = inside.iter().zip(&view).map(|(a, b)| *a && *b).collect();
                seed = first_true(&candidate);
            }
            "bounds" => {
                if let Some(raw) = get(selector, "index_bounds").filter(|v| !v.is_null()) {
                    let b = Arr::from_json(raw).map_err(|_| err("index_bounds must have shape (2, 3)"))?;
                    if b.shape != [2, 3] {
                        return Err(err("index_bounds must have shape (2, 3)"));
                    }
                    let iv: Vec<i64> = b.data.iter().map(|v| v.trunc() as i64).collect();
                    let lo: [i64; 3] = std::array::from_fn(|a| iv[a].min(iv[a + 3]));
                    let hi: [i64; 3] = std::array::from_fn(|a| iv[a].max(iv[a + 3]));
                    if (0..3).any(|a| lo[a] < 0 || hi[a] >= shape[a] as i64) {
                        return Err(err("index_bounds lie outside the registered field"));
                    }
                    candidate = vec![false; view.len()];
                    for i in 0..shape[0] {
                        for j in 0..shape[1] {
                            for k in 0..shape[2] {
                                let c = [i as i64, j as i64, k as i64];
                                let f = self.grid.flat(i, j, k);
                                candidate[f] = (0..3).all(|a| c[a] >= lo[a] && c[a] <= hi[a]) && view[f];
                            }
                        }
                    }
                    seed = Some([lo[0] as usize, lo[1] as usize, lo[2] as usize]);
                } else {
                    let b = Arr::from_opt(get(selector, "world_bounds_mm"))
                        .map_err(|_| err("world_bounds_mm must have shape (2, 3)"))?;
                    if b.shape != [2, 3] || !b.all_finite() {
                        return Err(err("world_bounds_mm must have shape (2, 3)"));
                    }
                    let lo: [f64; 3] = std::array::from_fn(|a| b.data[a].min(b.data[a + 3]));
                    let hi: [f64; 3] = std::array::from_fn(|a| b.data[a].max(b.data[a + 3]));
                    candidate = coords
                        .iter()
                        .zip(&view)
                        .map(|(c, v)| (0..3).all(|a| c[a] >= lo[a] && c[a] <= hi[a]) && *v)
                        .collect();
                    seed = first_true(&candidate);
                }
            }
            _ => {
                let s: Vec<i64> = shape.iter().map(|v| *v as i64).collect();
                let runs = decode_runs(Some(get(selector, "selected_runs").unwrap_or(&json!([]))), &s)?;
                candidate = runs.iter().zip(&view).map(|(a, b)| *a && *b).collect();
                seed = first_true(&candidate);
            }
        }
        let connected = match get(selector, "connected") {
            Some(v) => truthy(v),
            None => kind == "flood",
        };
        if connected && let Some(s) = seed {
            candidate = component(&candidate, shape, s);
        }
        Ok((candidate, snapped))
    }


    pub fn apply(&self, request: &Value) -> AResult<Self> {
        if !request.is_object() {
            return Err(err("selection operation must be an object"));
        }
        let Some(expected) = get(request, "selection_revision").filter(|v| !v.is_null()) else {
            return Err(err("selection_revision is required for every exact selection operation"));
        };
        let expected = py_int(expected)?;
        if expected != self.revision {
            return Err(err(format!("stale selection revision: expected {}, got {expected}", self.revision)));
        }
        let algebra = crate::py::lower_str(get(request, "operation"), "replace");
        if !SELECTION_ALGEBRA.contains(&algebra.as_str()) {
            return Err(err("selection operation must be replace, add, subtract or intersect"));
        }
        let selector = get(request, "selector").unwrap_or(request);
        if !selector.is_object() {
            return Err(err("selection selector must be an object"));
        }
        let (candidate, snapped) = self.candidate(selector)?;
        let n = candidate.len();
        let mut selected = vec![false; n];
        for i in 0..n {
            let operable = candidate[i] && !self.protected[i];
            selected[i] = match algebra.as_str() {
                "replace" => operable,
                "add" => self.selected[i] || operable,
                "subtract" => self.selected[i] && !operable,
                _ => self.selected[i] && operable,
            } && !self.protected[i];
        }
        let mut provenance = self.provenance.clone();
        provenance.push(json!({
            "sequence": self.revision + 1,
            "operation": algebra,
            "selector": selector,
            "base_definition_id": self.serialise(false)["definition_id"],
        }));
        let mut out = Self::new(
            &self.field_id,
            &self.values,
            self.grid.clone(),
            Some(&self.id),
            self.revision + 1,
            Some(&selected),
            &self.protected_masks,
            Some(&self.field_identity),
            &provenance,
            self.threshold,
        )?;
        let by_reason: Map<String, Value> = self
            .protected_masks
            .iter()
            .map(|(r, m)| (r.clone(), json!(candidate.iter().zip(m).filter(|(c, m)| **c && **m).count())))
            .collect();
        out.last_counts = json!({
            "selected": count(&selected),
            "candidate": count(&candidate),
            "affected": selected.iter().zip(&self.selected).filter(|(a, b)| a != b).count(),
            "protected": candidate.iter().zip(&self.protected).filter(|(c, p)| **c && **p).count(),
            "protected_total": count(&self.protected),
            "protected_by_reason": by_reason,
        });
        out.last_snapped = snapped;
        Ok(out)
    }

    #[must_use]
    pub fn serialise(&self, include_provenance: bool) -> Value {
        let by_reason: Map<String, Value> = self
            .protected_masks
            .iter()
            .map(|(r, m)| (r.clone(), json!({"count": count(m), "runs": encode_runs(m)})))
            .collect();
        let mut payload = Map::new();
        payload.insert("schema".into(), json!(SELECTION_SCHEMA));
        payload.insert("kind".into(), json!("cell_selection"));
        payload.insert("id".into(), json!(self.id));
        payload.insert("revision".into(), json!(self.revision));
        payload.insert("field_id".into(), json!(self.field_id));
        payload.insert("field_identity".into(), self.field_identity.clone());
        payload.insert("shape".into(), json!(self.grid.shape));
        payload.insert("grid".into(), self.grid.serialise());
        payload.insert("threshold".into(), jf(self.threshold));
        payload.insert("selected_runs".into(), encode_runs(&self.selected));
        payload.insert("protected_runs".into(), encode_runs(&self.protected));
        payload.insert("protected_by_reason".into(), Value::Object(by_reason));
        payload.insert("counts".into(), self.last_counts.clone());
        payload.insert("snapped".into(), self.last_snapped.clone().unwrap_or(Value::Null));
        if include_provenance {
            payload.insert("provenance".into(), Value::Array(self.provenance.clone()));
        }
        let identity: Map<String, Value> = payload
            .iter()
            .filter(|(k, _)| !["definition_id", "counts", "snapped"].contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        payload.insert("definition_id".into(), Value::from(content_id(&Value::Object(identity))));
        Value::Object(payload)
    }
}


pub fn write_spatial_selection(
    document: &Value,
    selection: &SpatialSelection,
    name: Option<&str>,
) -> AResult<Value> {
    let mut out = document.clone();
    let root = obj_mut(&mut out)?;
    let interaction = if root.get("schema").is_some_and(truthy) {
        setdefault_obj(setdefault_obj(setdefault_obj(root, "meta")?, "implexity")?, "interaction")?
    } else {
        setdefault_obj(setdefault_obj(root, "extensions")?, "interaction")?
    };
    let mut encoded = selection.serialise(true);
    {
        let selections = setdefault_list(interaction, "spatial_selections")?;
        let previous =
            selections.iter().find(|i| get(i, "id").map(py_str).as_deref() == Some(selection.id.as_str()));
        let prev_name = previous.and_then(|p| get(p, "region_name")).filter(|v| truthy(v)).map(py_str);
        let region_name =
            name.filter(|n| !n.is_empty()).map(ToString::to_string).or(prev_name).unwrap_or_default();
        let region_name = region_name.trim().to_string();
        if !region_name.is_empty() {
            let truncated: String = region_name.chars().take(80).collect();
            if let Some(m) = encoded.as_object_mut() {
                m.insert("region_name".into(), Value::from(truncated));
            }
        }
        selections.retain(|i| get(i, "id").map(py_str).as_deref() != Some(selection.id.as_str()));
        selections.push(encoded);
    }
    interaction.insert("active_spatial_selection".into(), Value::from(selection.id.clone()));
    Ok(out)
}

#[must_use]
pub fn find_spatial_selection(document: &Value, selection_id: Option<&str>) -> Option<Value> {
    let mut interaction =
        crate::py::path_obj(document, &["meta", "implexity", "interaction"]).cloned().unwrap_or_default();
    if interaction.is_empty() {
        interaction =
            crate::py::path_obj(document, &["extensions", "interaction"]).cloned().unwrap_or_default();
    }
    let target = selection_id
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| interaction.get("active_spatial_selection").filter(|v| truthy(v)).map(py_str))
        .unwrap_or_default();
    interaction
        .get("spatial_selections")
        .and_then(Value::as_array)?
        .iter()
        .find(|i| i.is_object() && get(i, "id").map(py_str).as_deref() == Some(target.as_str()))
        .cloned()
}

#[must_use]
pub fn as_cell_region_definition(
    selection: &SpatialSelection,
    name: Option<&str>,
    region_id: Option<&str>,
) -> Value {
    let selector = selection.serialise(true);
    let rid = region_id.filter(|r| !r.is_empty()).map_or_else(
        || format!("region_{}", selection.id.strip_prefix("selection_").unwrap_or(&selection.id)),
        ToString::to_string,
    );
    json!({
        "id": rid,
        "name": name.filter(|n| !n.is_empty()).unwrap_or("Current cell selection"),
        "description": "Exact registered cell-grid selection authored in the viewport",
        "selector": selector,
    })
}


pub fn selection_weights(selection: &Value, context: &Value) -> AResult<Vec<f64>> {
    let shape = shape_of(selection, None)?;
    let selected = decode_runs(Some(get(selection, "selected_runs").unwrap_or(&json!([]))), &shape)?;
    let indices = get(context, "cell_indices").or_else(|| get(context, "node_cell_indices"));
    let field_id =
        get(context, "field_id").or_else(|| get(context, "topology_field_id")).filter(|v| !v.is_null());
    let registration = get(context, "grid_registration").or_else(|| get(context, "registration"));
    let mut reg_id =
        get(context, "registration_id").or_else(|| get(context, "topology_registration_id")).cloned();
    if let Some(r) = registration.filter(|r| r.is_object()) {
        if let Some(id) = get(r, "registration_id") {
            reg_id = Some(id.clone());
        } else if reg_id.is_none() {
            reg_id = None;
        }
    }
    let expected =
        get(selection, "grid").and_then(|g| get(g, "registration_id")).cloned().unwrap_or(Value::Null);
    if let Some(r) = reg_id.filter(|v| !v.is_null())
        && py_str(&r) != py_str(&expected)
    {
        return Err(err("cell-selection registration identity is stale"));
    }
    if let Some(f) = field_id
        && py_str(f) != get(selection, "field_id").map_or_else(|| "None".to_string(), py_str)
    {
        return Err(err("cell-selection field identity is stale"));
    }
    let Some(indices) = indices.filter(|v| !v.is_null()) else {
        return Err(err("cell-selection compilation requires exact cell_indices"));
    };
    let array = Arr::from_json(indices)?;
    if array.ndim() != 2 || array.shape[1] != 3 {
        return Err(err("cell-selection cell_indices must have shape (n, 3)"));
    }
    let s: Vec<usize> = shape.iter().map(|v| *v as usize).collect();
    Ok(array
        .data
        .chunks(3)
        .map(|c| {
            let idx: [i64; 3] = std::array::from_fn(|a| c[a].trunc() as i64);
            if (0..3).all(|a| idx[a] >= 0 && idx[a] < shape[a]) {
                let f = (idx[0] as usize * s[1] + idx[1] as usize) * s[2] + idx[2] as usize;
                if selected[f] { 1.0 } else { 0.0 }
            } else {
                0.0
            }
        })
        .collect())
}
