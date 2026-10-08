// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use implexity_geometry::document::arrays;
use implexity_geometry::value::{ArrayData, DType, NdArray};
use serde_json::{Map, Value, json};

use crate::error::{AResult, AuthoringError};
use crate::interactive_runtime::CancellationToken;
use crate::py::{canonical_unicode, jf, py_str, sha256_hex};
use crate::sync::lock;

pub const SCHEMA: &str = "implexity-progressive-field/1";

pub(crate) fn verr(message: impl Into<String>) -> AuthoringError {
    AuthoringError::value("ValueError", message)
}

pub(crate) fn index_err(what: impl std::fmt::Display) -> AuthoringError {
    AuthoringError::value("IndexError", what.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reducer {
    Mean,
    Nearest,
    MaxAbsSigned,
}

impl Reducer {

    pub fn parse(name: &str) -> AResult<Self> {
        match name {
            "mean" => Ok(Self::Mean),
            "nearest" => Ok(Self::Nearest),
            "max_abs_signed" => Ok(Self::MaxAbsSigned),
            other => Err(verr(format!("Unsupported reducer: {other}"))),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Mean => "mean",
            Self::Nearest => "nearest",
            Self::MaxAbsSigned => "max_abs_signed",
        }
    }
}

#[must_use]
pub fn dtype_str(d: DType) -> String {
    d.npy_descr().to_string()
}

fn dtype_from_str(s: &str) -> AResult<DType> {
    [DType::F64, DType::F32, DType::I64, DType::I32, DType::I16, DType::I8, DType::U8, DType::Bool]
        .into_iter()
        .find(|d| d.npy_descr() == s)
        .ok_or_else(|| verr(format!("data type {s:?} not understood")))
}


pub fn spatial_shape(shape: &[usize]) -> AResult<[usize; 3]> {
    if shape.len() != 3 && shape.len() != 4 {
        return Err(verr("A visual field must have shape (nx, ny, nz) or (nx, ny, nz, components)"));
    }
    if shape[..3].contains(&0) {
        return Err(verr("Spatial field axes must be non-empty"));
    }
    Ok([shape[0], shape[1], shape[2]])
}


pub fn level_shapes(exact: [usize; 3], max_preview_voxels: i64) -> AResult<Vec<[usize; 3]>> {
    if max_preview_voxels < 8 {
        return Err(verr("max_preview_voxels must be at least 8"));
    }
    let budget = usize::try_from(max_preview_voxels).unwrap_or(usize::MAX);
    let mut descending = vec![exact];
    let mut current = exact;
    while current.iter().product::<usize>() > budget {
        current = current.map(|v| v.div_ceil(2).max(1));
        descending.push(current);
    }
    descending.reverse();
    Ok(descending)
}

fn tail_len(shape: &[usize]) -> usize {
    shape[3..].iter().product()
}


pub fn downsample_by_two(array: &NdArray, reducer: Reducer) -> AResult<NdArray> {
    let shape = array.shape().to_vec();
    let [nx, ny, nz] = spatial_shape(&shape)?;
    let tail = tail_len(&shape);
    let src = array.to_f64_vec();
    let (px, py, pz) = (nx + nx % 2, ny + ny % 2, nz + nz % 2);
    let (mx, my, mz) = (px / 2, py / 2, pz / 2);
    let at = |i: usize, j: usize, k: usize, t: usize| -> f64 {
        let (i, j, k) = (i.min(nx - 1), j.min(ny - 1), k.min(nz - 1));
        src[((i * ny + j) * nz + k) * tail + t]
    };
    let mut out: Vec<f32> = Vec::with_capacity(mx * my * mz * tail);
    for i in 0..mx {
        for j in 0..my {
            for k in 0..mz {
                for t in 0..tail {
                    let mut block = [0.0f64; 8];
                    for (n, slot) in block.iter_mut().enumerate() {
                        let (a, b, c) = (n / 4, (n / 2) % 2, n % 2);
                        *slot = at(2 * i + a, 2 * j + b, 2 * k + c, t);
                    }
                    #[allow(clippy::cast_possible_truncation)]
                    let v = match reducer {
                        Reducer::Nearest => block[0] as f32,
                        Reducer::MaxAbsSigned if tail == 1 && shape.len() == 3 => {
                            let mut best = 0;
                            for n in 1..8 {
                                if block[n].abs() > block[best].abs() {
                                    best = n;
                                }
                            }
                            block[best] as f32
                        }
                        _ => (block.iter().fold(0.0, |acc, x| acc + x) / 8.0) as f32,
                    };
                    out.push(v);
                }
            }
        }
    }
    let mut new_shape = vec![mx, my, mz];
    new_shape.extend_from_slice(&shape[3..]);
    NdArray::new(new_shape, ArrayData::F32(out)).ok_or_else(|| verr("downsample shape mismatch"))
}

#[must_use]
pub fn robust_range(values: &[f64], symmetric: bool) -> (f64, f64) {
    let mut finite: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite.is_empty() {
        return if symmetric { (-1.0, 1.0) } else { (0.0, 1.0) };
    }
    let limit = 500_000usize;
    if finite.len() > limit {
        let stride = (finite.len() / limit).max(1);
        finite = finite.iter().step_by(stride).take(limit).copied().collect();
    }
    let lo = implexity_mesh::numeric::percentile(&finite, 1.0);
    let hi = implexity_mesh::numeric::percentile(&finite, 99.0);
    if symmetric {
        let bound = lo.abs().max(hi.abs()).max(f64::EPSILON);
        return (-bound, bound);
    }
    if hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        let eps = (lo.abs() * 1e-6).max(1e-9);
        return (lo - eps, hi + eps);
    }
    (lo, hi)
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldIdentity {
    pub model_id: String,
    pub problem_id: String,
    pub field_name: String,
    pub registration_id: String,
    pub response_id: String,
    pub optimisation_iteration: Option<i64>,
}

impl FieldIdentity {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({"model_id": self.model_id, "problem_id": self.problem_id, "field_name": self.field_name,
            "registration_id": self.registration_id, "response_id": self.response_id,
            "optimisation_iteration": self.optimisation_iteration})
    }


    pub fn from_dict(v: &Value) -> AResult<Self> {
        let s = |k: &str| -> AResult<String> {
            v.get(k).map(py_str).ok_or_else(|| {
                AuthoringError::Type(format!("FieldIdentity.__init__() missing required argument: '{k}'"))
            })
        };
        Ok(Self {
            model_id: s("model_id")?,
            problem_id: s("problem_id")?,
            field_name: s("field_name")?,
            registration_id: s("registration_id")?,
            response_id: v.get("response_id").map(py_str).unwrap_or_default(),
            optimisation_iteration: v.get("optimisation_iteration").and_then(Value::as_i64),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TilePayload {
    pub header: Value,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct FieldRecord {
    pub field_id: String,
    pub identity: Value,
    pub registration: Value,
    pub exact_path: PathBuf,
    pub exact_shape: Vec<usize>,
    pub exact_dtype: String,
    pub exact_sha256: String,
    pub level_shapes: Vec<[usize; 3]>,
    pub reducer: Reducer,
    pub visual_range: (f64, f64),
    pub symmetric_range: bool,
    pub content_options: Value,
}

impl FieldRecord {
    #[must_use]
    pub fn exact_level(&self) -> usize {
        self.level_shapes.len() - 1
    }
}

struct Cache {
    entries: VecDeque<((String, usize), NdArray, usize)>,
    bytes: usize,
}

fn nbytes(a: &NdArray) -> usize {
    a.size() * a.dtype().itemsize()
}


pub fn atomic_write(path: &Path, data: &[u8]) -> AResult<()> {
    let io = |e: std::io::Error| AuthoringError::Io(format!("{e}: {}", path.display()));
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(io)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        implexity_io::atomic::unique_token()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        f.write_all(data).map_err(io)?;
        f.flush().map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    std::fs::rename(&tmp, path).map_err(io)
}


pub fn gzip_compress(raw: &[u8], level: u32) -> AResult<Vec<u8>> {
    Ok(crate::zlib_deflate::gzip(raw, level))
}


pub fn gzip_decompress(body: &[u8]) -> AResult<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body).read_to_end(&mut out).map_err(|e| verr(e.to_string()))?;
    Ok(out)
}

impl TilePayload {

    pub fn decode(&self) -> AResult<NdArray> {
        let encoding = self.header.get("encoding").map(py_str).unwrap_or_default();
        let raw = if encoding.starts_with("gzip") { gzip_decompress(&self.body)? } else { self.body.clone() };
        if sha256_hex(&raw) != py_str(&self.header["raw_sha256"]) {
            return Err(verr("tile checksum mismatch"));
        }
        let dtype = dtype_from_str(&py_str(&self.header["dtype"]))?;
        let shape: Vec<usize> = self.header["shape"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|v| usize::try_from(v).unwrap_or(0))
            .collect();
        NdArray::from_le_bytes(dtype, shape, &raw).ok_or_else(|| verr("tile shape mismatch"))
    }
}

#[derive(Clone, Debug)]
pub struct PublishOptions {
    pub reducer: Reducer,
    pub max_preview_voxels: i64,
    pub symmetric_range: bool,
    pub identity_extra: Value,
}

impl Default for PublishOptions {
    fn default() -> Self {
        Self {
            reducer: Reducer::Mean,
            max_preview_voxels: 64_000,
            symmetric_range: false,
            identity_extra: json!({}),
        }
    }
}

pub struct ProgressiveFieldStore {
    pub root: PathBuf,
    pub memory_budget_bytes: usize,
    registered_levels: bool,
    records: Mutex<BTreeMap<String, FieldRecord>>,
    cache: Mutex<Cache>,
    publish_lock: Mutex<()>,
}

fn record_to_manifest(record: &FieldRecord) -> Value {
    let levels: Vec<Value> = record
        .level_shapes
        .iter()
        .enumerate()
        .map(|(level, shape)| {
            json!({"level": level, "spatial_shape": shape, "exact": level == record.exact_level(),
                "voxel_count": shape.iter().product::<usize>()})
        })
        .collect();
    json!({
        "schema": SCHEMA, "field_id": record.field_id, "identity": record.identity,
        "registration": record.registration, "exact_shape": record.exact_shape,
        "exact_dtype": record.exact_dtype, "exact_sha256": record.exact_sha256,
        "level_shapes": record.level_shapes, "exact_level": record.exact_level(),
        "reducer": record.reducer.name(), "visual_range": [jf(record.visual_range.0), jf(record.visual_range.1)],
        "symmetric_range": record.symmetric_range, "content_options": record.content_options,
        "levels": levels,
    })
}

impl ProgressiveFieldStore {

    pub fn new(root: &Path, memory_budget_bytes: usize, registered_levels: bool) -> AResult<Self> {
        std::fs::create_dir_all(root).map_err(|e| AuthoringError::Io(format!("{e}: {}", root.display())))?;
        Ok(Self {
            root: root.to_path_buf(),
            memory_budget_bytes: memory_budget_bytes.max(8 * 1024 * 1024),
            registered_levels,
            records: Mutex::new(BTreeMap::new()),
            cache: Mutex::new(Cache { entries: VecDeque::new(), bytes: 0 }),
            publish_lock: Mutex::new(()),
        })
    }

    #[must_use]
    pub fn field_dir(&self, field_id: &str) -> PathBuf {
        self.root.join(field_id.get(..2).unwrap_or(field_id)).join(field_id)
    }


    pub fn publish(
        &self,
        array: &NdArray,
        identity: &FieldIdentity,
        registration: &Value,
        opts: &PublishOptions,
    ) -> AResult<String> {
        let spatial = spatial_shape(array.shape())?;
        let reg_shape: Vec<i64> = registration
            .get("shape")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(-1)).collect())
            .unwrap_or_default();
        #[allow(clippy::cast_possible_wrap)]
        if reg_shape != spatial.iter().map(|v| *v as i64).collect::<Vec<_>>() {
            return Err(verr("registration shape does not match field spatial shape"));
        }
        let raw = array.to_le_bytes();
        let exact_sha = sha256_hex(&raw);
        let shapes = level_shapes(spatial, opts.max_preview_voxels)?;
        let visual_range = robust_range(&array.to_f64_vec(), opts.symmetric_range);
        let content_options = json!({"max_preview_voxels": opts.max_preview_voxels,
            "symmetric_range": opts.symmetric_range, "identity_extra": opts.identity_extra});
        let metadata = json!({
            "schema": SCHEMA, "identity": identity.as_dict(), "registration": registration,
            "shape": array.shape(), "dtype": dtype_str(array.dtype()), "exact_sha256": exact_sha,
            "reducer": opts.reducer.name(), "level_shapes": shapes,
            "visual_range": [jf(visual_range.0), jf(visual_range.1)], "content_options": content_options,
        });
        let field_id = sha256_hex(canonical_unicode(&metadata).as_bytes());
        let dir = self.field_dir(&field_id);
        let exact_path = dir.join("level_exact.npy");
        let manifest_path = dir.join("manifest.json");
        let record = FieldRecord {
            field_id: field_id.clone(),
            identity: identity.as_dict(),
            registration: registration.clone(),
            exact_path: exact_path.clone(),
            exact_shape: array.shape().to_vec(),
            exact_dtype: dtype_str(array.dtype()),
            exact_sha256: exact_sha,
            level_shapes: shapes,
            reducer: opts.reducer,
            visual_range,
            symmetric_range: opts.symmetric_range,
            content_options,
        };
        let _publishing = lock(&self.publish_lock);
        if manifest_path.is_file() {
            lock(&self.records).remove(&field_id);
            let existing = self.record(&field_id)?;
            let same = existing.identity == record.identity
                && existing.registration == record.registration
                && existing.exact_shape == record.exact_shape
                && existing.exact_dtype == record.exact_dtype
                && existing.exact_sha256 == record.exact_sha256
                && existing.level_shapes == record.level_shapes
                && existing.reducer == record.reducer
                && existing.visual_range == record.visual_range
                && existing.symmetric_range == record.symmetric_range
                && existing.content_options == record.content_options;
            if !same {
                return Err(verr("content-addressed field manifest does not match its identity"));
            }
            if !exact_path.is_file() {
                return Err(verr("content-addressed field is missing its exact array"));
            }
            self.remember((field_id.clone(), existing.exact_level()), array.clone());
            return Ok(field_id);
        }
        atomic_write(&exact_path, &arrays::npy_bytes(array))?;
        let exact_level = record.exact_level();
        lock(&self.records).insert(field_id.clone(), record);
        self.remember((field_id.clone(), exact_level), array.clone());
        let manifest = self.manifest(&field_id)?;
        atomic_write(&manifest_path, canonical_unicode(&manifest).as_bytes())?;
        Ok(field_id)
    }


    pub fn record(&self, field_id: &str) -> AResult<FieldRecord> {
        if let Some(r) = lock(&self.records).get(field_id) {
            return Ok(r.clone());
        }
        let dir = self.field_dir(field_id);
        let path = dir.join("manifest.json");
        if !path.exists() || field_id.contains('/') || field_id.contains('\\') || field_id.contains("..") {
            return Err(AuthoringError::Key(crate::py::repr(&json!(field_id))));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| AuthoringError::Io(e.to_string()))?;
        let m = implexity_core::json::parse_strict(&text).map_err(|e| verr(e.to_string()))?;
        let shapes: Vec<[usize; 3]> = m["level_shapes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                let v: Vec<usize> = s
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .map(|x| usize::try_from(x).unwrap_or(0))
                    .collect();
                [
                    v.first().copied().unwrap_or(0),
                    v.get(1).copied().unwrap_or(0),
                    v.get(2).copied().unwrap_or(0),
                ]
            })
            .collect();
        if shapes.is_empty() {
            return Err(verr("field manifest has no levels"));
        }
        let record = FieldRecord {
            field_id: field_id.to_string(),
            identity: FieldIdentity::from_dict(&m["identity"])?.as_dict(),
            registration: m["registration"].clone(),
            exact_path: dir.join("level_exact.npy"),
            exact_shape: m["exact_shape"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .map(|x| usize::try_from(x).unwrap_or(0))
                .collect(),
            exact_dtype: py_str(&m["exact_dtype"]),
            exact_sha256: py_str(&m["exact_sha256"]),
            level_shapes: shapes,
            reducer: Reducer::parse(&py_str(&m["reducer"]))?,
            visual_range: (
                m["visual_range"][0].as_f64().unwrap_or(0.0),
                m["visual_range"][1].as_f64().unwrap_or(1.0),
            ),
            symmetric_range: crate::py::truthy(&m["symmetric_range"]),
            content_options: m.get("content_options").cloned().unwrap_or_else(|| json!({})),
        };
        lock(&self.records).insert(field_id.to_string(), record.clone());
        Ok(record)
    }


    pub fn manifest(&self, field_id: &str) -> AResult<Value> {
        Ok(record_to_manifest(&self.record(field_id)?))
    }

    fn remember(&self, key: (String, usize), array: NdArray) {
        let mut c = lock(&self.cache);
        if let Some(pos) = c.entries.iter().position(|(k, _, _)| *k == key)
            && let Some((_, _, n)) = c.entries.remove(pos)
        {
            c.bytes -= n;
        }
        let n = nbytes(&array);
        c.entries.push_back((key, array, n));
        c.bytes += n;
        while !c.entries.is_empty() && c.bytes > self.memory_budget_bytes {
            if let Some((_, _, n)) = c.entries.pop_front() {
                c.bytes -= n;
            }
        }
    }

    fn cached(&self, key: &(String, usize)) -> Option<NdArray> {
        let mut c = lock(&self.cache);
        let pos = c.entries.iter().position(|(k, _, _)| k == key)?;
        let entry = c.entries.remove(pos)?;
        let a = entry.1.clone();
        c.entries.push_back(entry);
        Some(a)
    }

    #[must_use]
    pub fn cached_bytes(&self) -> usize {
        lock(&self.cache).bytes
    }

    fn exact(&self, record: &FieldRecord, token: Option<&CancellationToken>) -> AResult<NdArray> {
        let key = (record.field_id.clone(), record.exact_level());
        if let Some(hit) = self.cached(&key) {
            return Ok(hit);
        }
        if token.is_some_and(CancellationToken::cancelled) {
            return Err(AuthoringError::runtime("RuntimeError", "operation cancelled"));
        }
        let blob = std::fs::read(&record.exact_path).map_err(|e| AuthoringError::Io(e.to_string()))?;
        let mut problems = Vec::new();
        let raw = arrays::npy_payload(&blob, "level_exact.npy", &mut problems)
            .ok_or_else(|| verr(problems.join("; ")))?;
        let dtype = dtype_from_str(&record.exact_dtype)?;
        let array = NdArray::from_le_bytes(dtype, record.exact_shape.clone(), &raw)
            .ok_or_else(|| verr("exact field checksum mismatch"))?;
        if sha256_hex(&array.to_le_bytes()) != record.exact_sha256 {
            return Err(verr("exact field checksum mismatch"));
        }
        self.remember(key, array.clone());
        Ok(array)
    }


    pub fn level(&self, field_id: &str, level: usize, token: Option<&CancellationToken>) -> AResult<NdArray> {
        let record = self.record(field_id)?;
        if level > record.exact_level() {
            return Err(index_err(level));
        }
        if level == record.exact_level() {
            return self.exact(&record, token);
        }
        let key = (field_id.to_string(), level);
        if let Some(hit) = self.cached(&key) {
            return Ok(hit);
        }
        if token.is_some_and(CancellationToken::cancelled) {
            return Err(AuthoringError::runtime("RuntimeError", "operation cancelled"));
        }
        let array = if self.registered_levels {
            let exact = self.exact(&record, token)?;
            let centering = record.registration.get("centering").map_or_else(|| "cell".to_string(), py_str);
            crate::incremental_fields::resample_registered(
                &exact,
                record.level_shapes[level],
                record.reducer,
                &centering,
            )?
        } else {
            let finer = self.level(field_id, level + 1, token)?;
            let down = downsample_by_two(&finer, record.reducer)?;
            crop(&down, record.level_shapes[level])?
        };
        if token.is_some_and(CancellationToken::cancelled) {
            return Err(AuthoringError::runtime("RuntimeError", "operation cancelled"));
        }
        self.remember(key, array.clone());
        Ok(array)
    }


    pub fn choose_level(&self, field_id: &str, voxel_budget: i64, exact: bool) -> AResult<usize> {
        let record = self.record(field_id)?;
        if exact {
            return Ok(record.exact_level());
        }
        let budget = usize::try_from(voxel_budget.max(1)).unwrap_or(1);
        Ok(record
            .level_shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| s.iter().product::<usize>() <= budget)
            .map(|(i, _)| i)
            .max()
            .unwrap_or(0))
    }


    pub fn tile(
        &self,
        field_id: &str,
        level: usize,
        tile_index: [i64; 3],
        tile_shape: [i64; 3],
        token: Option<&CancellationToken>,
    ) -> AResult<TilePayload> {
        let array = self.level(field_id, level, token)?;
        let shape = array.shape().to_vec();
        let starts: Vec<i64> = (0..3).map(|i| tile_index[i] * tile_shape[i]).collect();
        #[allow(clippy::cast_possible_wrap)]
        if starts.iter().enumerate().any(|(i, s)| *s < 0 || *s >= shape[i] as i64) {
            return Err(index_err(format!("({}, {}, {})", tile_index[0], tile_index[1], tile_index[2])));
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let (st, sp): (Vec<usize>, Vec<usize>) = (
            starts.iter().map(|s| *s as usize).collect(),
            (0..3).map(|i| (shape[i] as i64).min(starts[i] + tile_shape[i]) as usize).collect(),
        );
        let part = slice_box(&array, [st[0], st[1], st[2]], [sp[0], sp[1], sp[2]])?;
        let raw = part.to_le_bytes();
        let exact_level = self.record(field_id)?.exact_level();
        let body = gzip_compress(&raw, if level < exact_level { 1 } else { 6 })?;
        let header = json!({
            "schema": "implexity-field-tile/1", "field_id": field_id, "level": level,
            "exact": level == exact_level, "tile_index": tile_index, "offset": st,
            "shape": part.shape(), "dtype": dtype_str(part.dtype()), "encoding": "gzip+raw-c-order",
            "raw_sha256": sha256_hex(&raw), "body_sha256": sha256_hex(&body), "body_bytes": body.len(),
        });
        Ok(TilePayload { header, body })
    }


    pub fn iter_tiles(
        &self,
        field_id: &str,
        level: usize,
        tile_shape: [i64; 3],
        token: Option<&CancellationToken>,
    ) -> AResult<Vec<TilePayload>> {
        let array = self.level(field_id, level, token)?;
        #[allow(clippy::cast_possible_wrap)]
        let counts: Vec<i64> =
            (0..3).map(|i| (array.shape()[i] as i64 + tile_shape[i] - 1) / tile_shape[i]).collect();
        let mut out = Vec::new();
        for ix in 0..counts[0] {
            for iy in 0..counts[1] {
                for iz in 0..counts[2] {
                    if token.is_some_and(CancellationToken::cancelled) {
                        return Err(AuthoringError::runtime("RuntimeError", "operation cancelled"));
                    }
                    out.push(self.tile(field_id, level, [ix, iy, iz], tile_shape, token)?);
                }
            }
        }
        Ok(out)
    }
}


pub fn crop(array: &NdArray, target: [usize; 3]) -> AResult<NdArray> {
    slice_box(
        array,
        [0, 0, 0],
        [target[0].min(array.shape()[0]), target[1].min(array.shape()[1]), target[2].min(array.shape()[2])],
    )
}


pub fn slice_box(array: &NdArray, start: [usize; 3], stop: [usize; 3]) -> AResult<NdArray> {
    let shape = array.shape();
    spatial_shape(shape)?;
    let tail = tail_len(shape);
    let (ny, nz) = (shape[1], shape[2]);
    let mut idx: Vec<usize> = Vec::new();
    for i in start[0]..stop[0] {
        for j in start[1]..stop[1] {
            let base = ((i * ny + j) * nz + start[2]) * tail;
            let len = (stop[2] - start[2]) * tail;
            idx.extend(base..base + len);
        }
    }
    let data = gather(array.data(), &idx);
    let mut new_shape = vec![stop[0] - start[0], stop[1] - start[1], stop[2] - start[2]];
    new_shape.extend_from_slice(&shape[3..]);
    NdArray::new(new_shape, data).ok_or_else(|| verr("slice shape mismatch"))
}

#[must_use]
pub fn gather(data: &ArrayData, idx: &[usize]) -> ArrayData {
    match data {
        ArrayData::F64(v) => ArrayData::F64(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::F32(v) => ArrayData::F32(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::I64(v) => ArrayData::I64(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::I32(v) => ArrayData::I32(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::I16(v) => ArrayData::I16(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::I8(v) => ArrayData::I8(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::U8(v) => ArrayData::U8(idx.iter().map(|i| v[*i]).collect()),
        ArrayData::Bool(v) => ArrayData::Bool(idx.iter().map(|i| v[*i]).collect()),
    }
}

#[must_use]
pub fn object(v: &Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}
