// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::manifest::{FrameManifest, Location, Precision};
use super::{DynamicError, DynamicResult, STATUS_SCHEMA};

const MAGIC: &[u8; 4] = b"IXFR";
const VERSION: u16 = 1;
const ENC_SHUFFLE_DEFLATE: u16 = 1;
pub const HEADER_BYTES: usize = 100;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const STATUS_FILE: &str = "status.json";

fn segment_path(dir: &Path, ordinal: u64, ext: &str) -> PathBuf {
    dir.join(format!("seg_{ordinal:06}.{ext}"))
}

fn mesh_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("mesh_{index:02}.mesh"))
}

fn shuffle(raw: &[u8], width: usize) -> Vec<u8> {
    let n = raw.len() / width;
    let mut out = vec![0u8; raw.len()];
    for (i, chunk) in raw.chunks_exact(width).enumerate() {
        for (b, byte) in chunk.iter().enumerate() {
            out[b * n + i] = *byte;
        }
    }
    out
}

fn unshuffle(planes: &[u8], width: usize) -> Vec<u8> {
    let n = planes.len() / width;
    let mut out = vec![0u8; planes.len()];
    for i in 0..n {
        for b in 0..width {
            out[i * width + b] = planes[b * n + i];
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    Stored(u64),
    Skipped(&'static str),
}

#[derive(Clone, Debug)]
struct SegmentInfo {
    ordinal: u64,
    key: i64,
    bytes: u64,
    frames: u64,
    rows: u64,
    t_first: f64,
    t_last: f64,
}

impl SegmentInfo {
    fn to_value(&self) -> Value {
        json!({"ordinal": self.ordinal, "key": self.key, "bytes": self.bytes, "frames": self.frames,
               "series_rows": self.rows, "t_first": finite_or_null(self.t_first),
               "t_last": finite_or_null(self.t_last)})
    }
}

fn finite_or_null(x: f64) -> Value {
    if x.is_finite() { json!(x) } else { Value::Null }
}

#[derive(Clone, Copy, Debug)]
struct Range {
    min: f64,
    max: f64,
    magnitude_max: f64,
    nonfinite: u64,
}

impl Range {
    const EMPTY: Self = Self { min: f64::INFINITY, max: f64::NEG_INFINITY, magnitude_max: 0.0, nonfinite: 0 };

    fn to_value(self) -> Value {
        json!({"min": finite_or_null(self.min), "max": finite_or_null(self.max),
               "magnitude_max": self.magnitude_max, "nonfinite_values": self.nonfinite})
    }
}

struct OpenSegment {
    info: SegmentInfo,
    frames: BufWriter<File>,
    series: Option<BufWriter<File>>,
    bins: Vec<bool>,
}

pub struct FrameWriter {
    dir: PathBuf,
    manifest: FrameManifest,
    period: Option<f64>,
    phase_origin: f64,
    closed: Vec<SegmentInfo>,
    current: Option<OpenSegment>,
    next_ordinal: u64,
    next_seq: u64,
    evicted: u64,
    skipped: Map<String, Value>,
    dropped_rows: u64,
    ranges: Vec<Range>,
    last_t: f64,
    t_first: f64,
    finished: bool,
    meshes_written: Vec<bool>,
    static_bytes: u64,
    attribute_ranges: Map<String, Value>,
}

impl std::fmt::Debug for FrameWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameWriter")
            .field("dir", &self.dir)
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreSummary {
    pub dir: PathBuf,
    pub status: Value,
}

impl FrameWriter {


    pub fn create(dir: &Path, manifest: FrameManifest) -> DynamicResult<Self> {
        manifest.validate()?;
        match std::fs::read_dir(dir) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(DynamicError::invalid(format!(
                        "dynamic frame store directory {} is not empty",
                        dir.display()
                    )));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                implexity_io::fsguard::create_dir_all_owner_only(dir)
                    .map_err(|e| DynamicError::io("creation", dir, &e))?;
            }
            Err(e) => return Err(DynamicError::io("listing", dir, &e)),
        }
        let text = serde_json::to_vec_pretty(&manifest.to_value())
            .map_err(|e| DynamicError::invalid(e.to_string()))?;
        let path = dir.join(MANIFEST_FILE);
        implexity_io::atomic::write_atomic(&path, &text)
            .map_err(|e| DynamicError::io("manifest write", &path, &e))?;
        let fields = manifest.fields.len();
        let meshes = manifest.meshes.len();
        let w = Self {
            dir: dir.to_path_buf(),
            period: manifest.time.period,
            phase_origin: manifest.time.phase_origin,
            manifest,
            closed: Vec::new(),
            current: None,
            next_ordinal: 0,
            next_seq: 0,
            evicted: 0,
            skipped: Map::new(),
            dropped_rows: 0,
            ranges: vec![Range::EMPTY; fields],
            last_t: f64::NEG_INFINITY,
            t_first: f64::NAN,
            finished: false,
            meshes_written: vec![false; meshes],
            static_bytes: 0,
            attribute_ranges: Map::new(),
        };
        w.write_status("writing")?;
        Ok(w)
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub const fn manifest(&self) -> &FrameManifest {
        &self.manifest
    }

    #[must_use]
    pub const fn period(&self) -> Option<f64> {
        self.period
    }

    #[must_use]
    pub fn phase_of(&self, t: f64) -> Option<(i64, f64)> {
        self.period.map(|p| {
            let x = (t - self.phase_origin) / p;
            let cycle = x.floor();
            #[allow(clippy::cast_possible_truncation)]
            (cycle as i64, (x - cycle).clamp(0.0, 1.0 - f64::EPSILON))
        })
    }

    fn segment_key(&self, t: f64) -> i64 {
        if let Some((cycle, _)) = self.phase_of(t) {
            return cycle;
        }
        match self.manifest.retention.segment_length {
            #[allow(clippy::cast_possible_truncation)]
            Some(len) => ((t - self.phase_origin) / len).floor() as i64,
            None => 0,
        }
    }

    fn bin_of(&self, t: f64) -> Option<usize> {
        let bins = self.manifest.retention.phase_bins;
        if bins == 0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.phase_of(t).map(|(_, phase)| ((phase * bins as f64) as usize).min(bins - 1))
    }

    #[must_use]
    pub fn wants_frame(&self, t: f64) -> bool {
        if !t.is_finite() || t < self.last_t {
            return false;
        }
        let Some(bin) = self.bin_of(t) else { return true };
        match &self.current {
            Some(seg) if seg.info.key == self.segment_key(t) => !seg.bins[bin],
            _ => true,
        }
    }



    pub fn set_period(&mut self, period: f64, origin: f64) -> DynamicResult<()> {
        if !(period.is_finite() && period > 0.0 && origin.is_finite()) {
            return Err(DynamicError::invalid("the period must be positive and its origin finite"));
        }
        self.close_current()?;
        self.period = Some(period);
        self.phase_origin = origin;
        self.write_status("writing")
    }

    fn total_bytes(&self) -> u64 {
        self.static_bytes
            + self.closed.iter().map(|s| s.bytes).sum::<u64>()
            + self.current.as_ref().map_or(0, |c| c.info.bytes)
    }



    pub fn write_mesh(
        &mut self,
        name: &str,
        points: &[f64],
        cells: &[usize],
        attributes: &[(&str, &[f64])],
    ) -> DynamicResult<()> {
        let Some(index) = self.manifest.meshes.iter().position(|m| m.name == name) else {
            return Err(DynamicError::invalid(format!("the store declares no mesh {name:?}")));
        };
        if self.meshes_written[index] {
            return Err(DynamicError::invalid(format!("mesh {name} is already written")));
        }
        if self.next_seq > 0 {
            return Err(DynamicError::invalid("meshes are written before the first frame"));
        }
        let m = &self.manifest.meshes[index];
        let (dims, k) = (m.cell.dims(), m.cell.nodes_per_cell());
        if points.len() != m.nodes * dims || points.iter().any(|x| !x.is_finite()) {
            return Err(DynamicError::invalid(format!(
                "mesh {name} needs {} finite coordinates ({dims} per node)",
                m.nodes * dims
            )));
        }
        if cells.len() != m.cells * k || cells.iter().any(|&n| n >= m.nodes) {
            return Err(DynamicError::invalid(format!(
                "mesh {name} needs {} node indices below {} ({k} per cell)",
                m.cells * k,
                m.nodes
            )));
        }
        if attributes.len() != m.attributes.len() {
            return Err(DynamicError::invalid(format!(
                "mesh {name} carries all {} declared attributes, not {}",
                m.attributes.len(),
                attributes.len()
            )));
        }
        let precision = self.manifest.retention.precision;
        #[allow(clippy::cast_precision_loss)]
        let cell_values: Vec<f64> = cells.iter().map(|&n| n as f64).collect();
        let mut records = vec![
            encode_record(0, 0, 0.0, None, None, points, Precision::F64)?,
            encode_record(1, 0, 0.0, None, None, &cell_values, Precision::F64)?,
        ];
        let mut ranges = Vec::new();
        for (j, spec) in m.attributes.iter().enumerate() {
            let Some((_, values)) = attributes.iter().find(|(n, _)| *n == spec.name) else {
                return Err(DynamicError::invalid(format!("mesh {name} lacks attribute {}", spec.name)));
            };
            let sites = if spec.grid == m.name { m.nodes } else { m.cells };
            if values.len() != sites {
                return Err(DynamicError::invalid(format!(
                    "attribute {} needs {sites} values, not {}",
                    spec.name,
                    values.len()
                )));
            }
            records.push(encode_record(2 + j, 0, 0.0, None, None, values, precision)?);
            let mut r = Range::EMPTY;
            for &v in *values {
                if v.is_finite() {
                    r.min = r.min.min(v);
                    r.max = r.max.max(v);
                    r.magnitude_max = r.magnitude_max.max(v.abs());
                } else {
                    r.nonfinite += 1;
                }
            }
            ranges.push((spec.name.clone(), r.to_value()));
        }
        let bytes: u64 = records.iter().map(|r| r.len() as u64).sum();
        if self.total_bytes() + bytes > self.manifest.retention.byte_limit {
            return Err(DynamicError::Bound(format!(
                "mesh {name} ({bytes} bytes) does not fit the store byte limit"
            )));
        }
        let path = mesh_path(&self.dir, index);
        let file = implexity_io::fsguard::create_new_nofollow(&path, 0o600)
            .map_err(|e| DynamicError::io("mesh creation", &path, &e))?;
        let mut w = BufWriter::new(file);
        for r in &records {
            w.write_all(r).map_err(|e| DynamicError::io("mesh write", &path, &e))?;
        }
        w.flush().map_err(|e| DynamicError::io("mesh write", &path, &e))?;
        self.static_bytes += bytes;
        self.meshes_written[index] = true;
        self.attribute_ranges.extend(ranges);
        self.write_status("writing")
    }

    fn close_current(&mut self) -> DynamicResult<()> {
        if let Some(mut seg) = self.current.take() {
            seg.frames.flush().map_err(|e| DynamicError::io("flush", &self.dir, &e))?;
            if let Some(s) = seg.series.as_mut() {
                s.flush().map_err(|e| DynamicError::io("flush", &self.dir, &e))?;
            }
            self.closed.push(seg.info);
        }
        Ok(())
    }

    fn evict_oldest(&mut self) -> DynamicResult<bool> {
        if self.closed.is_empty() {
            return Ok(false);
        }
        let seg = self.closed.remove(0);
        for ext in ["frames", "series"] {
            let p = segment_path(&self.dir, seg.ordinal, ext);
            match std::fs::remove_file(&p) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(DynamicError::io("eviction", &p, &e)),
            }
        }
        self.evicted += 1;
        Ok(true)
    }

    fn ensure_segment(&mut self, t: f64) -> DynamicResult<()> {
        let key = self.segment_key(t);
        if self.current.as_ref().is_some_and(|c| c.info.key == key) {
            return Ok(());
        }
        self.close_current()?;
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        let path = segment_path(&self.dir, ordinal, "frames");
        let file = implexity_io::fsguard::create_new_nofollow(&path, 0o600)
            .map_err(|e| DynamicError::io("segment creation", &path, &e))?;
        self.current = Some(OpenSegment {
            info: SegmentInfo { ordinal, key, bytes: 0, frames: 0, rows: 0, t_first: t, t_last: t },
            frames: BufWriter::with_capacity(1 << 16, file),
            series: None,
            bins: vec![false; self.manifest.retention.phase_bins],
        });
        while self.closed.len() + 1 > self.manifest.retention.retain_segments {
            self.evict_oldest()?;
        }
        self.write_status("writing")
    }

    fn skip(&mut self, reason: &'static str) -> FrameOutcome {
        let n = self.skipped.get(reason).and_then(Value::as_u64).unwrap_or(0) + 1;
        self.skipped.insert(reason.to_owned(), json!(n));
        FrameOutcome::Skipped(reason)
    }



    pub fn push_frame(
        &mut self,
        t: f64,
        phase: Option<f64>,
        fields: &[(&str, &[f64])],
    ) -> DynamicResult<FrameOutcome> {
        if !t.is_finite() || t < self.last_t {
            return Err(DynamicError::invalid("frame times must be finite and non-decreasing"));
        }
        if phase.is_some_and(|p| !(0.0..=1.0).contains(&p)) {
            return Err(DynamicError::invalid("an explicit frame phase must lie in [0, 1]"));
        }
        if let Some(i) = self.meshes_written.iter().position(|w| !*w) {
            return Err(DynamicError::invalid(format!(
                "mesh {} must be written (write_mesh) before the first frame",
                self.manifest.meshes[i].name
            )));
        }
        let m = &self.manifest;
        if fields.len() != m.fields.len() {
            return Err(DynamicError::invalid(format!(
                "a frame carries all {} declared fields, not {}",
                m.fields.len(),
                fields.len()
            )));
        }
        let mut order = Vec::with_capacity(fields.len());
        for (i, spec) in m.fields.iter().enumerate() {
            let Some((_, values)) = fields.iter().find(|(n, _)| *n == spec.name) else {
                return Err(DynamicError::invalid(format!("the frame lacks field {}", spec.name)));
            };
            if values.len() != m.values_per_frame(i) {
                return Err(DynamicError::invalid(format!(
                    "field {} needs {} values per frame, not {}",
                    spec.name,
                    m.values_per_frame(i),
                    values.len()
                )));
            }
            order.push(*values);
        }
        if !self.wants_frame(t) {
            self.last_t = t;
            return Ok(self.skip("phase_bin_filled"));
        }
        let precision = m.retention.precision;
        let limit = m.retention.byte_limit;
        let components: Vec<usize> = (0..m.fields.len()).map(|i| m.components(i)).collect();
        let (cycle, derived_phase) = self.phase_of(t).map_or((None, None), |(c, p)| (Some(c), Some(p)));
        let phase = phase.or(derived_phase);
        let seq = self.next_seq;
        let mut records = Vec::with_capacity(order.len());
        for (i, values) in order.iter().enumerate() {
            records.push(encode_record(i, seq, t, phase, cycle, values, precision)?);
        }
        let needed: u64 = records.iter().map(|r| r.len() as u64).sum();
        self.ensure_segment(t)?;
        while self.total_bytes() + needed > limit {
            if !self.evict_oldest()? {
                self.last_t = t;
                return Ok(self.skip("byte_limit"));
            }
        }
        let bin = self.bin_of(t);
        let dir = self.dir.clone();
        let Some(seg) = self.current.as_mut() else {
            return Err(DynamicError::invalid("no open segment"));
        };
        for r in &records {
            seg.frames.write_all(r).map_err(|e| DynamicError::io("frame write", &dir, &e))?;
        }
        seg.info.bytes += needed;
        seg.info.frames += 1;
        seg.info.t_last = t;
        if let Some(b) = bin {
            seg.bins[b] = true;
        }
        for (i, values) in order.iter().enumerate() {
            let r = &mut self.ranges[i];
            let c = components[i];
            for cell in values.chunks_exact(c) {
                let mut mag2 = 0.0;
                let mut finite = true;
                for &v in cell {
                    if v.is_finite() {
                        r.min = r.min.min(v);
                        r.max = r.max.max(v);
                        mag2 += v * v;
                    } else {
                        finite = false;
                        r.nonfinite += 1;
                    }
                }
                if finite {
                    r.magnitude_max = r.magnitude_max.max(mag2.sqrt());
                }
            }
        }
        if self.t_first.is_nan() {
            self.t_first = t;
        }
        self.next_seq += 1;
        self.last_t = t;
        Ok(FrameOutcome::Stored(seq))
    }



    pub fn push_series(&mut self, t: f64, values: &[f64]) -> DynamicResult<()> {
        if values.len() != self.manifest.series.len() {
            return Err(DynamicError::invalid(format!(
                "a series row carries {} values, not {}",
                self.manifest.series.len(),
                values.len()
            )));
        }
        if !t.is_finite() || t < self.last_t {
            return Err(DynamicError::invalid("series times must be finite and non-decreasing"));
        }
        self.ensure_segment(t)?;
        let needed = 8 * (values.len() as u64 + 1);
        while self.total_bytes() + needed > self.manifest.retention.byte_limit {
            if !self.evict_oldest()? {
                self.dropped_rows += 1;
                return Ok(());
            }
        }
        let dir = self.dir.clone();
        let Some(seg) = self.current.as_mut() else {
            return Err(DynamicError::invalid("no open segment"));
        };
        if seg.series.is_none() {
            let path = segment_path(&dir, seg.info.ordinal, "series");
            let file = implexity_io::fsguard::create_new_nofollow(&path, 0o600)
                .map_err(|e| DynamicError::io("series creation", &path, &e))?;
            seg.series = Some(BufWriter::with_capacity(1 << 14, file));
        }
        let mut row = Vec::with_capacity(8 * (values.len() + 1));
        row.extend_from_slice(&t.to_le_bytes());
        for v in values {
            row.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(s) = seg.series.as_mut() {
            s.write_all(&row).map_err(|e| DynamicError::io("series write", &dir, &e))?;
        }
        seg.info.bytes += needed;
        seg.info.rows += 1;
        seg.info.t_last = t;
        if self.t_first.is_nan() {
            self.t_first = t;
        }
        self.last_t = t;
        Ok(())
    }

    fn status_value(&self, state: &str) -> Value {
        let mut segments: Vec<Value> = self.closed.iter().map(SegmentInfo::to_value).collect();
        if let Some(c) = &self.current {
            segments.push(c.info.to_value());
        }
        let mut ranges: Map<String, Value> = self
            .manifest
            .fields
            .iter()
            .zip(&self.ranges)
            .map(|(f, r)| (f.name.clone(), r.to_value()))
            .collect();
        ranges.extend(self.attribute_ranges.clone());
        let frames: u64 = self.closed.iter().map(|s| s.frames).sum::<u64>()
            + self.current.as_ref().map_or(0, |c| c.info.frames);
        json!({
            "schema": STATUS_SCHEMA, "extension": super::EXTENSION, "state": state,
            "frames_pushed_and_stored": self.next_seq, "frames_retained": frames,
            "frames_skipped": self.skipped, "series_rows_dropped": self.dropped_rows,
            "segments_evicted": self.evicted, "segments": segments, "bytes": self.total_bytes(),
            "mesh_bytes": self.static_bytes,
            "byte_limit": self.manifest.retention.byte_limit,
            "period": self.period, "phase_origin": self.phase_origin,
            "t_first": finite_or_null(self.t_first), "t_last": finite_or_null(self.last_t),
            "ranges": ranges,
        })
    }

    fn write_status(&self, state: &str) -> DynamicResult<()> {
        let text = serde_json::to_vec_pretty(&self.status_value(state))
            .map_err(|e| DynamicError::invalid(e.to_string()))?;
        let path = self.dir.join(STATUS_FILE);
        implexity_io::atomic::write_atomic(&path, &text)
            .map_err(|e| DynamicError::io("status write", &path, &e))
    }



    pub fn finish(mut self, complete: bool) -> DynamicResult<StoreSummary> {
        self.close_current()?;
        let state = if complete { "complete" } else { "aborted" };
        self.write_status(state)?;
        self.finished = true;
        Ok(StoreSummary { dir: self.dir.clone(), status: self.status_value(state) })
    }
}

impl Drop for FrameWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.close_current();
            let _ = self.write_status("aborted");
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn encode_record(
    field: usize,
    seq: u64,
    t: f64,
    phase: Option<f64>,
    cycle: Option<i64>,
    values: &[f64],
    precision: Precision,
) -> DynamicResult<Vec<u8>> {
    let width = precision.width();
    let mut raw = Vec::with_capacity(values.len() * width);
    match precision {
        Precision::F32 => values.iter().for_each(|v| raw.extend_from_slice(&(*v as f32).to_le_bytes())),
        Precision::F64 => values.iter().for_each(|v| raw.extend_from_slice(&v.to_le_bytes())),
    }
    let digest = Sha256::digest(&raw);
    let mut enc = DeflateEncoder::new(Vec::with_capacity(raw.len() / 2), Compression::fast());
    enc.write_all(&shuffle(&raw, width))
        .map_err(|e| DynamicError::Io(format!("frame compression failed: {e}")))?;
    let payload = enc.finish().map_err(|e| DynamicError::Io(format!("frame compression failed: {e}")))?;
    let mut out = Vec::with_capacity(HEADER_BYTES + payload.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&ENC_SHUFFLE_DEFLATE.to_le_bytes());
    out.extend_from_slice(&(field as u32).to_le_bytes());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&t.to_le_bytes());
    out.extend_from_slice(&phase.unwrap_or(f64::NAN).to_le_bytes());
    out.extend_from_slice(&cycle.unwrap_or(i64::MIN).to_le_bytes());
    out.extend_from_slice(&(values.len() as u64).to_le_bytes());
    out.push(width as u8);
    out.extend_from_slice(&[0u8; 7]);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&digest);
    debug_assert_eq!(out.len(), HEADER_BYTES);
    out.extend_from_slice(&payload);
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecordLoc {
    segment: u64,
    offset: u64,
    payload_len: u64,
    values: u64,
    width: u8,
    digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameEntry {
    pub seq: u64,
    pub t: f64,
    pub phase: Option<f64>,
    pub cycle: Option<i64>,
    pub segment: u64,
    records: Vec<Option<RecordLoc>>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SeriesTable {
    pub names: Vec<String>,
    pub t: Vec<f64>,
    pub columns: Vec<Vec<f64>>,
}

impl SeriesTable {
    #[must_use]
    pub fn column(&self, name: &str) -> Option<&[f64]> {
        self.names.iter().position(|n| n == name).map(|i| self.columns[i].as_slice())
    }
}

#[derive(Clone, Debug)]
pub struct DynamicStore {
    dir: PathBuf,
    manifest: FrameManifest,
    status: Value,
    frames: Vec<FrameEntry>,
    series_segments: Vec<u64>,
    meshes: Vec<Vec<RecordLoc>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MeshData {
    pub points: Vec<f64>,
    pub cells: Vec<usize>,
}

struct Header {
    field: usize,
    seq: u64,
    t: f64,
    phase: f64,
    cycle: i64,
    values: u64,
    width: u8,
    payload_len: u64,
    digest: [u8; 32],
}

fn parse_header(header: &[u8; HEADER_BYTES]) -> Option<Header> {
    if &header[0..4] != MAGIC
        || u16::from_le_bytes([header[4], header[5]]) != VERSION
        || u16::from_le_bytes([header[6], header[7]]) != ENC_SHUFFLE_DEFLATE
    {
        return None;
    }
    let field = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&header[68..100]);
    Some(Header {
        field,
        seq: le_u64(&header[12..]),
        t: le_f64(&header[20..]),
        phase: le_f64(&header[28..]),
        cycle: i64::from_le_bytes(header[36..44].try_into().unwrap_or([0; 8])),
        values: le_u64(&header[44..]),
        width: header[52],
        payload_len: le_u64(&header[60..]),
        digest,
    })
}

fn decode(p: &Path, loc: &RecordLoc, values: usize, what: &str) -> DynamicResult<Vec<f64>> {
    let mut file = File::open(p).map_err(|e| DynamicError::io("record open", p, &e))?;
    file.seek(SeekFrom::Start(loc.offset)).map_err(|e| DynamicError::io("record seek", p, &e))?;
    let payload_len =
        usize::try_from(loc.payload_len).map_err(|_| DynamicError::Corrupt("record too large".into()))?;
    let mut payload = vec![0u8; payload_len];
    file.read_exact(&mut payload).map_err(|e| DynamicError::io("record read", p, &e))?;
    let width = usize::from(loc.width);
    let expected =
        usize::try_from(loc.values).map_err(|_| DynamicError::Corrupt("record too large".into()))? * width;
    if expected != values * width {
        return Err(DynamicError::Corrupt(format!("{} holds a record of the wrong size", p.display())));
    }
    let mut planes = Vec::with_capacity(expected);
    DeflateDecoder::new(payload.as_slice())
        .take(expected as u64 + 1)
        .read_to_end(&mut planes)
        .map_err(|e| DynamicError::Corrupt(format!("{}: {e}", p.display())))?;
    if planes.len() != expected {
        return Err(DynamicError::Corrupt(format!("{} holds a truncated record", p.display())));
    }
    let raw = unshuffle(&planes, width);
    if Sha256::digest(&raw).as_slice() != loc.digest {
        return Err(DynamicError::Corrupt(format!("{} record of {what} fails its sha256", p.display())));
    }
    Ok(match width {
        4 => raw.chunks_exact(4).map(|c| f64::from(f32::from_le_bytes([c[0], c[1], c[2], c[3]]))).collect(),
        _ => raw.chunks_exact(8).map(le_f64).collect(),
    })
}

fn index_mesh_file(p: &Path, blocks: usize) -> DynamicResult<Vec<RecordLoc>> {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(DynamicError::io("mesh read", p, &e)),
    };
    let mut out: Vec<Option<RecordLoc>> = vec![None; blocks];
    let mut offset = 0usize;
    while offset + HEADER_BYTES <= bytes.len() {
        let mut header = [0u8; HEADER_BYTES];
        header.copy_from_slice(&bytes[offset..offset + HEADER_BYTES]);
        let Some(Header { field: block, values, width, payload_len, digest, .. }) = parse_header(&header)
        else {
            return Err(DynamicError::Corrupt(format!("{} has a malformed record at {offset}", p.display())));
        };
        let data = offset + HEADER_BYTES;
        let end = usize::try_from(payload_len).ok().and_then(|l| data.checked_add(l));
        let Some(end) = end.filter(|e| *e <= bytes.len()) else { break };
        if block >= blocks || !matches!(width, 4 | 8) {
            return Err(DynamicError::Corrupt(format!("{} has an invalid record at {offset}", p.display())));
        }
        out[block] = Some(RecordLoc { segment: 0, offset: data as u64, payload_len, values, width, digest });
        offset = end;
    }

    Ok(if out.iter().all(Option::is_some) { out.into_iter().flatten().collect() } else { Vec::new() })
}

fn read_exact_at(file: &mut BufReader<File>, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => return Ok(false),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

fn le_u64(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[..8]);
    u64::from_le_bytes(a)
}

fn le_f64(b: &[u8]) -> f64 {
    f64::from_bits(le_u64(b))
}

fn segment_ordinals(dir: &Path, ext: &str) -> DynamicResult<Vec<u64>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| DynamicError::io("listing", dir, &e))? {
        let entry = entry.map_err(|e| DynamicError::io("listing", dir, &e))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix("seg_").and_then(|r| r.strip_suffix(&format!(".{ext}")))
            && rest.len() == 6
            && let Ok(n) = rest.parse::<u64>()
        {
            out.push(n);
        }
    }
    out.sort_unstable();
    Ok(out)
}

impl DynamicStore {


    pub fn open(dir: &Path) -> DynamicResult<Self> {
        let path = dir.join(MANIFEST_FILE);
        let bytes = std::fs::read(&path).map_err(|e| DynamicError::io("manifest read", &path, &e))?;
        let doc = implexity_core::json::parse_strict_bytes(&bytes)
            .map_err(|e| DynamicError::Corrupt(format!("{}: {e}", path.display())))?;
        let manifest = FrameManifest::from_value(&doc)?;
        let status = std::fs::read(dir.join(STATUS_FILE))
            .ok()
            .and_then(|b| implexity_core::json::parse_strict_bytes(&b).ok())
            .unwrap_or(Value::Null);
        let nfields = manifest.fields.len();
        let mut frames: Vec<FrameEntry> = Vec::new();
        for ordinal in segment_ordinals(dir, "frames")? {
            let p = segment_path(dir, ordinal, "frames");
            let file = match File::open(&p) {
                Ok(f) => f,

                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(DynamicError::io("segment open", &p, &e)),
            };
            let len = file.metadata().map_err(|e| DynamicError::io("segment stat", &p, &e))?.len();
            let mut r = BufReader::new(file);
            let mut offset = 0u64;
            let mut header = [0u8; HEADER_BYTES];
            loop {
                if offset + HEADER_BYTES as u64 > len
                    || !read_exact_at(&mut r, &mut header)
                        .map_err(|e| DynamicError::io("segment read", &p, &e))?
                {
                    break;
                }
                let Some(Header { field, seq, t, phase, cycle, values, width, payload_len, digest }) =
                    parse_header(&header)
                else {
                    return Err(DynamicError::Corrupt(format!(
                        "{} has a malformed record at {offset}",
                        p.display()
                    )));
                };
                let data_offset = offset + HEADER_BYTES as u64;
                if data_offset + payload_len > len {
                    break;
                }
                if field >= nfields || !matches!(width, 4 | 8) {
                    return Err(DynamicError::Corrupt(format!(
                        "{} has an invalid record at {offset}",
                        p.display()
                    )));
                }
                let loc =
                    RecordLoc { segment: ordinal, offset: data_offset, payload_len, values, width, digest };
                if let Some(f) = frames.last_mut().filter(|f| f.seq == seq) {
                    f.records[field] = Some(loc);
                } else {
                    let mut records = vec![None; nfields];
                    records[field] = Some(loc);
                    frames.push(FrameEntry {
                        seq,
                        t,
                        phase: phase.is_finite().then_some(phase),
                        cycle: (cycle != i64::MIN).then_some(cycle),
                        segment: ordinal,
                        records,
                    });
                }
                #[allow(clippy::cast_possible_wrap)]
                r.seek(SeekFrom::Current(payload_len as i64))
                    .map_err(|e| DynamicError::io("segment seek", &p, &e))?;
                offset = data_offset + payload_len;
            }
        }

        frames.retain(|f| f.records.iter().all(Option::is_some));
        frames.sort_by_key(|f| f.seq);
        let series_segments = segment_ordinals(dir, "series")?;
        let meshes = manifest
            .meshes
            .iter()
            .enumerate()
            .map(|(i, m)| index_mesh_file(&mesh_path(dir, i), 2 + m.attributes.len()))
            .collect::<DynamicResult<Vec<_>>>()?;
        Ok(Self { dir: dir.to_path_buf(), manifest, status, frames, series_segments, meshes })
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub const fn manifest(&self) -> &FrameManifest {
        &self.manifest
    }

    #[must_use]
    pub const fn status(&self) -> &Value {
        &self.status
    }

    #[must_use]
    pub fn frames(&self) -> &[FrameEntry] {
        &self.frames
    }

    #[must_use]
    pub fn period(&self) -> Option<f64> {
        self.status.get("period").and_then(Value::as_f64).or(self.manifest.time.period)
    }

    #[must_use]
    pub fn phase_origin(&self) -> f64 {
        self.status.get("phase_origin").and_then(Value::as_f64).unwrap_or(self.manifest.time.phase_origin)
    }

    #[must_use]
    pub fn range(&self, name: &str) -> Option<(f64, f64, f64)> {
        let r = self.status.get("ranges")?.get(name)?;
        Some((r.get("min")?.as_f64()?, r.get("max")?.as_f64()?, r.get("magnitude_max")?.as_f64()?))
    }



    pub fn read_field(&self, frame: &FrameEntry, field: usize) -> DynamicResult<Vec<f64>> {
        let Some(Some(loc)) = frame.records.get(field) else {
            return Err(DynamicError::invalid(format!("frame {} has no field {field}", frame.seq)));
        };
        let p = segment_path(&self.dir, loc.segment, "frames");
        decode(&p, loc, self.manifest.values_per_frame(field), &format!("frame {} field {field}", frame.seq))
    }



    pub fn mesh(&self, name: &str) -> DynamicResult<MeshData> {
        let (i, m) = self.mesh_index(name)?;
        let blocks = &self.meshes[i];
        let p = mesh_path(&self.dir, i);
        let points = decode(&p, &blocks[0], m.nodes * m.cell.dims(), "mesh points")?;
        let raw = decode(&p, &blocks[1], m.cells * m.cell.nodes_per_cell(), "mesh cells")?;
        let mut cells = Vec::with_capacity(raw.len());
        for x in raw {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = x as usize;

            #[allow(clippy::cast_precision_loss, clippy::float_cmp)]
            let exact = n as f64 == x;
            if x < 0.0 || !exact || n >= m.nodes {
                return Err(DynamicError::Corrupt(format!("{} holds an invalid node index", p.display())));
            }
            cells.push(n);
        }
        Ok(MeshData { points, cells })
    }

    fn mesh_index(&self, name: &str) -> DynamicResult<(usize, &super::manifest::MeshSpec)> {
        let Some(i) = self.manifest.meshes.iter().position(|m| m.name == name) else {
            return Err(DynamicError::invalid(format!("the store has no mesh {name:?}")));
        };
        if self.meshes[i].is_empty() {
            return Err(DynamicError::Corrupt(format!("mesh {name} was not written completely")));
        }
        Ok((i, &self.manifest.meshes[i]))
    }



    pub fn read_attribute(&self, name: &str) -> DynamicResult<Vec<f64>> {
        let Some((mesh, spec)) = self.manifest.attribute(name) else {
            return Err(DynamicError::invalid(format!("the store has no mesh attribute {name:?}")));
        };
        let (i, m) = self.mesh_index(&mesh.name)?;
        let j = m.attributes.iter().position(|a| a.name == spec.name).unwrap_or(0);
        let sites = match self.manifest.location(&spec.grid) {
            Some(Location::Nodes(_)) => m.nodes,
            _ => m.cells,
        };
        decode(&mesh_path(&self.dir, i), &self.meshes[i][2 + j], sites, &format!("attribute {name}"))
    }



    pub fn read_named(&self, frame: &FrameEntry, name: &str) -> DynamicResult<Vec<f64>> {
        if self.manifest.field(name).is_none() && self.manifest.attribute(name).is_some() {
            return self.read_attribute(name);
        }
        let (i, _) = self
            .manifest
            .field(name)
            .ok_or_else(|| DynamicError::invalid(format!("the store has no field {name:?}")))?;
        self.read_field(frame, i)
    }



    pub fn series(&self) -> DynamicResult<SeriesTable> {
        let n = self.manifest.series.len();
        let mut table = SeriesTable {
            names: self.manifest.series.iter().map(|s| s.name.clone()).collect(),
            t: Vec::new(),
            columns: vec![Vec::new(); n],
        };
        if n == 0 {
            return Ok(table);
        }
        let row = 8 * (n + 1);
        for &ordinal in &self.series_segments {
            let p = segment_path(&self.dir, ordinal, "series");
            let bytes = match std::fs::read(&p) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(DynamicError::io("series read", &p, &e)),
            };
            for chunk in bytes.chunks_exact(row) {
                table.t.push(le_f64(chunk));
                for (i, col) in table.columns.iter_mut().enumerate() {
                    col.push(le_f64(&chunk[8 * (i + 1)..]));
                }
            }
        }
        Ok(table)
    }

    #[must_use]
    pub fn describe(&self) -> Value {
        let m = &self.manifest;
        let cycles: std::collections::BTreeSet<i64> = self.frames.iter().filter_map(|f| f.cycle).collect();
        json!({
            "manifest": m.to_value(),
            "state": self.status.get("state").cloned().unwrap_or(json!("unknown")),
            "frames": self.frames.len(),
            "t_range": [self.frames.first().map(|f| f.t), self.frames.last().map(|f| f.t)],
            "cycles_retained": cycles.into_iter().collect::<Vec<_>>(),
            "period": self.period(),
            "phase_origin": self.phase_origin(),
            "bytes": self.status.get("bytes").cloned().unwrap_or(Value::Null),
            "ranges": self.status.get("ranges").cloned().unwrap_or(Value::Null),
            "frames_skipped": self.status.get("frames_skipped").cloned().unwrap_or(Value::Null),
            "segments_evicted": self.status.get("segments_evicted").cloned().unwrap_or(Value::Null),
        })
    }
}

