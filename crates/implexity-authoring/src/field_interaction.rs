// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{Map, Value, json};

use implexity_geometry::document::SCHEMA as MODEL_SCHEMA;
use implexity_geometry::document::arrays::{b64decode_strict, decode_array, encode_array, inline_entry};
use implexity_geometry::field_registration::{GridRegistration, axis_aligned_registration};
use implexity_geometry::lattice::component_field::{component_source, replace_component, split_component_id};
use implexity_geometry::value::NdArray;

use crate::error::{AResult, AuthoringError};
use crate::geometry_holds::field_held_mask;
use crate::py::{
    Arr, bool_array, get, jf, jfs, nested, nested_bool, np_sum, obj_mut, path_obj, py_float, py_int, py_str,
    repr, row_norm3, setdefault_list, setdefault_obj, sha_unicode, truthy,
};

pub const FIELD_SCHEMA: &str = "implexity-spatial-field/2";
pub const BRUSH_SCHEMA: &str = "implexity-field-brush/2";
pub const CONTROL_LATTICE_SCHEMA: &str = "implexity-control-lattice/1";
pub const CAGE_SCHEMA: &str = "implexity-deformation-cage/1";

const CLASS: &str = "FieldInteractionError";

pub(crate) fn err(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value(CLASS, message)
}

#[must_use]
pub fn stable_id(prefix: &str, value: &Value) -> String {
    format!("{prefix}{}", &sha_unicode(value)[..24])
}

fn is_model_doc(document: &Map<String, Value>) -> bool {
    document.get("schema").and_then(Value::as_str) == Some(MODEL_SCHEMA)
}


pub fn interaction_metadata(document: &mut Map<String, Value>) -> AResult<&mut Map<String, Value>> {
    if is_model_doc(document) {
        let meta = setdefault_obj(document, "meta")?;
        let implexity = setdefault_obj(meta, "implexity")?;
        return setdefault_obj(implexity, "interaction");
    }
    let ext = setdefault_obj(document, "extensions")?;
    setdefault_obj(ext, "interaction")
}


pub fn field_metadata<'a>(
    document: &'a mut Map<String, Value>,
    field_id: &str,
) -> AResult<&'a mut Map<String, Value>> {
    if is_model_doc(document) {
        let meta = setdefault_obj(document, "meta")?;
        let implexity = setdefault_obj(meta, "implexity")?;
        let fields = setdefault_obj(implexity, "spatial_fields")?;
        return setdefault_obj(fields, field_id);
    }
    let fm = setdefault_obj(document, "field_metadata")?;
    setdefault_obj(fm, field_id)
}

#[must_use]
pub fn read_field_metadata(document: &Value, field_id: &str) -> Value {
    if let Some(m) =
        path_obj(document, &["meta", "implexity", "spatial_fields"]).and_then(|f| f.get(field_id))
        && m.is_object()
    {
        return m.clone();
    }
    match path_obj(document, &["field_metadata"]).and_then(|f| f.get(field_id)) {
        Some(v) if v.is_object() => v.clone(),
        _ => json!({}),
    }
}


pub fn vec3(value: Option<&Value>, name: &str) -> AResult<[f64; 3]> {
    let bad = || err(format!("{name} must contain three finite values"));
    let a = Arr::from_opt(value).map_err(|_| bad())?;
    match a.vec3() {
        Some(v) if v.iter().all(|x| x.is_finite()) => Ok(v),
        _ => Err(bad()),
    }
}


pub fn shape3_json(value: Option<&Value>, name: &str) -> AResult<[usize; 3]> {
    let Some(Value::Array(items)) = value else {
        let t = value.map_or("NoneType", crate::py::type_name);
        return Err(crate::py::type_error(format!("object of type '{t}' has no len()")));
    };
    if items.len() != 3 {
        return Err(err(format!("{name} must contain three integers")));
    }
    let mut out = [0i64; 3];
    for (i, v) in items.iter().enumerate() {
        out[i] = py_int(v)?;
    }
    shape3(&out, name)
}


pub fn shape3(value: &[i64], name: &str) -> AResult<[usize; 3]> {
    if value.len() != 3 {
        return Err(err(format!("{name} must contain three integers")));
    }
    if value.iter().any(|v| *v < 2) {
        return Err(err(format!("{name} entries must be at least two")));
    }
    Ok([value[0] as usize, value[1] as usize, value[2] as usize])
}

fn to_i64(shape: &[usize]) -> Vec<i64> {
    shape.iter().map(|v| *v as i64).collect()
}


pub fn bounds(value: &Value) -> AResult<([f64; 3], [f64; 3])> {
    let (lo, hi) = if value.is_object() {
        let lo = vec3(crate::py::get_either(value, "min_mm", "minimum"), "bounds.min_mm")?;
        let hi = vec3(crate::py::get_either(value, "max_mm", "maximum"), "bounds.max_mm")?;
        (lo, hi)
    } else {
        let a = Arr::from_json(value).map_err(|_| err("bounds must have shape (2, 3)"))?;
        if a.shape != [2, 3] {
            return Err(err("bounds must have shape (2, 3)"));
        }
        ([a.data[0], a.data[1], a.data[2]], [a.data[3], a.data[4], a.data[5]])
    };
    if (0..3).any(|i| !(hi[i] > lo[i])) {
        return Err(err("each upper bound must exceed its lower bound"));
    }
    Ok((lo, hi))
}


pub fn falloff(r: f64, kind: &str) -> AResult<f64> {
    let t = crate::py::clip(1.0 - r, 0.0, 1.0);
    Ok(match kind {
        "constant" => {
            if r <= 1.0 {
                1.0
            } else {
                0.0
            }
        }
        "linear" => t,
        "smoothstep" => t * t * (3.0 - 2.0 * t),
        "gaussian" => {
            let edge = (-4.5f64).exp();
            crate::py::clip(((-4.5 * r * r).exp() - edge) / (1.0 - edge), 0.0, 1.0)
        }
        _ => return Err(err(format!("unsupported falloff: {}", repr(&Value::from(kind))))),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct GridGeometry {
    pub shape: [usize; 3],
    pub minimum_mm: [f64; 3],
    pub maximum_mm: [f64; 3],
    pub registration: GridRegistration,
}

fn reg_err(e: implexity_geometry::GeometryError) -> AuthoringError {
    let message = match e {
        implexity_geometry::GeometryError::Value(m) => m,
        other => other.to_string(),
    };
    AuthoringError::value("ValueError", message)
}

impl GridGeometry {

    pub fn new(
        shape: [usize; 3],
        minimum_mm: [f64; 3],
        maximum_mm: [f64; 3],
        registration: Option<GridRegistration>,
    ) -> AResult<Self> {
        let shape = shape3(&to_i64(&shape), "shape")?;
        if !minimum_mm.iter().chain(&maximum_mm).all(|v| v.is_finite()) {
            return Err(err("bounds.min_mm must contain three finite values"));
        }
        if (0..3).any(|i| !(maximum_mm[i] > minimum_mm[i])) {
            return Err(err("each upper bound must exceed its lower bound"));
        }
        let registration = match registration {
            Some(r) => r,
            None => axis_aligned_registration(shape, minimum_mm, maximum_mm, "cell").map_err(reg_err)?,
        };
        if registration.shape != shape {
            return Err(err("field shape does not match grid registration"));
        }
        Ok(Self { shape, minimum_mm, maximum_mm, registration })
    }


    pub fn from_values(shape: &[i64], bounds_mm: &Value) -> AResult<Self> {
        let shape3 = shape3(shape, "shape")?;
        if bounds_mm.get("origin").is_some() && bounds_mm.get("basis").is_some() && bounds_mm.is_object() {
            let mut wire = bounds_mm.clone();
            if let Some(m) = wire.as_object_mut() {
                m.entry("shape").or_insert_with(|| json!(shape3));
            }
            let registration = GridRegistration::from_wire(&wire)
                .map_err(|e| err(format!("invalid spatial-field registration: {e}")))?;
            if registration.shape != shape3 {
                return Err(err("spatial-field registration must match shape"));
            }
            let m = registration.matrix();
            let node = usize::from(registration.centering == "node");
            let extent: [f64; 3] = std::array::from_fn(|a| (shape3[a] - node) as f64);
            let mut lo = [f64::INFINITY; 3];
            let mut hi = [f64::NEG_INFINITY; 3];
            for i in [0.0, extent[0]] {
                for j in [0.0, extent[1]] {
                    for k in [0.0, extent[2]] {
                        let idx = [i, j, k];
                        for r in 0..3 {
                            let c = registration.origin[r] + matvec_row(&m[r], idx);
                            lo[r] = lo[r].min(c);
                            hi[r] = hi[r].max(c);
                        }
                    }
                }
            }
            return Self::new(shape3, lo, hi, Some(registration));
        }
        let (lo, hi) = bounds(bounds_mm)?;
        Self::new(shape3, lo, hi, None)
    }


    pub fn from_shape(shape: &[usize], bounds_mm: &Value) -> AResult<Self> {
        Self::from_values(&to_i64(shape), bounds_mm)
    }

    #[must_use]
    pub fn size(&self) -> usize {
        self.shape.iter().product()
    }

    #[must_use]
    pub fn flat(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    pub fn spacing_mm(&self) -> [f64; 3] {
        let m = self.registration.matrix();
        std::array::from_fn(|c| (m[0][c] * m[0][c] + m[1][c] * m[1][c] + m[2][c] * m[2][c]).sqrt())
    }

    #[must_use]
    pub fn coordinates(&self) -> Vec<[f64; 3]> {
        let m = self.registration.matrix();
        let o = self.registration.offset();
        let mut out = Vec::with_capacity(self.size());
        for i in 0..self.shape[0] {
            for j in 0..self.shape[1] {
                for k in 0..self.shape[2] {
                    let idx = [i as f64, j as f64, k as f64];
                    out.push(std::array::from_fn(|r| o[r] + matvec_row(&m[r], idx)));
                }
            }
        }
        out
    }

    #[must_use]
    pub fn serialise(&self) -> Value {
        let mut out = self.registration.to_wire();
        if let Some(m) = out.as_object_mut() {
            m.insert(
                "bounds_mm".into(),
                json!({"min_mm": jfs(&self.minimum_mm), "max_mm": jfs(&self.maximum_mm)}),
            );
            m.insert("spacing_mm".into(), jfs(&self.spacing_mm()));
        }
        out
    }
}

#[must_use]
pub fn matvec_row(row: &[f64; 3], v: [f64; 3]) -> f64 {
    row[0] * v[0] + row[1] * v[1] + row[2] * v[2]
}

#[derive(Clone, Debug, PartialEq)]
pub struct BrushSample {
    pub point_mm: [f64; 3],
    pub radius_mm: [f64; 3],
    pub strength: f64,
    pub mode: String,
    pub target: Option<f64>,
    pub falloff: String,
}

impl BrushSample {

    pub fn from_mapping(value: &Value) -> AResult<Self> {
        let point = vec3(crate::py::get_either(value, "point_mm", "point"), "point_mm")?;
        let radius_raw =
            get(value, "radius_mm").or_else(|| get(value, "radius")).cloned().unwrap_or(json!(1.0));
        let radius = if radius_raw.is_array() {
            vec3(Some(&radius_raw), "radius_mm")?
        } else {
            let s = py_float(&radius_raw)?;
            [s, s, s]
        };
        if radius.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(err("brush radii must be finite and positive"));
        }
        let strength =
            py_float(get(value, "strength").or_else(|| get(value, "delta")).unwrap_or(&json!(0.1)))?;
        if !strength.is_finite() {
            return Err(err("brush strength must be finite"));
        }
        let mode = crate::py::lower_str(get(value, "mode"), "add");
        if !["add", "subtract", "set", "smooth"].contains(&mode.as_str()) {
            return Err(err(format!("unsupported brush mode: {}", repr(&Value::from(mode)))));
        }
        let target = match get(value, "target") {
            None | Some(Value::Null) => None,
            Some(t) => Some(py_float(t)?),
        };
        let fall = crate::py::lower_str(get(value, "falloff"), "smoothstep");
        if !["constant", "linear", "smoothstep", "gaussian"].contains(&fall.as_str()) {
            return Err(err(format!("unsupported falloff: {}", repr(&Value::from(fall)))));
        }
        Ok(Self { point_mm: point, radius_mm: radius, strength, mode, target, falloff: fall })
    }

    #[must_use]
    pub fn serialise(&self) -> Value {
        json!({
            "schema": BRUSH_SCHEMA,
            "point_mm": jfs(&self.point_mm),
            "radius_mm": jfs(&self.radius_mm),
            "strength": jf(self.strength),
            "mode": self.mode,
            "target": self.target.map_or(Value::Null, jf),
            "falloff": self.falloff,
        })
    }
}

#[derive(Clone, Debug)]
pub struct FieldBrushGesture {
    pub baseline: Vec<f64>,
    pub grid: GridGeometry,
    pub lower: Option<f64>,
    pub upper: Option<f64>,
    pub samples: Vec<BrushSample>,
    coords: Vec<[f64; 3]>,
    pub protected_masks: BTreeMap<String, Vec<bool>>,
    protected: Vec<bool>,
}

impl FieldBrushGesture {

    pub fn new(
        values: &Arr,
        grid: GridGeometry,
        lower: Option<f64>,
        upper: Option<f64>,
        protected_masks: &[(String, Value)],
    ) -> AResult<Self> {
        if values.shape != grid.shape {
            return Err(err(format!(
                "field shape {} does not match grid {}",
                shape_repr(&values.shape),
                shape_repr(&grid.shape)
            )));
        }
        if !values.all_finite() {
            return Err(err("field values must be finite"));
        }
        if let (Some(l), Some(u)) = (lower, upper)
            && l > u
        {
            return Err(err("field lower bound exceeds upper bound"));
        }
        let mut masks = BTreeMap::new();
        for (name, raw) in protected_masks {
            let (shape, mask) = bool_array(raw)?;
            if shape != grid.shape {
                return Err(err(format!(
                    "protected mask {} does not match field shape",
                    repr(&Value::from(name.clone()))
                )));
            }
            masks.insert(name.clone(), mask);
        }
        let mut protected = vec![false; grid.size()];
        for m in masks.values() {
            for (p, v) in protected.iter_mut().zip(m) {
                *p |= *v;
            }
        }
        let coords = grid.coordinates();
        Ok(Self {
            baseline: values.data.clone(),
            grid,
            lower,
            upper,
            samples: Vec::new(),
            coords,
            protected_masks: masks,
            protected,
        })
    }


    pub fn add_sample(&mut self, sample: &Value) -> AResult<()> {
        self.samples.push(BrushSample::from_mapping(sample)?);
        Ok(())
    }

    fn sample_weights(&self, s: &BrushSample) -> AResult<Vec<f64>> {
        self.coords
            .iter()
            .map(|c| {
                let rel: [f64; 3] = std::array::from_fn(|a| (c[a] - s.point_mm[a]) / s.radius_mm[a]);
                falloff(row_norm3(rel), &s.falloff)
            })
            .collect()
    }

    #[must_use]
    pub fn smooth(values: &[f64], shape: [usize; 3]) -> Vec<f64> {
        let [nx, ny, nz] = shape;
        let at = |i: isize, j: isize, k: isize| -> f64 {
            let c = |v: isize, n: usize| v.clamp(0, n as isize - 1) as usize;
            values[(c(i, nx) * ny + c(j, ny)) * nz + c(k, nz)]
        };
        let mut out = vec![0.0; values.len()];
        for di in 0..3isize {
            for dj in 0..3isize {
                for dk in 0..3isize {
                    for i in 0..nx {
                        for j in 0..ny {
                            for k in 0..nz {
                                out[(i * ny + j) * nz + k] +=
                                    at(i as isize + di - 1, j as isize + dj - 1, k as isize + dk - 1);
                            }
                        }
                    }
                }
            }
        }
        out.iter().map(|v| v / 27.0).collect()
    }


    pub fn preview(&self) -> AResult<Vec<f64>> {
        let mut values = self.baseline.clone();
        for s in &self.samples {
            let w = self.sample_weights(s)?;
            match s.mode.as_str() {
                "add" => values.iter_mut().zip(&w).for_each(|(v, w)| *v += s.strength * w),
                "subtract" => values.iter_mut().zip(&w).for_each(|(v, w)| *v -= s.strength.abs() * w),
                "set" => {
                    let target = s.target.unwrap_or(s.strength);
                    for (v, w) in values.iter_mut().zip(&w) {
                        let blend = crate::py::clip(s.strength.abs() * w, 0.0, 1.0);
                        *v = *v * (1.0 - blend) + target * blend;
                    }
                }
                _ => {
                    let smoothed = Self::smooth(&values, self.grid.shape);
                    for ((v, w), sm) in values.iter_mut().zip(&w).zip(&smoothed) {
                        let blend = crate::py::clip(s.strength.abs() * w, 0.0, 1.0);
                        *v = *v * (1.0 - blend) + sm * blend;
                    }
                }
            }
            if self.lower.is_some() || self.upper.is_some() {
                let lo = self.lower.unwrap_or(f64::NEG_INFINITY);
                let hi = self.upper.unwrap_or(f64::INFINITY);
                for v in &mut values {
                    *v = np_clip(*v, lo, hi);
                }
            }
            for (i, p) in self.protected.iter().enumerate() {
                if *p {
                    values[i] = self.baseline[i];
                }
            }
        }
        Ok(values)
    }

    #[must_use]
    pub fn serialise(&self) -> Value {
        let masks: Map<String, Value> = self
            .protected_masks
            .iter()
            .map(|(k, m)| {
                (k.clone(), json!({"count": m.iter().filter(|v| **v).count(), "shape": self.grid.shape}))
            })
            .collect();
        let mut payload = json!({
            "schema": BRUSH_SCHEMA,
            "grid": self.grid.serialise(),
            "lower": self.lower.map_or(Value::Null, jf),
            "upper": self.upper.map_or(Value::Null, jf),
            "samples": self.samples.iter().map(BrushSample::serialise).collect::<Vec<_>>(),
            "protected_masks": masks,
        });
        let id = stable_id("brush_", &payload);
        if let Some(m) = payload.as_object_mut() {
            m.insert("gesture_id".into(), Value::from(id));
        }
        payload
    }
}

#[must_use]
pub fn np_clip(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_nan() { v } else { v.max(lo).min(hi) }
}

#[must_use]
pub fn shape_repr(shape: &[usize]) -> String {
    implexity_geometry::pyfmt::shape_str(shape)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ControlLattice {
    pub bounds_min_mm: [f64; 3],
    pub bounds_max_mm: [f64; 3],
    pub offsets_mm: Vec<f64>,
    pub shape: [usize; 3],
    pub locked: Vec<bool>,
}

impl ControlLattice {

    pub fn create(bounds_mm: &Value, shape: [i64; 3]) -> AResult<Self> {
        let (lo, hi) = bounds(bounds_mm)?;
        let shape = shape3(&shape, "shape")?;
        let n = shape.iter().product::<usize>();
        Ok(Self {
            bounds_min_mm: lo,
            bounds_max_mm: hi,
            offsets_mm: vec![0.0; n * 3],
            shape,
            locked: vec![false; n],
        })
    }


    pub fn from_mapping(value: &Value) -> AResult<Self> {
        let b = get(value, "bounds_mm").or_else(|| get(value, "bounds")).cloned().unwrap_or(Value::Null);
        let (lo, hi) = bounds(&b)?;
        let offsets = Arr::from_opt(get(value, "offsets_mm").or_else(|| get(value, "offsets")))?;
        if offsets.ndim() != 4 || offsets.shape[3] != 3 || offsets.shape[..3].iter().any(|s| *s < 2) {
            return Err(err("control lattice offsets must have shape (nx, ny, nz, 3), each n >= 2"));
        }
        let shape = [offsets.shape[0], offsets.shape[1], offsets.shape[2]];
        let locked = match get(value, "locked") {
            None => vec![false; shape.iter().product()],
            Some(raw) => {
                let (s, m) = bool_array(raw)?;
                if s != shape {
                    return Err(err("control lattice lock mask has the wrong shape"));
                }
                m
            }
        };
        Ok(Self { bounds_min_mm: lo, bounds_max_mm: hi, offsets_mm: offsets.data, shape, locked })
    }

    fn idx(&self, i: usize, j: usize, k: usize) -> usize {
        (i * self.shape[1] + j) * self.shape[2] + k
    }

    #[must_use]
    pub fn control_positions_mm(&self) -> Vec<f64> {
        let axes: Vec<Vec<f64>> = (0..3)
            .map(|a| {
                implexity_mesh::numeric::linspace(self.bounds_min_mm[a], self.bounds_max_mm[a], self.shape[a])
            })
            .collect();
        let mut out = self.offsets_mm.clone();
        for i in 0..self.shape[0] {
            for j in 0..self.shape[1] {
                for k in 0..self.shape[2] {
                    let n = self.idx(i, j, k) * 3;
                    let base = [axes[0][i], axes[1][j], axes[2][k]];
                    for a in 0..3 {
                        out[n + a] = base[a] + self.offsets_mm[n + a];
                    }
                }
            }
        }
        out
    }


    pub fn move_control(
        &mut self,
        index: [i64; 3],
        delta_mm: &Value,
        influence_radius: f64,
        falloff_kind: &str,
        symmetry_axes: &[i64],
    ) -> AResult<()> {
        if (0..3).any(|d| index[d] < 0 || index[d] >= self.shape[d] as i64) {
            return Err(err("control-point index lies outside the lattice"));
        }
        let delta = vec3(Some(delta_mm), "delta_mm")?;
        let idx = [index[0] as usize, index[1] as usize, index[2] as usize];
        let flat = self.idx(idx[0], idx[1], idx[2]);
        if self.locked[flat] {
            return Ok(());
        }
        if influence_radius <= 0.0 {
            for a in 0..3 {
                self.offsets_mm[flat * 3 + a] += delta[a];
            }
        } else {
            for i in 0..self.shape[0] {
                for j in 0..self.shape[1] {
                    for k in 0..self.shape[2] {
                        let d =
                            [i as f64 - idx[0] as f64, j as f64 - idx[1] as f64, k as f64 - idx[2] as f64];
                        let distance = row_norm3(d) / influence_radius;
                        let n = self.idx(i, j, k);
                        let w = if self.locked[n] { 0.0 } else { falloff(distance, falloff_kind)? };
                        for a in 0..3 {
                            self.offsets_mm[n * 3 + a] += w * delta[a];
                        }
                    }
                }
            }
        }
        for axis in symmetry_axes {
            if !(0..=2).contains(axis) {
                return Err(err("symmetry axis must be 0, 1 or 2"));
            }
            let axis = *axis as usize;
            let mut mirror = idx;
            mirror[axis] = self.shape[axis] - 1 - mirror[axis];
            let m = self.idx(mirror[0], mirror[1], mirror[2]);
            if mirror != idx && !self.locked[m] {
                for a in 0..3 {
                    let sign = if a == axis { -1.0 } else { 1.0 };
                    self.offsets_mm[m * 3 + a] = self.offsets_mm[flat * 3 + a] * sign;
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn displacement_mm(&self, points: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let ext: [f64; 3] = std::array::from_fn(|a| self.bounds_max_mm[a] - self.bounds_min_mm[a]);
        points
            .iter()
            .map(|p| {
                let mut lower = [0usize; 3];
                let mut upper = [0usize; 3];
                let mut frac = [0.0; 3];
                for a in 0..3 {
                    let u = crate::py::clip((p[a] - self.bounds_min_mm[a]) / ext[a], 0.0, 1.0)
                        * (self.shape[a] as f64 - 1.0);
                    let l = u.floor();
                    lower[a] = l as usize;
                    upper[a] = (lower[a] + 1).min(self.shape[a] - 1);
                    frac[a] = u - l;
                }
                let mut out = [0.0; 3];
                for bx in 0..2 {
                    for by in 0..2 {
                        for bz in 0..2 {
                            let choose = [bx, by, bz];
                            let ii: [usize; 3] =
                                std::array::from_fn(|a| if choose[a] == 0 { lower[a] } else { upper[a] });
                            let wf: [f64; 3] =
                                std::array::from_fn(|a| if choose[a] == 0 { 1.0 - frac[a] } else { frac[a] });
                            let w = wf[0] * wf[1] * wf[2];
                            let n = self.idx(ii[0], ii[1], ii[2]) * 3;
                            for a in 0..3 {
                                out[a] += w * self.offsets_mm[n + a];
                            }
                        }
                    }
                }
                out
            })
            .collect()
    }

    #[must_use]
    pub fn deform_points_mm(&self, points: &[[f64; 3]]) -> Vec<[f64; 3]> {
        let d = self.displacement_mm(points);
        points.iter().zip(d).map(|(p, d)| std::array::from_fn(|a| p[a] + d[a])).collect()
    }

    #[must_use]
    pub fn inverse_points_mm(
        &self,
        deformed: &[[f64; 3]],
        iterations: usize,
        tolerance_mm: f64,
    ) -> Vec<[f64; 3]> {
        let mut estimate = deformed.to_vec();
        for _ in 0..iterations {
            let disp = self.displacement_mm(&estimate);
            let residual: Vec<[f64; 3]> = estimate
                .iter()
                .zip(&disp)
                .zip(deformed)
                .map(|((e, d), t)| std::array::from_fn(|a| e[a] + d[a] - t[a]))
                .collect();
            for (e, r) in estimate.iter_mut().zip(&residual) {
                for a in 0..3 {
                    e[a] -= r[a];
                }
            }
            let worst = residual.iter().map(|r| row_norm3(*r)).fold(f64::NEG_INFINITY, f64::max);
            if worst <= tolerance_mm {
                break;
            }
        }
        estimate
    }

    #[must_use]
    pub fn laplacian_energy(&self) -> f64 {
        let [nx, ny, nz] = self.shape;
        let mut energy = 0.0;
        for axis in 0..3 {
            let mut diffs = Vec::new();
            let lim = match axis {
                0 => [nx - 1, ny, nz],
                1 => [nx, ny - 1, nz],
                _ => [nx, ny, nz - 1],
            };
            for i in 0..lim[0] {
                for j in 0..lim[1] {
                    for k in 0..lim[2] {
                        let (i2, j2, k2) = match axis {
                            0 => (i + 1, j, k),
                            1 => (i, j + 1, k),
                            _ => (i, j, k + 1),
                        };
                        for a in 0..3 {
                            let d = self.offsets_mm[self.idx(i2, j2, k2) * 3 + a]
                                - self.offsets_mm[self.idx(i, j, k) * 3 + a];
                            diffs.push(d * d);
                        }
                    }
                }
            }
            energy += np_sum(&diffs);
        }
        energy
    }

    #[must_use]
    pub fn serialise(&self) -> Value {
        let s = self.shape;
        let mut payload = json!({
            "schema": CONTROL_LATTICE_SCHEMA,
            "bounds_mm": {"min_mm": jfs(&self.bounds_min_mm), "max_mm": jfs(&self.bounds_max_mm)},
            "shape": s,
            "offsets_mm": nested(&[s[0], s[1], s[2], 3], &self.offsets_mm),
            "locked": nested_bool(&s, &self.locked),
            "regularisation": {"kind": "edge_laplacian", "energy": jf(self.laplacian_energy())},
        });
        let id = stable_id("ctl_", &payload);
        if let Some(m) = payload.as_object_mut() {
            m.insert("id".into(), Value::from(id));
        }
        payload
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeformationCage {
    pub lattice: ControlLattice,
    pub target: Value,
    pub enabled: bool,
}

impl DeformationCage {

    pub fn create(bounds_mm: &Value, shape: [i64; 3], target: &Value) -> AResult<Self> {
        Ok(Self { lattice: ControlLattice::create(bounds_mm, shape)?, target: target.clone(), enabled: true })
    }


    pub fn from_mapping(value: &Value) -> AResult<Self> {
        let lattice_value = get(value, "lattice").unwrap_or(value);
        let target = get(value, "target")
            .filter(|t| t.is_object())
            .ok_or_else(|| err("deformation cage requires a target selector"))?;
        Ok(Self {
            lattice: ControlLattice::from_mapping(lattice_value)?,
            target: target.clone(),
            enabled: get(value, "enabled").is_none_or(truthy),
        })
    }

    #[must_use]
    pub fn serialise(&self) -> Value {
        let mut payload = json!({
            "schema": CAGE_SCHEMA,
            "kind": "deformation_cage",
            "enabled": self.enabled,
            "target": self.target,
            "lattice": self.lattice.serialise(),
        });
        let id = stable_id("cage_", &payload);
        if let Some(m) = payload.as_object_mut() {
            m.insert("id".into(), Value::from(id));
        }
        payload
    }
}


pub fn write_control_lattice(document: &Value, cage: &DeformationCage) -> AResult<Value> {
    let mut out = document.clone();
    let interaction = interaction_metadata(obj_mut(&mut out)?)?;
    let cages = setdefault_list(interaction, "deformation_cages")?;
    let encoded = cage.serialise();
    let id = encoded.get("id").cloned();
    cages.retain(|item| item.get("id") != id.as_ref());
    cages.push(encoded);
    Ok(out)
}


pub fn promote_cage_to_shape_coordinate(
    document: &Value,
    cage_id: &str,
    coordinate: Option<&str>,
) -> AResult<Value> {
    let mut out = document.clone();
    let modern = ["meta", "implexity", "interaction", "deformation_cages"];
    let legacy = ["extensions", "interaction", "deformation_cages"];
    let use_modern = crate::py::path(&out, &modern).is_some_and(truthy);
    let use_legacy = !use_modern && crate::py::path(&out, &legacy).is_some_and(truthy);
    let cages: Vec<Value> = if use_modern {
        crate::py::path(&out, &modern).and_then(Value::as_array).cloned().unwrap_or_default()
    } else if use_legacy {
        crate::py::path(&out, &legacy).and_then(Value::as_array).cloned().unwrap_or_default()
    } else {
        Vec::new()
    };
    let position = cages.iter().position(|x| get(x, "id").map(py_str).as_deref() == Some(cage_id));
    let Some(position) = position else {
        return Err(err(format!("deformation cage {} was not found", repr(&Value::from(cage_id)))));
    };
    let cage = &cages[position];
    let lattice = get(cage, "lattice").filter(|l| truthy(l)).cloned().unwrap_or_else(|| json!({}));
    let shape = shape3_json(get(&lattice, "shape"), "cage shape")?;
    let coord = coordinate.map_or_else(|| format!("model:shape:{cage_id}"), ToString::to_string);
    let offsets = Arr::from_opt(get(&lattice, "offsets_mm"))?;
    if offsets.shape != [shape[0], shape[1], shape[2], 3] {
        return Err(err("deformation cage control-point offsets are malformed"));
    }
    let key = format!("shape_{}", stable_id("coord_", &json!({"coordinate": coord})));
    let units = out.get("units").cloned().unwrap_or_else(|| json!("mm"));
    let entry = json!({"schema": "implexity-shape-coordinate/1", "shape": offsets.shape,
        "dtype": "float64", "values": jfs(&offsets.data), "units": units, "cage_id": cage_id});
    let root = obj_mut(&mut out)?;
    setdefault_obj(root, "arrays")?.insert(key.clone(), entry);
    {
        let meta = setdefault_obj(setdefault_obj(root, "meta")?, "implexity")?;
        let design = setdefault_list(meta, "design_coordinates")?;
        design.retain(|x| get(x, "coordinate") != Some(&Value::from(coord.clone())));
        design.push(json!({"coordinate": coord, "kind": "shape_free", "array": key,
            "cage_id": cage_id, "lower": -1.0, "upper": 1.0, "basis": "deformation_cage_trilinear"}));
    }
    let list_path: &[&str] = if use_modern { &modern } else { &legacy };
    let mut cur = &mut out;
    for k in list_path {
        cur = cur.get_mut(*k).ok_or_else(|| err("deformation cage list vanished"))?;
    }
    let cage = cur
        .get_mut(position)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| err("deformation cage list vanished"))?;
    let opt = setdefault_obj(cage, "optimization")?;
    opt.insert("coordinate".into(), Value::from(coord));
    opt.insert("array".into(), Value::from(key));
    opt.insert("role".into(), Value::from("shape_free"));
    Ok(out)
}

fn validate_binary_mask(raw: &Value, size: usize) -> Option<Vec<bool>> {
    let flat = flatten_json(raw)?;
    if flat.len() != size {
        return None;
    }
    flat.iter()
        .map(|v| match v {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => {
                let f = n.as_f64()?;
                if f == 0.0 {
                    Some(false)
                } else if f == 1.0 {
                    Some(true)
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

#[must_use]
pub fn flatten_json(v: &Value) -> Option<Vec<&Value>> {
    fn go<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| go(x, out)),
            other => out.push(other),
        }
    }
    let mut out = Vec::new();
    if !v.is_array() {
        out.push(v);
        return Some(out);
    }
    go(v, &mut out);
    Some(out)
}


pub fn write_spatial_field(
    document: &Value,
    field_id: &str,
    values: &[f64],
    grid: &GridGeometry,
    lower: Option<f64>,
    upper: Option<f64>,
    protected_masks: Option<&[(String, Value)]>,
) -> AResult<Value> {
    let protected: Option<Vec<(String, Value)>> = protected_masks
        .map(|m| m.iter().filter(|(k, _)| k != "manual_and_optimization_hold").cloned().collect());
    if values.len() != grid.size() || !values.iter().all(|v| v.is_finite()) {
        return Err(err("spatial field values are incompatible with the grid"));
    }
    let mut array = values.to_vec();
    let held = field_held_mask(document, field_id, &grid.shape)?;
    if held.iter().any(|h| *h) {
        let base = read_spatial_field(document, field_id)?;
        if base.grid.registration.to_wire() != grid.registration.to_wire() {
            return Err(err("held field registration changed"));
        }
        for (i, h) in held.iter().enumerate() {
            if *h {
                array[i] = base.values[i];
            }
        }
    }
    if split_component_id(field_id).map_err(AuthoringError::from)?.is_some() {
        if let Some(l) = lower
            && array.iter().any(|v| *v < l)
        {
            return Err(err("native component violates lower bound"));
        }
        if let Some(u) = upper
            && array.iter().any(|v| *v > u)
        {
            return Err(err("native component violates upper bound"));
        }
        let mut out = replace_component(document, field_id, &array, &grid.serialise())?;
        if let Some(masks) = &protected {
            let mut encoded = Map::new();
            for (name, value) in masks {
                let Some(m) = validate_binary_mask(value, grid.size()) else {
                    return Err(err("invalid protected native component mask"));
                };
                let shape_ok = match bool_array(value) {
                    Ok((s, _)) => s == grid.shape,
                    Err(_) => false,
                };
                if !shape_ok {
                    return Err(err("invalid protected native component mask"));
                }
                encoded.insert(name.clone(), Value::Array(m.into_iter().map(Value::Bool).collect()));
            }
            field_metadata(obj_mut(&mut out)?, field_id)?
                .insert("protected_masks".into(), Value::Object(encoded));
        }
        return Ok(out);
    }
    let mut out = document.clone();
    let grid_wire = grid.serialise();
    let bounds_v = json!({"lower": lower.map_or(Value::Null, jf), "upper": upper.map_or(Value::Null, jf)});
    let mut protected_encoded: Option<Map<String, Value>> = None;
    if let Some(masks) = &protected {
        let mut enc = Map::new();
        for (name, raw) in masks {
            let (shape, mask) = bool_array(raw)?;
            if shape != grid.shape {
                return Err(err(format!(
                    "protected mask {} does not match field shape",
                    repr(&Value::from(name.clone()))
                )));
            }
            enc.insert(name.clone(), Value::Array(mask.into_iter().map(Value::Bool).collect()));
        }
        protected_encoded = Some(enc);
    }
    let encoded_values = jfs(&array);
    let root = obj_mut(&mut out)?;
    let store_key = ["arrays", "array_store", "arrayStore"]
        .iter()
        .find(|k| root.get(**k).is_some_and(Value::is_object))
        .map(|k| (*k).to_string());
    let store_key = if let Some(k) = store_key {
        k
    } else {
        root.insert("arrays".into(), json!({}));
        "arrays".to_string()
    };
    let existing = root.get(&store_key).and_then(|s| s.get(field_id)).cloned();
    match existing {
        Some(Value::Object(e)) if e.contains_key("b64") || e.contains_key("file") => {
            let nd = NdArray::from_f64(grid.shape.to_vec(), array.clone())
                .ok_or_else(|| err("spatial field values are incompatible with the grid"))?;
            let (entry, raw) = encode_array(&nd);
            if let Some(store) = root.get_mut(&store_key).and_then(Value::as_object_mut) {
                store.insert(field_id.to_string(), inline_entry(&entry, &raw));
            }
            let metadata = field_metadata(root, field_id)?;
            metadata.insert("schema".into(), Value::from(FIELD_SCHEMA));
            metadata.insert("grid".into(), grid_wire.clone());
            metadata.insert("bounds_mm".into(), grid_wire["bounds_mm"].clone());
            metadata.insert("bounds".into(), bounds_v);
            if let Some(p) = protected_encoded {
                metadata.insert("protected_masks".into(), Value::Object(p));
            }
        }
        Some(Value::Object(e)) => {
            let mut updated = e.clone();
            let data_key = ["values", "data", "flat", "payload"]
                .iter()
                .find(|k| updated.contains_key(**k))
                .copied()
                .unwrap_or("values");
            updated.insert(data_key.into(), encoded_values);
            updated.insert("shape".into(), json!(grid.shape));
            updated.entry("dtype").or_insert_with(|| json!("float64"));
            updated.insert("grid".into(), grid_wire);
            updated.entry("bounds").or_insert(bounds_v);
            updated.entry("schema").or_insert_with(|| json!(FIELD_SCHEMA));
            if let Some(p) = protected_encoded {
                updated.insert("protected_masks".into(), Value::Object(p));
            }
            if let Some(store) = root.get_mut(&store_key).and_then(Value::as_object_mut) {
                store.insert(field_id.to_string(), Value::Object(updated));
            }
        }
        Some(Value::Array(_)) => {
            if let Some(store) = root.get_mut(&store_key).and_then(Value::as_object_mut) {
                store.insert(field_id.to_string(), nested(&grid.shape, &array));
            }
            let metadata = field_metadata(root, field_id)?;
            metadata.insert("grid".into(), grid_wire.clone());
            metadata.insert("bounds_mm".into(), grid_wire["bounds_mm"].clone());
            if let Some(p) = protected_encoded {
                metadata.insert("protected_masks".into(), Value::Object(p));
            }
        }
        _ => {
            let mut encoded = json!({
                "schema": FIELD_SCHEMA,
                "shape": grid.shape,
                "dtype": "float64",
                "values": encoded_values,
                "grid": grid_wire,
                "bounds": bounds_v,
            });
            if let (Some(p), Some(m)) = (protected_encoded, encoded.as_object_mut()) {
                m.insert("protected_masks".into(), Value::Object(p));
            }
            if let Some(store) = root.get_mut(&store_key).and_then(Value::as_object_mut) {
                store.insert(field_id.to_string(), encoded);
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpatialFieldRead {
    pub values: Vec<f64>,
    pub grid: GridGeometry,
    pub store_key: String,
    pub data_key: Option<String>,
    pub entry: Option<Value>,
    pub metadata: Option<Value>,
}


pub fn read_spatial_field(document: &Value, field_id: &str) -> AResult<SpatialFieldRead> {
    if split_component_id(field_id)?.is_some() {
        let (base, index, tensor, registration) = component_source(document, field_id)?;
        let mut metadata = read_field_metadata(document, field_id);
        let topology = path_obj(document, &["meta", "implexity", "topology"]).cloned().unwrap_or_default();
        if metadata.get("bounds").is_none()
            && topology.get("ref").and_then(Value::as_str) == Some("model:control")
        {
            let root = document
                .get("nodes")
                .and_then(|n| n.get(document.get("root").and_then(Value::as_str).unwrap_or_default()))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if root.pointer("/params/control/array").and_then(Value::as_str) == Some(base.as_str()) {
                let lo = topology.get("lower").filter(|v| v.is_number());
                let hi = topology.get("upper").filter(|v| v.is_number());
                if let (Some(l), Some(h)) = (lo, hi) {
                    let (lf, hf) = (l.as_f64().unwrap_or(f64::NAN), h.as_f64().unwrap_or(f64::NAN));
                    if lf.is_finite()
                        && hf.is_finite()
                        && lf < hf
                        && let Some(m) = metadata.as_object_mut()
                    {
                        m.insert("bounds".into(), json!({"lower": l, "upper": h}));
                    }
                }
            }
        }
        if let Some(m) = metadata.as_object_mut() {
            m.insert("native_tensor_key".into(), Value::from(base.clone()));
            m.insert("component_index".into(), Value::from(index));
        }
        let shape = tensor.shape()[1..].to_vec();
        let n: usize = shape.iter().product();
        let all = tensor.to_f64_vec();
        let values = all[index * n..(index + 1) * n].to_vec();
        let grid = GridGeometry::from_shape(&shape, &registration)?;
        let entry = document.get("arrays").and_then(|a| a.get(&base)).cloned();
        return Ok(SpatialFieldRead {
            values,
            grid,
            store_key: "arrays".into(),
            data_key: Some("b64".into()),
            entry,
            metadata: Some(metadata),
        });
    }
    for store_key in ["arrays", "array_store", "arrayStore"] {
        let Some(store) = document.get(store_key).and_then(Value::as_object) else { continue };
        let Some(entry) = store.get(field_id) else { continue };
        if let Some(e) = entry.as_object() {
            let shape: Vec<i64> = e
                .get("shape")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(py_int).collect::<AResult<Vec<_>>>())
                .transpose()?
                .unwrap_or_default();
            let mut data: Option<Vec<f64>> = None;
            let mut data_key = None;
            for candidate in ["values", "data", "flat", "payload"] {
                if let Some(d) = e.get(candidate) {
                    data = Some(Arr::from_json(d)?.data);
                    data_key = Some(candidate.to_string());
                    break;
                }
            }
            if data.is_none()
                && let Some(b64) = e.get("b64").filter(|v| !v.is_null())
            {
                let bad = || {
                    err(format!(
                        "field {} has an invalid inline binary payload",
                        repr(&Value::from(field_id))
                    ))
                };
                let raw = b64decode_strict(&py_str(b64)).map_err(|_| bad())?;
                let nd = decode_array(entry, &raw).map_err(|_| bad())?;
                data = Some(nd.to_f64_vec());
                data_key = Some("b64".into());
            }
            let Some(data) = data.filter(|_| shape.len() == 3) else {
                return Err(err(format!(
                    "field {} lacks a three-dimensional shape or payload",
                    repr(&Value::from(field_id))
                )));
            };
            if shape.iter().any(|v| *v < 0) || data.len() != shape.iter().product::<i64>() as usize {
                return Err(crate::py::value_error(format!(
                    "cannot reshape array of size {} into shape ({})",
                    data.len(),
                    shape.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
                )));
            }
            let grid_raw = e.get("grid").cloned().unwrap_or_else(|| json!({}));
            let metadata = read_field_metadata(document, field_id);
            let topology =
                path_obj(document, &["meta", "implexity", "topology"]).cloned().unwrap_or_default();
            let mut grid_source = if grid_raw.is_object()
                && grid_raw.get("origin").is_some()
                && grid_raw.get("basis").is_some()
            {
                Some(grid_raw.clone())
            } else if grid_raw.is_object() {
                grid_raw.get("bounds_mm").or_else(|| e.get("bounds_mm")).cloned()
            } else {
                e.get("bounds_mm").cloned()
            };
            if grid_source.as_ref().is_none_or(Value::is_null) {
                grid_source = metadata.get("grid").or_else(|| metadata.get("bounds_mm")).cloned();
            }
            if grid_source.as_ref().is_none_or(Value::is_null)
                && topology.get("array_key").map(py_str).as_deref() == Some(field_id)
            {
                grid_source = topology.get("registration").cloned();
            }
            let Some(grid_source) = grid_source.filter(|v| !v.is_null()) else {
                return Err(err(format!(
                    "field {} lacks model-coordinate bounds",
                    repr(&Value::from(field_id))
                )));
            };
            let grid = GridGeometry::from_values(&shape, &grid_source)?;
            return Ok(SpatialFieldRead {
                values: data,
                grid,
                store_key: store_key.into(),
                data_key,
                entry: Some(entry.clone()),
                metadata: Some(metadata),
            });
        }
        if entry.is_array() {
            let values = Arr::from_json(entry)?;
            if values.ndim() != 3 {
                return Err(err(format!(
                    "list-backed field {} must be three-dimensional",
                    repr(&Value::from(field_id))
                )));
            }
            let metadata = read_field_metadata(document, field_id);
            let grid_source =
                metadata.get("grid").or_else(|| metadata.get("bounds_mm")).filter(|v| !v.is_null());
            let Some(grid_source) = grid_source else {
                return Err(err(format!(
                    "list-backed field {} lacks field_metadata.bounds_mm",
                    repr(&Value::from(field_id))
                )));
            };
            let grid = GridGeometry::from_shape(&values.shape, grid_source)?;
            return Ok(SpatialFieldRead {
                values: values.data,
                grid,
                store_key: store_key.into(),
                data_key: None,
                entry: None,
                metadata: None,
            });
        }
    }
    Err(err(format!("spatial field {} was not found", repr(&Value::from(field_id)))))
}


pub fn sample_trilinear_field(values: &[f64], grid: &GridGeometry, points: &[[f64; 3]]) -> AResult<Vec<f64>> {
    if values.len() != grid.size() {
        return Err(err("field shape does not match interpolation grid"));
    }
    let mut out = Vec::with_capacity(points.len());
    for p in points {
        let uvw = grid.registration.world_to_index(*p).map_err(reg_err)?;
        let mut lower = [0usize; 3];
        let mut upper = [0usize; 3];
        let mut frac = [0.0; 3];
        for a in 0..3 {
            let u = uvw[a].max(0.0).min(grid.shape[a] as f64 - 1.0);
            let l = u.floor();
            lower[a] = l as usize;
            upper[a] = (lower[a] + 1).min(grid.shape[a] - 1);
            frac[a] = u - l;
        }
        let mut acc = 0.0;
        for bx in 0..2 {
            for by in 0..2 {
                for bz in 0..2 {
                    let choose = [bx, by, bz];
                    let ii: [usize; 3] =
                        std::array::from_fn(|a| if choose[a] == 0 { lower[a] } else { upper[a] });
                    let wf: [f64; 3] =
                        std::array::from_fn(|a| if choose[a] == 0 { 1.0 - frac[a] } else { frac[a] });
                    acc += wf[0] * wf[1] * wf[2] * values[grid.flat(ii[0], ii[1], ii[2])];
                }
            }
        }
        out.push(acc);
    }
    Ok(out)
}


pub fn deform_spatial_field(
    values: &[f64],
    grid: &GridGeometry,
    lattice: &ControlLattice,
) -> AResult<Vec<f64>> {
    let source = lattice.inverse_points_mm(&grid.coordinates(), 12, 1.0e-8);
    sample_trilinear_field(values, grid, &source)
}

#[must_use]
pub fn b64(raw: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(raw)
}
