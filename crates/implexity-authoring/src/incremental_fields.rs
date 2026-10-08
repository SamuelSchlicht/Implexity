// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;

use implexity_geometry::field_registration::GridRegistration;
use implexity_geometry::value::{ArrayData, NdArray};
use serde_json::{Value, json};

use crate::error::{AResult, AuthoringError};
use crate::interactive_runtime::CancellationToken;
use crate::progressive_fields::{
    FieldIdentity, ProgressiveFieldStore, PublishOptions, Reducer, TilePayload, atomic_write, dtype_str,
    gather, index_err, slice_box, verr,
};
use crate::py::{canonical_unicode, jf, py_str, sha256_hex};
use crate::sync::lock;

pub const REVISION_SCHEMA: &str = "implexity-progressive-field/2";
pub const DELTA_SCHEMA: &str = "implexity-field-delta/1";
pub const DEFAULT_TILE_SHAPE: [i64; 3] = [32, 32, 32];


pub fn positive_triplet(value: &[i64], name: &str) -> AResult<[i64; 3]> {
    if value.len() != 3 {
        return Err(verr(format!("{name} must contain three integers")));
    }
    if value.iter().any(|v| *v <= 0) {
        return Err(verr(format!("{name} values must be positive")));
    }
    Ok([value[0], value[1], value[2]])
}

#[allow(clippy::cast_possible_wrap)]
fn as_i64(shape: &[usize]) -> Vec<i64> {
    shape.iter().map(|v| *v as i64).collect()
}


pub fn tile_counts(shape: &[i64], tile_shape: &[i64]) -> AResult<[i64; 3]> {
    let s = positive_triplet(&shape[..3.min(shape.len())], "shape")?;
    let t = positive_triplet(tile_shape, "tile_shape")?;
    Ok([(s[0] + t[0] - 1) / t[0], (s[1] + t[1] - 1) / t[1], (s[2] + t[2] - 1) / t[2]])
}


#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn tile_slices(
    array_shape: &[usize],
    index: [i64; 3],
    tile_shape: &[i64],
) -> AResult<([usize; 3], [usize; 3], [usize; 3])> {
    let spatial = positive_triplet(&as_i64(&array_shape[..3.min(array_shape.len())]), "array_shape")?;
    if index.iter().any(|v| *v < 0) {
        return Err(verr("tile_index must contain three non-negative integers"));
    }
    let tile = positive_triplet(tile_shape, "tile_shape")?;
    let counts = tile_counts(&spatial, &tile)?;
    if (0..3).any(|i| index[i] >= counts[i]) {
        return Err(index_err(format!("({}, {}, {})", index[0], index[1], index[2])));
    }
    let starts: [i64; 3] = [index[0] * tile[0], index[1] * tile[1], index[2] * tile[2]];
    let stops: [i64; 3] = [
        spatial[0].min(starts[0] + tile[0]),
        spatial[1].min(starts[1] + tile[1]),
        spatial[2].min(starts[2] + tile[2]),
    ];
    let st = starts.map(|v| v as usize);
    let sp = stops.map(|v| v as usize);
    Ok((st, sp, [sp[0] - st[0], sp[1] - st[1], sp[2] - st[2]]))
}


#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn axis_cell_weights(n: usize, m: usize) -> AResult<Vec<Vec<f64>>> {
    if n < 1 || m < 1 {
        return Err(verr("source and target counts must be positive"));
    }
    let scale = n as f64 / m as f64;
    let mut out = vec![vec![0.0; n]; m];
    for (j, row) in out.iter_mut().enumerate() {
        let (lo, hi) = (j as f64 * scale, (j + 1) as f64 * scale);
        let first = lo.floor().max(0.0) as usize;
        let last = ((hi.ceil() as i64) - 1).min(n as i64 - 1);
        let mut i = first as i64;
        while i <= last {
            let fi = i as f64;
            let overlap = (hi.min(fi + 1.0) - lo.max(fi)).max(0.0);
            if overlap != 0.0 {
                row[i as usize] = overlap / scale;
            }
            i += 1;
        }
    }
    Ok(out)
}

fn f32_array(shape: Vec<usize>, data: &[f64]) -> AResult<NdArray> {
    #[allow(clippy::cast_possible_truncation)]
    let v: Vec<f32> = data.iter().map(|x| *x as f32).collect();
    NdArray::new(shape, ArrayData::F32(v)).ok_or_else(|| verr("resample shape mismatch"))
}

fn along_axis(
    data: &[f64],
    shape: &[usize; 4],
    axis: usize,
    m: usize,
    f: &dyn Fn(&[f64]) -> Vec<f64>,
) -> (Vec<f64>, [usize; 4]) {
    let mut out_shape = *shape;
    out_shape[axis] = m;
    let stride = |s: &[usize; 4], a: usize| s[a + 1..].iter().product::<usize>();
    let (in_stride, out_stride) = (stride(shape, axis), stride(&out_shape, axis));
    let outer: usize = shape[..axis].iter().product();
    let mut out = vec![0.0; out_shape.iter().product()];
    let n = shape[axis];
    for o in 0..outer {
        for inner in 0..in_stride {
            let line: Vec<f64> = (0..n).map(|i| data[(o * n + i) * in_stride + inner]).collect();
            let res = f(&line);
            for (j, v) in res.into_iter().enumerate() {
                out[(o * m + j) * out_stride + inner] = v;
            }
        }
    }
    (out, out_shape)
}


#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
pub fn resample_registered(
    source: &NdArray,
    target: [usize; 3],
    reducer: Reducer,
    centering: &str,
) -> AResult<NdArray> {
    let shape = source.shape().to_vec();
    let src_spatial = [shape[0], shape[1], shape[2]];
    positive_triplet(&as_i64(&target), "target_shape")?;
    if src_spatial == target {
        return Ok(source.clone());
    }
    let tail: usize = shape[3..].iter().product();
    let tail_shape: Vec<usize> = shape[3..].to_vec();
    let s4 = [shape[0], shape[1], shape[2], tail];
    let out_shape = |t: [usize; 3]| {
        let mut v = t.to_vec();
        v.extend_from_slice(&tail_shape);
        v
    };
    if centering == "node" {
        let mut data = source.to_f64_vec();
        let mut cur = s4;
        for axis in 0..3 {
            let (n, m) = (src_spatial[axis], target[axis]);
            let coords: Vec<f64> = if m > 1 {
                implexity_mesh::numeric::linspace(0.0, (n.max(1) - 1) as f64, m)
            } else {
                vec![(n as f64 - 1.0) * 0.5]
            };
            let f = |line: &[f64]| -> Vec<f64> {
                coords
                    .iter()
                    .map(|c| {
                        let lower = c.floor() as i64;
                        let lower_u = lower.max(0) as usize;
                        let upper = (lower_u + 1).min(n - 1);
                        let frac = c - lower as f64;
                        line[lower_u] * (1.0 - frac) + line[upper] * frac
                    })
                    .collect()
            };
            let (d, s) = along_axis(&data, &cur, axis, m, &f);
            data = d;
            cur = s;
        }
        return f32_array(out_shape(target), &data);
    }
    if reducer == Reducer::Nearest {
        let indices: Vec<Vec<usize>> = (0..3)
            .map(|a| {
                let (n, m) = (src_spatial[a], target[a]);
                (0..m)
                    .map(|i| ((((i as f64) + 0.5) * n as f64 / m as f64).floor() as usize).min(n - 1))
                    .collect()
            })
            .collect();
        let mut idx = Vec::with_capacity(target.iter().product::<usize>() * tail);
        for i in &indices[0] {
            for j in &indices[1] {
                for k in &indices[2] {
                    let base = ((i * shape[1] + j) * shape[2] + k) * tail;
                    idx.extend(base..base + tail);
                }
            }
        }
        return NdArray::new(out_shape(target), gather(source.data(), &idx))
            .ok_or_else(|| verr("resample shape mismatch"));
    }
    if reducer == Reducer::MaxAbsSigned && shape.len() == 3 {

        let mut data: Vec<f64> = source.to_f64_vec().iter().map(|x| f64::from(*x as f32)).collect();
        let mut cur = s4;
        for axis in 0..3 {
            let (n, m) = (src_spatial[axis], target[axis]);
            let bounds = implexity_mesh::numeric::linspace(0.0, n as f64, m + 1);
            let f = |line: &[f64]| -> Vec<f64> {
                (0..m)
                    .map(|index| {
                        let start = bounds[index].floor().max(0.0) as usize;
                        let stop = (bounds[index + 1].ceil() as usize).min(n).max(start + 1);
                        let mut best = start;
                        for i in start..stop {
                            if (line[i] as f32).abs() > (line[best] as f32).abs() {
                                best = i;
                            }
                        }
                        line[best]
                    })
                    .collect()
            };
            let (d, s) = along_axis(&data, &cur, axis, m, &f);
            data = d;
            cur = s;
        }
        return f32_array(out_shape(target), &data);
    }
    let mut data = source.to_f64_vec();
    let mut cur = s4;
    for axis in 0..3 {
        let (n, m) = (src_spatial[axis], target[axis]);
        let w = axis_cell_weights(n, m)?;
        let f = |line: &[f64]| -> Vec<f64> {
            w.iter().map(|row| row.iter().zip(line).fold(0.0, |acc, (a, b)| acc + a * b)).collect()
        };
        let (d, s) = along_axis(&data, &cur, axis, m, &f);
        data = d;
        cur = s;
    }
    f32_array(out_shape(target), &data)
}


#[allow(clippy::cast_precision_loss)]
pub fn registration_for_shape(registration: &Value, target: [usize; 3]) -> AResult<Value> {
    let exact = GridRegistration::from_wire(registration)?;
    let mut origin = exact.origin;
    let mut factors = [1.0; 3];
    for axis in 0..3 {
        let (n, m) = (exact.shape[axis], target[axis]);
        if exact.centering == "cell" {
            factors[axis] = n as f64 / m as f64;
        } else if m > 1 {
            factors[axis] = (n as f64 - 1.0) / (m as f64 - 1.0);
        } else {
            for c in 0..3 {
                origin[c] += exact.basis[axis][c] * ((n as f64 - 1.0) * 0.5);
            }
            factors[axis] = 1.0;
        }
    }
    let basis = [0, 1, 2].map(|a| exact.basis[a].map(|c| c * factors[a]));
    Ok(GridRegistration::new(target, origin, basis, &exact.centering, &exact.axis_order, &exact.frame)?
        .to_wire())
}

pub struct IncrementalProgressiveFieldStore {
    pub base: ProgressiveFieldStore,
    digests: Mutex<(VecDeque<(String, String)>, u64, u64)>,
    digest_entries: usize,
}

fn digest_key(field_id: &str, level: usize, index: [i64; 3], tile: [i64; 3]) -> String {
    format!("{field_id}|{level}|{index:?}|{tile:?}")
}

impl IncrementalProgressiveFieldStore {

    pub fn new(
        root: &std::path::Path,
        memory_budget_bytes: usize,
        digest_cache_entries: usize,
    ) -> AResult<Self> {
        Ok(Self {
            base: ProgressiveFieldStore::new(root, memory_budget_bytes, true)?,
            digests: Mutex::new((VecDeque::new(), 0, 0)),
            digest_entries: digest_cache_entries.max(128),
        })
    }


    pub fn level(&self, field_id: &str, level: usize, token: Option<&CancellationToken>) -> AResult<NdArray> {
        self.base.level(field_id, level, token)
    }


    pub fn level_registration(&self, field_id: &str, level: usize) -> AResult<Value> {
        let record = self.base.record(field_id)?;
        if level > record.exact_level() {
            return Err(index_err(level));
        }
        registration_for_shape(&record.registration, record.level_shapes[level])
    }

    fn revision_path(&self, field_id: &str) -> PathBuf {
        self.base.field_dir(field_id).join("revision_v22.json")
    }

    fn tile_of(value: &Value) -> AResult<[i64; 3]> {
        match value.get("tile_shape") {
            None => Ok(DEFAULT_TILE_SHAPE),
            Some(v) => {
                let t: Vec<i64> =
                    v.as_array().into_iter().flatten().map(|x| x.as_i64().unwrap_or(0)).collect();
                positive_triplet(&t, "tile_shape")
            }
        }
    }

    fn write_revision(&self, field_id: &str, value: &Value) -> AResult<()> {
        let path = self.revision_path(field_id);
        let immutable = json!({"schema": REVISION_SCHEMA, "field_id": field_id,
            "tile_shape": Self::tile_of(value)?, "identity": value.get("identity").cloned().unwrap_or_else(|| json!({}))});
        let data = canonical_unicode(&immutable);
        if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|e| AuthoringError::Io(e.to_string()))?;
            let existing = implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))?;
            let existing_immutable = json!({"schema": existing.get("schema").cloned().unwrap_or(Value::Null),
                "field_id": existing.get("field_id").cloned().unwrap_or(Value::Null),
                "tile_shape": Self::tile_of(&existing)?,
                "identity": existing.get("identity").cloned().unwrap_or_else(|| json!({}))});
            if canonical_unicode(&existing_immutable) != data {
                return Err(verr("content-addressed field revision metadata is immutable"));
            }
            if text.as_bytes() != data.as_bytes() {
                atomic_write(&path, data.as_bytes())?;
            }
            return Ok(());
        }
        atomic_write(&path, data.as_bytes())
    }

    fn read_revision(&self, field_id: &str) -> AResult<Value> {
        let path = self.revision_path(field_id);
        if !path.is_file() {
            return Ok(
                json!({"schema": REVISION_SCHEMA, "field_id": field_id, "tile_shape": DEFAULT_TILE_SHAPE, "identity": {}}),
            );
        }
        let text = std::fs::read_to_string(&path).map_err(|e| AuthoringError::Io(e.to_string()))?;
        implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))
    }


    pub fn publish_revision(
        &self,
        array: &NdArray,
        identity: &FieldIdentity,
        registration: &Value,
        parent_field_id: Option<&str>,
        reducer: Reducer,
        max_preview_voxels: i64,
        symmetric_range: bool,
        tile_shape: [i64; 3],
    ) -> AResult<Value> {
        let tile = positive_triplet(&tile_shape, "tile_shape")?;
        let opts = PublishOptions {
            reducer,
            max_preview_voxels,
            symmetric_range,
            identity_extra: json!({"schema": REVISION_SCHEMA, "tile_shape": tile}),
        };
        let field_id = self.base.publish(array, identity, registration, &opts)?;
        let parent = parent_field_id.filter(|p| *p != field_id);
        let revision = json!({"schema": REVISION_SCHEMA, "field_id": field_id, "tile_shape": tile, "identity": identity.as_dict()});
        self.write_revision(&field_id, &revision)?;
        let exact_level = self.base.record(&field_id)?.exact_level();
        #[allow(clippy::cast_possible_wrap)]
        let delta = self.delta_manifest(&field_id, parent, Some(exact_level as i64), Some(tile), None)?;
        let exact_delta = json!({"changed_count": delta["changed_count"].clone(), "unchanged_count": delta["unchanged_count"].clone(),
            "changed_fraction": delta["changed_fraction"].clone(), "compatible": delta["compatible"].clone()});
        let mut manifest = self.manifest_v22(&field_id)?;
        if let Some(m) = manifest.as_object_mut() {
            m.insert("parent_field_id".into(), json!(parent));
            m.insert("exact_delta".into(), exact_delta);
        }
        Ok(json!({"field_id": field_id, "manifest": manifest, "delta": delta}))
    }


    pub fn manifest_v22(&self, field_id: &str) -> AResult<Value> {
        let mut base = self.base.manifest(field_id)?;
        let revision = self.read_revision(field_id)?;
        let tile = Self::tile_of(&revision)?;
        let b = base.as_object_mut().ok_or_else(|| verr("manifest is not an object"))?;
        b.insert("schema".into(), json!(REVISION_SCHEMA));
        b.insert("tile_shape".into(), json!(tile));
        b.insert("tile_encoding".into(), json!("raw-numpy-or-gzip-raw-c-order"));
        let mut levels = b.get("levels").and_then(Value::as_array).cloned().unwrap_or_default();
        for level in &mut levels {
            let shape: Vec<i64> = level["spatial_shape"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| v.as_i64().unwrap_or(0))
                .collect();
            let counts = tile_counts(&shape, &tile)?;
            let idx = usize::try_from(level["level"].as_u64().unwrap_or(0)).unwrap_or(0);
            let reg = self.level_registration(field_id, idx)?;
            if let Some(l) = level.as_object_mut() {
                l.insert("tile_counts".into(), json!(counts));
                l.insert("tile_count".into(), json!(counts.iter().product::<i64>()));
                l.insert("registration".into(), reg);
            }
        }
        b.insert("levels".into(), Value::Array(levels));
        b.insert(
            "endpoints".into(),
            json!({"manifest": format!("/v1/implicit/field-stream/{field_id}/manifest"),
                "delta": format!("/v1/implicit/field-stream/{field_id}/delta"),
                "tile": format!("/v1/implicit/field-stream/{field_id}/tile")}),
        );
        Ok(base)
    }

    fn digest_cached(&self, key: &str) -> Option<String> {
        let mut d = lock(&self.digests);
        if let Some(pos) = d.0.iter().position(|(k, _)| k == key) {
            let entry = d.0.remove(pos)?;
            let v = entry.1.clone();
            d.0.push_back(entry);
            d.1 += 1;
            return Some(v);
        }
        d.2 += 1;
        None
    }

    fn digest_remember(&self, key: String, digest: String) -> String {
        let mut d = lock(&self.digests);
        if let Some(pos) = d.0.iter().position(|(k, _)| *k == key) {
            d.0.remove(pos);
        }
        d.0.push_back((key, digest.clone()));
        while d.0.len() > self.digest_entries {
            d.0.pop_front();
        }
        digest
    }

    fn digest_for_part(&self, key: String, part: &NdArray) -> String {
        if let Some(hit) = self.digest_cached(&key) {
            return hit;
        }
        self.digest_remember(key, sha256_hex(&part.to_le_bytes()))
    }


    pub fn tile_digest(
        &self,
        field_id: &str,
        level: usize,
        index: [i64; 3],
        tile_shape: [i64; 3],
        token: Option<&CancellationToken>,
    ) -> AResult<String> {
        let tile = positive_triplet(&tile_shape, "tile_shape")?;
        let key = digest_key(field_id, level, index, tile);
        if let Some(hit) = self.digest_cached(&key) {
            return Ok(hit);
        }
        let array = self.level(field_id, level, token)?;
        let (st, sp, _) = tile_slices(array.shape(), index, &tile)?;
        let part = slice_box(&array, st, sp)?;
        Ok(self.digest_remember(key, sha256_hex(&part.to_le_bytes())))
    }

    #[must_use]
    pub fn digest_cache_stats(&self) -> Value {
        let d = lock(&self.digests);
        json!({"entries": d.0.len(), "hits": d.1, "misses": d.2})
    }

    fn compatible(current: &Value, previous: &Value) -> (bool, Option<String>) {
        let ci = current.get("identity").cloned().unwrap_or_else(|| json!({}));
        let pi = previous.get("identity").cloned().unwrap_or_else(|| json!({}));
        let g = |v: &Value, a: &str, b: Option<&str>| -> Value {
            let x = v.get(a).cloned().unwrap_or(Value::Null);
            match b {
                Some(k) => x.get(k).cloned().unwrap_or(Value::Null),
                None => x,
            }
        };
        let checks = [
            (
                g(current, "registration", Some("registration_id")),
                g(previous, "registration", Some("registration_id")),
                "registration",
            ),
            (g(current, "exact_shape", None), g(previous, "exact_shape", None), "shape"),
            (g(current, "exact_dtype", None), g(previous, "exact_dtype", None), "dtype"),
            (g(current, "reducer", None), g(previous, "reducer", None), "reducer"),
            (g(&ci, "field_name", None), g(&pi, "field_name", None), "field name"),
            (g(&ci, "response_id", None), g(&pi, "response_id", None), "response"),
        ];
        for (l, r, name) in checks {
            if l != r {
                return (false, Some(format!("{name} differs")));
            }
        }
        (true, None)
    }


    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    pub fn delta_manifest(
        &self,
        field_id: &str,
        from_field_id: Option<&str>,
        level: Option<i64>,
        tile_shape: Option<[i64; 3]>,
        token: Option<&CancellationToken>,
    ) -> AResult<Value> {
        let current = self.manifest_v22(field_id)?;
        let exact_level = current["exact_level"].as_i64().unwrap_or(0);
        let level = level.unwrap_or(exact_level);
        if level < 0 || level > exact_level {
            return Err(index_err(level));
        }
        let lvl = usize::try_from(level).unwrap_or(0);
        let tile = match tile_shape {
            Some(t) => positive_triplet(&t, "tile_shape")?,
            None => Self::tile_of(&current)?,
        };
        let shape: Vec<i64> = current["levels"][lvl]["spatial_shape"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|v| v.as_i64().unwrap_or(0))
            .collect();
        let counts = tile_counts(&shape, &tile)?;
        let mut compatible = false;
        let mut reason: Option<String> = None;
        match from_field_id {
            Some(from) if !from.is_empty() => match self.manifest_v22(from) {
                Ok(previous) => {
                    let (c, r) = Self::compatible(&current, &previous);
                    compatible = c;
                    reason = r;
                    if compatible && level > previous["exact_level"].as_i64().unwrap_or(-1) {
                        compatible = false;
                        reason = Some("previous field has no corresponding level".into());
                    }
                    if compatible {
                        let prev_shape: Vec<i64> = previous["levels"][lvl]["spatial_shape"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|v| v.as_i64().unwrap_or(0))
                            .collect();
                        if prev_shape != shape {
                            compatible = false;
                            reason = Some("level shape differs".into());
                        }
                    }
                }
                Err(AuthoringError::Key(_)) => reason = Some("previous field is unknown".into()),
                Err(e) => return Err(e),
            },
            Some(_) => {}
            None => reason = Some("no previous field supplied".into()),
        }
        let current_array = self.level(field_id, lvl, token)?;
        let previous_array = match from_field_id {
            Some(from) if compatible && !from.is_empty() => Some(self.level(from, lvl, token)?),
            _ => None,
        };
        let mut changed = Vec::new();
        let mut unchanged = 0usize;
        for ix in 0..counts[0] {
            for iy in 0..counts[1] {
                for iz in 0..counts[2] {
                    if token.is_some_and(CancellationToken::cancelled) {
                        return Err(AuthoringError::runtime("RuntimeError", "operation cancelled"));
                    }
                    let index = [ix, iy, iz];
                    let (st, sp, part_shape) = tile_slices(current_array.shape(), index, &tile)?;
                    let part = slice_box(&current_array, st, sp)?;
                    let current_digest = self.digest_for_part(digest_key(field_id, lvl, index, tile), &part);
                    let previous_digest = match (&previous_array, from_field_id) {
                        (Some(prev), Some(from)) => {
                            let pp = slice_box(prev, st, sp)?;
                            Some(self.digest_for_part(digest_key(from, lvl, index, tile), &pp))
                        }
                        _ => None,
                    };
                    if previous_digest.as_deref() == Some(current_digest.as_str()) {
                        unchanged += 1;
                        continue;
                    }
                    changed.push(json!({"index": index, "offset": st, "shape": part_shape,
                        "value_shape": part.shape(), "raw_bytes": part.size() * part.dtype().itemsize(),
                        "raw_sha256": current_digest, "previous_raw_sha256": previous_digest}));
                }
            }
        }
        let total: i64 = counts.iter().product();
        let changed_count = changed.len();
        Ok(json!({
            "schema": DELTA_SCHEMA, "field_id": field_id, "from_field_id": from_field_id,
            "level": level, "exact": level == exact_level,
            "registration_id": current.get("registration").and_then(|r| r.get("registration_id")).cloned().unwrap_or(Value::Null),
            "tile_shape": tile, "tile_counts": counts, "compatible": compatible,
            "incompatibility_reason": if compatible { None } else { reason },
            "changed": changed, "changed_count": changed_count, "unchanged_count": unchanged,
            "total_tiles": total,
            "changed_fraction": if total != 0 { jf(changed_count as f64 / total as f64) } else { jf(0.0) },
            "lossless": true,
        }))
    }


    pub fn tile_raw(
        &self,
        field_id: &str,
        level: usize,
        index: [i64; 3],
        tile_shape: [i64; 3],
        token: Option<&CancellationToken>,
    ) -> AResult<TilePayload> {
        let array = self.level(field_id, level, token)?;
        let tile = positive_triplet(&tile_shape, "tile_shape")?;
        let (st, sp, part_shape) = tile_slices(array.shape(), index, &tile)?;
        let part = slice_box(&array, st, sp)?;
        let raw = part.to_le_bytes();
        let digest = sha256_hex(&raw);
        let exact = level == self.base.record(field_id)?.exact_level();
        let header = json!({
            "schema": "implexity-field-tile/2", "field_id": field_id, "level": level, "exact": exact,
            "tile_index": index, "offset": st, "spatial_shape": part_shape, "shape": part.shape(),
            "dtype": dtype_str(part.dtype()), "encoding": "raw-c-order", "raw_sha256": digest,
            "body_sha256": digest, "body_bytes": raw.len(),
            "registration": self.level_registration(field_id, level)?,
        });
        Ok(TilePayload { header, body: raw })
    }


    pub fn tile(
        &self,
        field_id: &str,
        level: usize,
        index: [i64; 3],
        tile_shape: [i64; 3],
        token: Option<&CancellationToken>,
    ) -> AResult<TilePayload> {
        self.base.tile(field_id, level, index, tile_shape, token)
    }
}

#[must_use]
pub fn field_id_of(v: &Value) -> String {
    v.get("field_id").map(py_str).unwrap_or_default()
}
