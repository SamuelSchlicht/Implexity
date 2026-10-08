// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use serde_json::{Value, json};

use crate::MeshError;
use crate::cast::trunc_usize;
use crate::grid::Grid3;

#[derive(Clone, Debug, PartialEq)]
pub struct ModelStatus {
    pub content_id: String,
    pub structure_id: String,
    pub aabb: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evaluated {
    pub values: Vec<f64>,
    pub content_id: String,
    pub evaluator: String,
    pub mode: String,
    pub units: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SamplingHint {
    pub spacing_mm: f64,
    pub declared_by: Vec<String>,
    pub frame: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Centering {
    Node,
    Cell,
}

impl Centering {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Cell => "cell",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredGrid {
    pub values: Grid3,
    pub origin: [f64; 3],
    pub spacing: [f64; 3],
    pub centering: Centering,
    pub record: Value,
}

pub trait LiveLockGuard {}

impl<T> LiveLockGuard for T {}

pub type LiveGuard<'a> = Box<dyn LiveLockGuard + 'a>;

pub trait ModelView: Sync {


    fn status(&self) -> Result<ModelStatus, MeshError>;

    fn live_lock(&self) -> LiveGuard<'_>;



    fn evaluate_exact(&self, points: &[[f64; 3]]) -> Result<Evaluated, MeshError>;



    fn geometry_sampling_hint(&self) -> Result<Option<SamplingHint>, MeshError>;



    fn registered_fields(&self) -> Result<Vec<Value>, MeshError>;



    fn resolve_registered_field(&self, field: &str) -> Result<RegisteredGrid, MeshError>;
}

#[must_use]
pub fn max_eval_points() -> usize {
    std::env::var("IMPLEXITY_IMPLICIT_MAX_POINTS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(200_000)
}



pub fn evaluate_model_blocks(
    view: &dyn ModelView,
    points: &[[f64; 3]],
    expected_content: &str,
) -> Result<(Vec<f64>, Value), MeshError> {
    let block_length = max_eval_points().max(1);
    let mut values = Vec::with_capacity(points.len());
    let mut records: Vec<Evaluated> = Vec::new();
    for block in points.chunks(block_length) {
        let used = block.len();
        let mut padded = block.to_vec();
        if let Some(last) = block.last() {
            padded.resize(block_length, *last);
        }
        let mut evaluated = view.evaluate_exact(&padded)?;
        if evaluated.values.len() != block_length || evaluated.content_id != expected_content {
            return Err(MeshError::invalid(
                "the authoritative model evaluator returned an invalid or stale sample block",
            ));
        }
        values.extend_from_slice(&evaluated.values[..used]);
        evaluated.values = Vec::new();
        records.push(evaluated);
    }
    let Some(first) = records.first() else {
        return Ok((values, json!({"blocks": 0, "block_points": block_length})));
    };
    if records.iter().any(|r| r.evaluator != first.evaluator || r.mode != first.mode) {
        return Err(MeshError::invalid("the model evaluator changed between sample blocks"));
    }
    if values.iter().any(|v| !v.is_finite()) {
        return Err(MeshError::invalid("the authoritative model evaluator returned non-finite samples"));
    }
    Ok((
        values,
        json!({"evaluator": first.evaluator, "mode": first.mode, "units": first.units,
               "content_id": first.content_id, "blocks": records.len(), "block_points": block_length}),
    ))
}

#[derive(Clone, Debug)]
pub struct GridSampler {
    values: Grid3,
    sample_origin: [f64; 3],
    spacing: [f64; 3],
    domain_lo: [f64; 3],
    domain_hi: [f64; 3],
    nearest: bool,
}

impl GridSampler {
    #[must_use]
    pub fn new(grid: &RegisteredGrid, nearest: bool) -> Self {
        let cell = grid.centering == Centering::Cell;
        let shape = grid.values.shape;
        let sample_origin: [f64; 3] =
            std::array::from_fn(|a| grid.origin[a] + if cell { 0.5 * grid.spacing[a] } else { 0.0 });
        let domain_hi: [f64; 3] = std::array::from_fn(|a| {
            let n = shape[a] as f64;
            grid.origin[a] + grid.spacing[a] * if cell { n } else { (n - 1.0).max(0.0) }
        });
        Self {
            values: grid.values.clone(),
            sample_origin,
            spacing: grid.spacing,
            domain_lo: grid.origin,
            domain_hi,
            nearest,
        }
    }



    pub fn sample(&self, points: &[[f64; 3]], tolerance_mm: f64) -> Result<(Vec<f64>, Vec<bool>), MeshError> {
        if points.iter().flatten().any(|v| !v.is_finite()) {
            return Err(MeshError::invalid("registered-field sample points must be finite xyz values"));
        }
        if !tolerance_mm.is_finite() || tolerance_mm < 0.0 {
            return Err(MeshError::invalid("registered-field coverage tolerance must be >= 0"));
        }
        let shape = self.values.shape;
        let mut out = Vec::with_capacity(points.len());
        let mut valid = Vec::with_capacity(points.len());
        for p in points {
            valid.push((0..3).all(|a| {
                p[a] >= self.domain_lo[a] - tolerance_mm && p[a] <= self.domain_hi[a] + tolerance_mm
            }));
            let c: [f64; 3] = std::array::from_fn(|a| (p[a] - self.sample_origin[a]) / self.spacing[a]);
            if self.nearest {
                let idx: [usize; 3] = std::array::from_fn(|a| {
                    let r = c[a].round_ties_even().max(0.0);
                    trunc_usize(r).min(shape[a] - 1)
                });
                out.push(self.values.at(idx[0], idx[1], idx[2]));
                continue;
            }
            let mut lo = [0usize; 3];
            let mut hi = [0usize; 3];
            let mut f = [0.0; 3];
            for a in 0..3 {
                let top = (shape[a] - 1) as f64;
                let ca = c[a].max(0.0).min(top);
                let lower = trunc_usize(ca.floor()).min(shape[a].saturating_sub(2));
                lo[a] = lower;
                hi[a] = (lower + 1).min(shape[a] - 1);
                f[a] = ca - lower as f64;
            }
            let v = |i: usize, j: usize, k: usize| self.values.at(i, j, k);
            let (u, w1, w) = (f[0], f[1], f[2]);
            let c00 = v(lo[0], lo[1], lo[2]) * (1.0 - u) + v(hi[0], lo[1], lo[2]) * u;
            let c10 = v(lo[0], hi[1], lo[2]) * (1.0 - u) + v(hi[0], hi[1], lo[2]) * u;
            let c01 = v(lo[0], lo[1], hi[2]) * (1.0 - u) + v(hi[0], lo[1], hi[2]) * u;
            let c11 = v(lo[0], hi[1], hi[2]) * (1.0 - u) + v(hi[0], hi[1], hi[2]) * u;
            out.push((c00 * (1.0 - w1) + c10 * w1) * (1.0 - w) + (c01 * (1.0 - w1) + c11 * w1) * w);
        }
        Ok((out, valid))
    }
}



pub fn registered_field_sampler(
    view: &dyn ModelView,
    field: &str,
    nearest: bool,
) -> Result<(GridSampler, Value), MeshError> {
    let grid = view.resolve_registered_field(field)?;
    Ok((GridSampler::new(&grid, nearest), grid.record.clone()))
}

