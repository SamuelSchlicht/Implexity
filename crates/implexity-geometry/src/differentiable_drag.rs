// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{Value, json};

use crate::numpy;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DragError(pub String);

type DR<T> = Result<T, DragError>;

fn err<T>(m: impl Into<String>) -> DR<T> {
    Err(DragError(m.into()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DragPolicy {
    pub damping: f64,
    pub trust_radius: f64,
    pub max_component_step: f64,
    pub feasibility_tolerance: f64,
    pub rank_tolerance: f64,
    pub bound_margin: f64,
}

impl Default for DragPolicy {
    fn default() -> Self {
        Self {
            damping: 1.0e-8,
            trust_radius: 0.20,
            max_component_step: 0.25,
            feasibility_tolerance: 2.0e-3,
            rank_tolerance: 1.0e-10,
            bound_margin: 0.0,
        }
    }
}

impl DragPolicy {

    pub fn validated(self) -> DR<Self> {
        for (name, v) in [
            ("damping", self.damping),
            ("trust_radius", self.trust_radius),
            ("max_component_step", self.max_component_step),
            ("feasibility_tolerance", self.feasibility_tolerance),
            ("rank_tolerance", self.rank_tolerance),
            ("bound_margin", self.bound_margin),
        ] {
            if !v.is_finite() || v < 0.0 {
                return err(format!("{name} must be finite and non-negative"));
            }
        }
        if self.trust_radius == 0.0 {
            return err("trust_radius must be positive");
        }
        if self.max_component_step == 0.0 {
            return err("max_component_step must be positive");
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DragResult {
    pub delta_native: Vec<f64>,
    pub delta_normalised: Vec<f64>,
    pub predicted_surface_residual: Vec<f64>,
    pub requested_normal_motion: Vec<f64>,
    pub achieved_normal_motion: Vec<f64>,
    pub active_parameters: Vec<usize>,
    pub clipped_parameters: Vec<usize>,
    pub singular_values: Vec<f64>,
    pub effective_rank: usize,
    pub condition_number: f64,
    pub trust_scale: f64,
    pub feasible_first_order: bool,
    pub influence: Vec<f64>,
    pub warnings: Vec<String>,
}

impl DragResult {
    #[must_use]
    pub fn as_dict(&self) -> Value {
        json!({
            "delta_native": self.delta_native, "delta_normalised": self.delta_normalised,
            "predicted_surface_residual": self.predicted_surface_residual,
            "requested_normal_motion": self.requested_normal_motion, "achieved_normal_motion": self.achieved_normal_motion,
            "active_parameters": self.active_parameters, "clipped_parameters": self.clipped_parameters,
            "singular_values": self.singular_values, "effective_rank": self.effective_rank,
            "condition_number": crate::value::json_f64(self.condition_number), "trust_scale": self.trust_scale,
            "feasible_first_order": self.feasible_first_order, "influence": self.influence, "warnings": self.warnings,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Matrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

impl Matrix {

    pub fn from_rows(rows: &[Vec<f64>], name: &str, columns: Option<usize>) -> DR<Self> {
        let r = rows.len();
        let c = rows.first().map_or(0, Vec::len);
        if r == 0 || c == 0 || rows.iter().any(|x| x.len() != c) {
            return err(format!("{name} must be a non-empty rank-2 array"));
        }
        if let Some(want) = columns
            && c != want
        {
            return err(format!("{name} must have {want} columns"));
        }
        let data: Vec<f64> = rows.iter().flatten().copied().collect();
        if !data.iter().all(|v| v.is_finite()) {
            return err(format!("{name} contains non-finite values"));
        }
        Ok(Self { rows: r, cols: c, data })
    }

    fn at(&self, i: usize, j: usize) -> f64 {
        self.data[i * self.cols + j]
    }
}

fn vector(v: &[f64], name: &str, size: usize, positive: bool) -> DR<Vec<f64>> {
    if v.is_empty() {
        return err(format!("{name} must not be empty"));
    }
    if v.len() != size {
        return err(format!("{name} must contain {size} values"));
    }
    if !v.iter().all(|x| x.is_finite()) {
        return err(format!("{name} contains non-finite values"));
    }
    if positive && v.iter().any(|x| *x <= 0.0) {
        return err(format!("{name} must be strictly positive"));
    }
    Ok(v.to_vec())
}

#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn thin_svd(b: &Matrix) -> (Vec<Vec<f64>>, Vec<f64>, Vec<Vec<f64>>) {
    let (m, k) = (b.rows, b.cols);
    let mut cols: Vec<Vec<f64>> = (0..k).map(|j| (0..m).map(|i| b.at(i, j)).collect()).collect();
    let mut v: Vec<Vec<f64>> =
        (0..k).map(|j| (0..k).map(|i| if i == j { 1.0 } else { 0.0 }).collect()).collect();
    for _sweep in 0..80 {
        let mut rotated = false;
        for p in 0..k {
            for q in p + 1..k {
                let alpha: f64 = cols[p].iter().map(|x| x * x).sum();
                let beta: f64 = cols[q].iter().map(|x| x * x).sum();
                let gamma: f64 = cols[p].iter().zip(&cols[q]).map(|(x, y)| x * y).sum();
                if gamma == 0.0 || gamma.abs() <= f64::EPSILON * (alpha * beta).sqrt() {
                    continue;
                }
                rotated = true;
                let zeta = (beta - alpha) / (2.0 * gamma);
                let t = zeta.signum() / (zeta.abs() + (1.0 + zeta * zeta).sqrt());
                let t = if zeta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = c * t;
                for i in 0..m {
                    let (x, y) = (cols[p][i], cols[q][i]);
                    cols[p][i] = c * x - s * y;
                    cols[q][i] = s * x + c * y;
                }
                for row in &mut v {
                    let (x, y) = (row[p], row[q]);
                    row[p] = c * x - s * y;
                    row[q] = s * x + c * y;
                }
            }
        }
        if !rotated {
            break;
        }
    }
    let mut order: Vec<(f64, usize)> = cols.iter().enumerate().map(|(j, c)| (numpy::norm(c), j)).collect();
    order.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let r = m.min(k);
    let mut u = Vec::with_capacity(r);
    let mut s = Vec::with_capacity(r);
    let mut vv = Vec::with_capacity(r);
    for &(sv, j) in order.iter().take(r) {
        s.push(sv);
        u.push(if sv > 0.0 { cols[j].iter().map(|x| x / sv).collect() } else { vec![0.0; m] });
        vv.push((0..k).map(|i| v[i][j]).collect());
    }
    (u, s, vv)
}

#[derive(Clone, Debug)]
pub struct DragProblem {
    pub spatial_gradient: Vec<Vec<f64>>,
    pub parameter_jacobian: Vec<Vec<f64>>,
    pub desired_displacement: Vec<Vec<f64>>,
    pub parameter_values: Vec<f64>,
    pub parameter_scales: Vec<f64>,
    pub lower_bounds: Option<Vec<f64>>,
    pub upper_bounds: Option<Vec<f64>>,
    pub locked: Option<Vec<bool>>,
    pub parameter_weights: Option<Vec<f64>>,
    pub sample_weights: Option<Vec<f64>>,
}


#[allow(clippy::too_many_lines)]
pub fn solve_surface_drag(pb: &DragProblem, policy: Option<DragPolicy>) -> DR<DragResult> {
    let policy = policy.unwrap_or_default().validated()?;
    let gx = Matrix::from_rows(&pb.spatial_gradient, "spatial_gradient", Some(3))?;
    let gp = Matrix::from_rows(&pb.parameter_jacobian, "parameter_jacobian", None)?;
    if gp.rows != gx.rows {
        return err("spatial_gradient and parameter_jacobian sample counts differ");
    }
    let dx = Matrix::from_rows(&pb.desired_displacement, "desired_displacement", Some(3))?;
    if dx.rows != gx.rows {
        return err("desired_displacement sample count differs");
    }
    let (m, n) = (gp.rows, gp.cols);
    let values = vector(&pb.parameter_values, "parameter_values", n, false)?;
    let scales = vector(&pb.parameter_scales, "parameter_scales", n, true)?;
    let lower = match &pb.lower_bounds {
        None => vec![f64::NEG_INFINITY; n],
        Some(v) => vector(v, "lower_bounds", n, false)?,
    };
    let upper = match &pb.upper_bounds {
        None => vec![f64::INFINITY; n],
        Some(v) => vector(v, "upper_bounds", n, false)?,
    };
    if lower.iter().zip(&upper).any(|(l, u)| l > u) {
        return err("lower_bounds exceed upper_bounds");
    }
    if values.iter().zip(&lower).any(|(v, l)| v < l) || values.iter().zip(&upper).any(|(v, u)| v > u) {
        return err("parameter_values lie outside their bounds");
    }
    let lock = pb.locked.clone().unwrap_or_else(|| vec![false; n]);
    if lock.len() != n {
        return err(format!("locked must contain {n} values"));
    }
    let active: Vec<usize> = (0..n).filter(|i| !lock[*i]).collect();
    if active.is_empty() {
        return err("all parameters are locked");
    }
    let pweight = match &pb.parameter_weights {
        None => vec![1.0; n],
        Some(v) => vector(v, "parameter_weights", n, true)?,
    };
    let sweight = match &pb.sample_weights {
        None => vec![1.0; m],
        Some(v) => vector(v, "sample_weights", m, true)?,
    };
    let norms: Vec<f64> = (0..m).map(|i| numpy::norm(&[gx.at(i, 0), gx.at(i, 1), gx.at(i, 2)])).collect();
    let bad: Vec<usize> = (0..m).filter(|i| norms[*i] <= 1.0e-14).collect();
    if !bad.is_empty() {
        let list = crate::pyfmt::PyObj::List(
            bad.iter().map(|b| crate::pyfmt::PyObj::Int(i64::try_from(*b).unwrap_or(0))).collect(),
        )
        .repr();
        return err(format!("surface spatial gradient vanishes at samples {list}"));
    }
    let requested: Vec<f64> =
        (0..m).map(|i| (0..3).map(|a| gx.at(i, a) / norms[i] * dx.at(i, a)).sum()).collect();
    let rhs: Vec<f64> = requested.iter().map(|r| -r).collect();
    let a_mat: Vec<Vec<f64>> =
        (0..m).map(|i| (0..n).map(|j| gp.at(i, j) / norms[i] * scales[j]).collect()).collect();
    let sw: Vec<f64> = sweight.iter().map(|w| w.sqrt()).collect();
    let root_pw: Vec<f64> = active.iter().map(|j| pweight[*j].sqrt()).collect();
    let b_rows: Vec<Vec<f64>> = (0..m)
        .map(|i| active.iter().zip(&root_pw).map(|(j, r)| sw[i] * a_mat[i][*j] / r).collect())
        .collect();
    let bw: Vec<f64> = (0..m).map(|i| sw[i] * rhs[i]).collect();
    let bmat = Matrix { rows: m, cols: active.len(), data: b_rows.iter().flatten().copied().collect() };
    let (u, singular, v) = thin_svd(&bmat);
    let (rank, condition) = if singular.is_empty() {
        (0, f64::INFINITY)
    } else {
        let threshold = policy.rank_tolerance.max(singular[0] * policy.rank_tolerance);
        let rank = singular.iter().filter(|s| **s > threshold).count();
        #[allow(clippy::cast_precision_loss)]
        let cond = if rank > 0 { singular[0] / singular[rank - 1] } else { f64::INFINITY };
        (rank, cond)
    };
    let mut q = vec![0.0; active.len()];
    for (k, s) in singular.iter().enumerate() {
        let filt = s / (s * s + policy.damping);
        let ub: f64 = u[k].iter().zip(&bw).map(|(a, b)| a * b).sum();
        for (qi, vi) in q.iter_mut().zip(&v[k]) {
            *qi += vi * filt * ub;
        }
    }
    let mut dz = vec![0.0; n];
    for ((j, qv), r) in active.iter().zip(&q).zip(&root_pw) {
        dz[*j] = qv / r;
    }
    let mut warnings = Vec::new();
    if rank < m.min(active.len()) {
        warnings.push(
            "surface drag is underdetermined or rank-deficient; regularised minimum movement was used"
                .to_string(),
        );
    }
    if !condition.is_finite() || condition > 1.0e8 {
        warnings.push("surface-to-parameter mapping is poorly conditioned".into());
    }
    let mut trust_scale = 1.0f64;
    let norm = numpy::norm(&dz);
    if norm > policy.trust_radius {
        trust_scale = trust_scale.min(policy.trust_radius / norm);
    }
    let max_component = dz.iter().fold(0.0f64, |a, v| a.max(v.abs()));
    if max_component > policy.max_component_step {
        trust_scale = trust_scale.min(policy.max_component_step / max_component);
    }
    if trust_scale < 1.0 {
        for v in &mut dz {
            *v *= trust_scale;
        }
        warnings.push("parameter update was reduced by the drag trust region".into());
    }
    let proposed: Vec<f64> = (0..n).map(|j| values[j] + scales[j] * dz[j]).collect();
    let lo: Vec<f64> = (0..n).map(|j| lower[j] + policy.bound_margin * scales[j]).collect();
    let hi: Vec<f64> = (0..n).map(|j| upper[j] - policy.bound_margin * scales[j]).collect();
    if lo.iter().zip(&hi).any(|(l, h)| l > h) {
        return err("bound_margin leaves an empty feasible interval");
    }
    let bounded: Vec<f64> = (0..n).map(|j| proposed[j].max(lo[j]).min(hi[j])).collect();
    let clipped: Vec<usize> =
        (0..n).filter(|j| (bounded[*j] - proposed[*j]).abs() > 1.0e-14 * scales[*j].max(1.0)).collect();
    if !clipped.is_empty() {
        warnings.push("one or more parameters reached a bound".into());
    }
    let dp: Vec<f64> = (0..n).map(|j| bounded[j] - values[j]).collect();
    let dz: Vec<f64> = (0..n).map(|j| dp[j] / scales[j]).collect();
    let adz: Vec<f64> = (0..m).map(|i| (0..n).map(|j| a_mat[i][j] * dz[j]).sum()).collect();
    let residual: Vec<f64> = adz.iter().zip(&rhs).map(|(a, r)| a - r).collect();
    let achieved: Vec<f64> = adz.iter().map(|a| -a).collect();
    let scale_ref = requested.iter().fold(0.0f64, |a, v| a.max(v.abs())).max(1.0e-12);
    let feasible = residual.iter().fold(0.0f64, |a, v| a.max(v.abs()))
        <= policy.feasibility_tolerance * scale_ref + 1.0e-12;
    if !feasible {
        warnings.push(
            "requested surface motion is not fully achievable with the active parameters at first order"
                .into(),
        );
    }
    let mut influence: Vec<f64> =
        (0..n).map(|j| numpy::norm(&(0..m).map(|i| a_mat[i][j]).collect::<Vec<_>>())).collect();
    for (inf, l) in influence.iter_mut().zip(&lock) {
        if *l {
            *inf = 0.0;
        }
    }
    let total: f64 = influence.iter().sum();
    if total > 0.0 {
        for v in &mut influence {
            *v /= total;
        }
    }
    Ok(DragResult {
        delta_native: dp,
        delta_normalised: dz,
        predicted_surface_residual: residual,
        requested_normal_motion: requested,
        achieved_normal_motion: achieved,
        active_parameters: active,
        clipped_parameters: clipped,
        singular_values: singular,
        effective_rank: rank,
        condition_number: condition,
        trust_scale,
        feasible_first_order: feasible,
        influence,
        warnings,
    })
}


pub fn rank_parameter_influence(
    spatial_gradient: &[Vec<f64>],
    parameter_jacobian: &[Vec<f64>],
    parameter_scales: &[f64],
    locked: Option<&[bool]>,
) -> DR<Vec<f64>> {
    let gx = Matrix::from_rows(spatial_gradient, "spatial_gradient", Some(3))?;
    let gp = Matrix::from_rows(parameter_jacobian, "parameter_jacobian", None)?;
    if gp.rows != gx.rows {
        return err("sample counts differ");
    }
    let scales = vector(parameter_scales, "parameter_scales", gp.cols, true)?;
    let norms: Vec<f64> =
        (0..gx.rows).map(|i| numpy::norm(&[gx.at(i, 0), gx.at(i, 1), gx.at(i, 2)])).collect();
    if norms.iter().any(|v| *v <= 1.0e-14) {
        return err("surface spatial gradient vanishes");
    }
    let mut score: Vec<f64> = (0..gp.cols)
        .map(|j| numpy::norm(&(0..gp.rows).map(|i| gp.at(i, j) / norms[i] * scales[j]).collect::<Vec<_>>()))
        .collect();
    if let Some(mask) = locked {
        if mask.len() != score.len() {
            return err("locked has the wrong size");
        }
        for (s, l) in score.iter_mut().zip(mask) {
            if *l {
                *s = 0.0;
            }
        }
    }
    let total: f64 = score.iter().sum();
    if total > 0.0 {
        for s in &mut score {
            *s /= total;
        }
    }
    Ok(score)
}

#[derive(Clone, Debug)]
pub struct DragSession {
    pub session_id: String,
    pub base_revision: String,
    pub initial_values: Vec<f64>,
    pub created_at: Instant,
    pub last_sequence: i64,
    pub accepted_sequence: i64,
    pub pending_sequence: i64,
    pub preview_values: Option<Vec<f64>>,
    pub committed: bool,
    pub cancelled: bool,
}

impl DragSession {
    fn new(session_id: &str, base_revision: &str, values: Vec<f64>) -> Self {
        Self {
            session_id: session_id.into(),
            base_revision: base_revision.into(),
            initial_values: values,
            created_at: Instant::now(),
            last_sequence: -1,
            accepted_sequence: -1,
            pending_sequence: -1,
            preview_values: None,
            committed: false,
            cancelled: false,
        }
    }


    pub fn accept_preview(&mut self, sequence: i64, values: &[f64]) -> DR<bool> {
        if self.committed || self.cancelled {
            return err("drag session is closed");
        }
        if sequence <= self.accepted_sequence {
            return Ok(false);
        }
        if values.len() != self.initial_values.len() || !values.iter().all(|v| v.is_finite()) {
            return err("preview values are invalid");
        }
        self.last_sequence = self.last_sequence.max(sequence);
        self.accepted_sequence = sequence;
        self.preview_values = Some(values.to_vec());
        Ok(true)
    }


    pub fn mark_requested(&mut self, sequence: i64) -> DR<bool> {
        if self.committed || self.cancelled {
            return err("drag session is closed");
        }
        if sequence <= self.pending_sequence {
            return Ok(false);
        }
        self.pending_sequence = sequence;
        self.last_sequence = self.last_sequence.max(sequence);
        Ok(true)
    }


    pub fn commit(&mut self, expected_revision: &str, expected_sequence: Option<i64>) -> DR<Vec<f64>> {
        if self.cancelled {
            return err("drag session was cancelled");
        }
        if self.committed {
            return err("drag session is already committed");
        }
        if expected_revision != self.base_revision {
            return err("model revision changed during drag");
        }
        if self.pending_sequence != self.accepted_sequence {
            return err(format!(
                "the latest requested preview has not been acknowledged: requested {}, accepted {}",
                self.pending_sequence, self.accepted_sequence
            ));
        }
        if let Some(e) = expected_sequence
            && e != self.accepted_sequence
        {
            return err(format!(
                "commit does not name the exact accepted preview: expected {e}, accepted {}",
                self.accepted_sequence
            ));
        }
        self.committed = true;
        Ok(self.preview_values.clone().unwrap_or_else(|| self.initial_values.clone()))
    }


    pub fn cancel(&mut self) -> DR<Vec<f64>> {
        if self.committed {
            return err("committed drag cannot be cancelled");
        }
        self.cancelled = true;
        Ok(self.initial_values.clone())
    }
}

#[derive(Debug, Default)]
pub struct DragSessionRegistry {
    sessions: Mutex<BTreeMap<String, DragSession>>,
}

impl DragSessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }


    pub fn begin(&self, session_id: &str, base_revision: &str, values: &[f64]) -> DR<DragSession> {
        if values.is_empty() || !values.iter().all(|v| v.is_finite()) {
            return err("initial values are invalid");
        }
        let mut s = self.sessions.lock().map_err(|_| DragError("drag registry poisoned".into()))?;
        if s.contains_key(session_id) {
            return err(format!("drag session already exists: {session_id}"));
        }
        let session = DragSession::new(session_id, base_revision, values.to_vec());
        s.insert(session_id.into(), session.clone());
        Ok(session)
    }


    pub fn with<T>(&self, session_id: &str, f: impl FnOnce(&mut DragSession) -> DR<T>) -> DR<T> {
        let mut s = self.sessions.lock().map_err(|_| DragError("drag registry poisoned".into()))?;
        let Some(session) = s.get_mut(session_id) else {
            return err(format!("unknown drag session: {session_id}"));
        };
        f(session)
    }


    pub fn get(&self, session_id: &str) -> DR<DragSession> {
        self.with(session_id, |s| Ok(s.clone()))
    }

    pub fn close(&self, session_id: &str) {
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(session_id);
        }
    }
}

