// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::path::{Path, PathBuf};
use std::time::Instant;

use implexity_io::npy::{NpyArray, NpyData};
use ndarray::ArrayD;
use serde_json::Value;

use crate::design::NamedArrays;

pub const CHECKPOINT_FILE: &str = "ckpt.npz";
pub const CHECKPOINT_SCHEMA: &str = "implexity-optimisation-checkpoint/1";

#[derive(Debug, thiserror::Error)]
pub enum OptJobError {
    #[error("{0}")]
    Halted(String),
    #[error("{0}")]
    NotACheckpoint(String),
    #[error("{0}")]
    Io(String),
}

#[must_use]
pub fn read_control(job_dir: &Path) -> Option<String> {
    let p = job_dir.join("control.json");
    if !p.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&p).ok()?;
    let value = implexity_core::json::parse_with(
        &text,
        implexity_core::json::ParseOptions { reject_duplicate_keys: false },
    )
    .ok()?;
    match value {
        Value::Object(m) => m.get("op").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}


pub fn atomic_savez(
    path: &Path,
    compress: bool,
    payload: &[(String, NpyArray)],
) -> Result<(f64, u64), OptJobError> {
    let t0 = Instant::now();
    let members: Vec<(&str, &NpyArray)> = payload.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let bytes = if compress {
        implexity_io::npz::save_compressed(&members)
    } else {
        implexity_io::npz::save(&members)
    }
    .map_err(|e| OptJobError::Io(e.to_string()))?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp.npz");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &bytes).map_err(|e| OptJobError::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| OptJobError::Io(format!("{}: {e}", path.display())))?;
    let size = std::fs::metadata(path).map_err(|e| OptJobError::Io(e.to_string()))?.len();
    Ok((t0.elapsed().as_secs_f64() * 1e3, size))
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdamState {
    pub count: i32,
    pub mu: NamedArrays,
    pub nu: NamedArrays,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Adam {
    pub b1: f64,
    pub b2: f64,
    pub eps: f64,
}

impl Default for Adam {
    fn default() -> Self {
        Self { b1: 0.9, b2: 0.999, eps: 1e-8 }
    }
}

impl Adam {
    #[must_use]
    pub fn init(&self, params: &NamedArrays) -> AdamState {
        let zeros: NamedArrays =
            params.iter().map(|(k, v)| (k.to_string(), ArrayD::zeros(v.raw_dim()))).collect();
        AdamState { count: 0, mu: zeros.clone(), nu: zeros }
    }

    #[must_use]
    pub fn direction(&self, grads: &NamedArrays, state: &AdamState) -> (NamedArrays, AdamState) {
        let count = state.count.saturating_add(1);
        let mut mu = NamedArrays::new();
        let mut nu = NamedArrays::new();
        for (k, g) in grads.iter() {
            let m0 = state.mu.get(k).cloned().unwrap_or_else(|| ArrayD::zeros(g.raw_dim()));
            let v0 = state.nu.get(k).cloned().unwrap_or_else(|| ArrayD::zeros(g.raw_dim()));
            let mut m = m0;
            ndarray::Zip::from(&mut m).and(g).for_each(|m, g| *m = self.b1 * *m + (1.0 - self.b1) * g);
            let mut v = v0;
            ndarray::Zip::from(&mut v).and(g).for_each(|v, g| *v = self.b2 * *v + (1.0 - self.b2) * g * g);
            mu.insert(k, m);
            nu.insert(k, v);
        }
        let c1 = 1.0 - self.b1.powf(f64::from(count));
        let c2 = 1.0 - self.b2.powf(f64::from(count));
        let mut out = NamedArrays::new();
        for (k, _) in grads.iter() {
            let (Some(m), Some(v)) = (mu.get(k), nu.get(k)) else { continue };
            let mut d = m.clone();
            ndarray::Zip::from(&mut d).and(v).for_each(|d, v| *d = (*d / c1) / ((v / c2).sqrt() + self.eps));
            out.insert(k, d);
        }
        (out, AdamState { count, mu, nu })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    pub params: NamedArrays,
    pub adam: Option<AdamState>,
    pub warm: NamedArrays,
    pub stage_idx: i64,
    pub it_next: i64,
    pub l0: f64,
    pub n_rows: i64,
    pub refs: Value,
    pub extras: Value,
}

#[derive(Debug, Clone, Copy)]
pub struct CheckpointInput<'a> {
    pub params: &'a NamedArrays,
    pub adam: Option<&'a AdamState>,
    pub warm: &'a NamedArrays,
    pub it_next: i64,
    pub l0: f64,
    pub refs: &'a Value,
    pub n_rows: i64,
    pub extras: &'a Value,
    pub stage_idx: i64,
}

fn py_dumps(value: &Value) -> String {
    let v = if value.is_null() { Value::Object(serde_json::Map::new()) } else { value.clone() };
    implexity_core::json::dumps(&v, &implexity_core::json::DumpOptions::default())
}


pub fn save_checkpoint(job_dir: &Path, input: &CheckpointInput<'_>) -> Result<(f64, u64), OptJobError> {
    let mut payload: Vec<(String, NpyArray)> = Vec::new();
    for (k, v) in input.params.iter() {
        payload.push((format!("p_{k}"), NpyArray::from_f64(v)));
    }
    payload.push(("schema".into(), NpyArray::scalar_str(CHECKPOINT_SCHEMA)));
    let mut keys = input.params.names();
    keys.sort();
    payload.push(("param_keys".into(), NpyArray::strings(&keys)));
    if let Some(adam) = input.adam {
        let count = NpyArray::new(Vec::new(), NpyData::I32(vec![adam.count]))
            .map_err(|e| OptJobError::Io(e.to_string()))?;
        payload.push(("adam_count".into(), count));
        for (k, _) in input.params.iter() {
            let mu = adam.mu.get(k).ok_or_else(|| OptJobError::Io(format!("adam state omits mu of {k}")))?;
            let nu = adam.nu.get(k).ok_or_else(|| OptJobError::Io(format!("adam state omits nu of {k}")))?;
            payload.push((format!("adam_mu_{k}"), NpyArray::from_f64(mu)));
            payload.push((format!("adam_nu_{k}"), NpyArray::from_f64(nu)));
        }
    }
    for (k, v) in input.warm.iter() {
        payload.push((format!("warm_{k}"), NpyArray::from_f64(v)));
    }
    payload.push(("stage_idx".into(), NpyArray::scalar_i64(input.stage_idx)));
    payload.push(("it_next".into(), NpyArray::scalar_i64(input.it_next)));
    payload.push(("l0".into(), NpyArray::scalar_f64(input.l0)));
    payload.push(("n_rows".into(), NpyArray::scalar_i64(input.n_rows)));
    payload.push(("refs_json".into(), NpyArray::scalar_str(&py_dumps(input.refs))));
    payload.push(("extras_json".into(), NpyArray::scalar_str(&py_dumps(input.extras))));
    atomic_savez(&job_dir.join(CHECKPOINT_FILE), false, &payload)
}


pub fn load_checkpoint(job_dir: &Path) -> Result<Checkpoint, OptJobError> {
    let p = job_dir.join(CHECKPOINT_FILE);
    let npz = implexity_io::npz::load_file(&p).map_err(|e| OptJobError::Io(e.to_string()))?;
    let not_ckpt =
        || OptJobError::NotACheckpoint(format!("{} is not a {CHECKPOINT_SCHEMA} checkpoint", p.display()));
    if npz.get("schema").and_then(NpyArray::as_scalar_str) != Some(CHECKPOINT_SCHEMA) {
        return Err(not_ckpt());
    }
    let strings = |key: &str| -> Result<Vec<String>, OptJobError> {
        match npz.get(key).map(|a| &a.data) {
            Some(NpyData::Unicode { values, .. }) => Ok(values.clone()),
            _ => Err(OptJobError::Io(format!("checkpoint member {key} is missing or not text"))),
        }
    };
    let array = |key: &str| -> Result<ArrayD<f64>, OptJobError> {
        npz.get(key)
            .and_then(NpyArray::to_f64)
            .ok_or_else(|| OptJobError::Io(format!("checkpoint member {key} is missing or not numeric")))
    };
    let scalar_i = |key: &str| -> Result<i64, OptJobError> {
        npz.get(key)
            .and_then(NpyArray::to_i64)
            .and_then(|a| a.iter().next().copied())
            .ok_or_else(|| OptJobError::Io(format!("checkpoint member {key} is missing or not an integer")))
    };
    let text = |key: &str| -> Result<Value, OptJobError> {
        let s = npz
            .get(key)
            .and_then(NpyArray::as_scalar_str)
            .ok_or_else(|| OptJobError::Io(format!("checkpoint member {key} is missing")))?;
        implexity_core::json::parse_with(
            s,
            implexity_core::json::ParseOptions { reject_duplicate_keys: false },
        )
        .map_err(|e| OptJobError::Io(format!("checkpoint member {key}: {e}")))
    };
    let keys = strings("param_keys")?;
    let mut params = NamedArrays::new();
    for k in &keys {
        params.insert(k.clone(), array(&format!("p_{k}"))?);
    }
    let adam = if npz.contains("adam_count") {
        let count = i32::try_from(scalar_i("adam_count")?).map_err(|e| OptJobError::Io(e.to_string()))?;
        let mut mu = NamedArrays::new();
        let mut nu = NamedArrays::new();
        for k in &keys {
            mu.insert(k.clone(), array(&format!("adam_mu_{k}"))?);
            nu.insert(k.clone(), array(&format!("adam_nu_{k}"))?);
        }
        Some(AdamState { count, mu, nu })
    } else {
        None
    };
    let mut warm = NamedArrays::new();
    for name in npz.files() {
        if let Some(stripped) = name.strip_prefix("warm_") {
            warm.insert(stripped.to_string(), array(name)?);
        }
    }
    let l0 = npz
        .get("l0")
        .and_then(NpyArray::to_f64)
        .and_then(|a| a.iter().next().copied())
        .ok_or_else(|| OptJobError::Io("checkpoint member l0 is missing".into()))?;
    Ok(Checkpoint {
        params,
        adam,
        warm,
        stage_idx: scalar_i("stage_idx")?,
        it_next: scalar_i("it_next")?,
        l0,
        n_rows: scalar_i("n_rows")?,
        refs: text("refs_json")?,
        extras: text("extras_json")?,
    })
}

#[must_use]
pub fn all_finite(tree: &NamedArrays) -> bool {
    tree.iter().all(|(_, a)| a.iter().all(|v| v.is_finite()))
}


pub fn box_section_png(rho: &ArrayD<f64>, axis: usize, scale: usize) -> Result<Vec<u8>, OptJobError> {
    if rho.ndim() != 3 || axis > 2 {
        return Err(OptJobError::Io("box_section_png needs a three-dimensional field".into()));
    }
    let n = rho.shape()[axis];
    let section = rho.index_axis(ndarray::Axis(axis), n / 2);
    let (a, b) = (section.shape()[0], section.shape()[1]);
    let height = b * scale;
    let width = a * scale;
    let mut pixels = Vec::with_capacity(width * height * 3);
    for row in 0..height {
        let j = (height - 1 - row) / scale;
        for col in 0..width {
            let i = col / scale;
            let r = section[[i, j]].clamp(0.0, 1.0);
            for value in [0.15 + 0.85 * r, 0.10 + 0.65 * r, 0.30 + 0.20 * r] {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let byte = (value.clamp(0.0, 1.0) * 255.0) as u8;
                pixels.push(byte);
            }
        }
    }
    let w = u32::try_from(width).map_err(|e| OptJobError::Io(e.to_string()))?;
    let h = u32::try_from(height).map_err(|e| OptJobError::Io(e.to_string()))?;
    implexity_io::png_io::encode_rgb(w, h, &pixels).map_err(|e| OptJobError::Io(e.to_string()))
}

#[must_use]
#[cfg(target_os = "macos")]
#[allow(clippy::cast_precision_loss)]
pub fn peak_rss_mb() -> f64 {
    nix::sys::resource::getrusage(nix::sys::resource::UsageWho::RUSAGE_SELF)
        .map_or(f64::NAN, |usage| usage.max_rss() as f64 / (1024.0 * 1024.0))
}

#[must_use]
#[cfg(not(target_os = "macos"))]
pub fn peak_rss_mb() -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else { return f64::NAN };
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<f64>().ok())
        .map_or(f64::NAN, |kb| kb / 1024.0)
}

