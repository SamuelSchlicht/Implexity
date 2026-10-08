// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END





use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde_json::{Value, json};

use super::manifest::{FrameManifest, MIN_BYTE_LIMIT};
use super::store::{FrameWriter, STATUS_FILE};
use super::{DynamicError, DynamicResult};

pub const DIR_ENV: &str = "IMPLEXITY_DYNAMIC_CAPTURE_DIR";
pub const BUDGET_ENV: &str = "IMPLEXITY_DYNAMIC_CAPTURE_BUDGET_BYTES";
pub const DEFAULT_BUDGET_BYTES: u64 = 1 << 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureRoot {
    pub dir: PathBuf,
    pub budget_bytes: u64,
}

impl CaptureRoot {


    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> DynamicResult<Option<Self>> {
        let Some(dir) = lookup(DIR_ENV).filter(|d| !d.is_empty()) else { return Ok(None) };
        let dir = PathBuf::from(dir);
        if !dir.is_absolute() {
            return Err(DynamicError::invalid(format!("{DIR_ENV} must be an absolute path")));
        }
        let budget_bytes = match lookup(BUDGET_ENV) {
            None => DEFAULT_BUDGET_BYTES,
            Some(s) => s.trim().parse::<u64>().ok().filter(|b| *b >= MIN_BYTE_LIMIT).ok_or_else(|| {
                DynamicError::invalid(format!("{BUDGET_ENV} must be an integer of at least {MIN_BYTE_LIMIT}"))
            })?,
        };
        Ok(Some(Self { dir, budget_bytes }))
    }

    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        vec![
            (DIR_ENV.to_owned(), self.dir.display().to_string()),
            (BUDGET_ENV.to_owned(), self.budget_bytes.to_string()),
        ]
    }
}

static ROOT: RwLock<Option<CaptureRoot>> = RwLock::new(None);

thread_local! {
    static IDENTITY: RefCell<Option<Value>> = const { RefCell::new(None) };
}

pub fn install(root: Option<CaptureRoot>) {
    if let Ok(mut g) = ROOT.write() {
        *g = root;
    }
}



pub fn current_root() -> DynamicResult<Option<CaptureRoot>> {
    if let Some(root) = ROOT.read().ok().and_then(|g| g.clone()) {
        return Ok(Some(root));
    }
    CaptureRoot::from_lookup(|k| std::env::var(k).ok())
}

pub fn with_identity<R>(identity: Value, f: impl FnOnce() -> R) -> R {
    let previous = IDENTITY.with(|c| c.replace(Some(identity)));
    let out = f();
    IDENTITY.with(|c| *c.borrow_mut() = previous);
    out
}

#[must_use]
pub fn identity() -> Option<Value> {
    IDENTITY.with(|c| c.borrow().clone())
}

fn sanitize(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(48)
        .collect();
    if s.is_empty() { "run".to_owned() } else { s }
}

#[derive(Clone, Debug)]
struct Entry {
    seq: u64,
    dir: PathBuf,
    bytes: u64,
    writing: bool,
}

fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir).map_or(0, |it| {
        it.filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok())
            .filter(std::fs::Metadata::is_file)
            .map(|m| m.len())
            .sum()
    })
}

fn entries(root: &Path) -> DynamicResult<Vec<Entry>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(DynamicError::io("listing", root, &e)),
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(seq) =
            name.split_once('_').and_then(|(n, _)| (n.len() == 6).then(|| n.parse::<u64>().ok()).flatten())
        else {
            continue;
        };
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else { continue };
        if !meta.is_dir() {
            continue;
        }
        let status = std::fs::read(e.path().join(STATUS_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let writing = status.as_ref().and_then(|s| s.get("state")).and_then(Value::as_str) == Some("writing");
        out.push(Entry { seq, bytes: dir_bytes(&e.path()), dir: e.path(), writing });
    }
    out.sort_by_key(|e| e.seq);
    Ok(out)
}



pub fn make_room(root: &Path, budget: u64, incoming: u64) -> DynamicResult<Vec<PathBuf>> {
    let mut list = entries(root)?;
    let mut evicted = Vec::new();
    loop {
        let total: u64 = list.iter().map(|e| e.bytes).sum();
        if total + incoming <= budget || list.is_empty() {
            return Ok(evicted);
        }
        let n = list.len();
        let interior = (1..n.saturating_sub(1))
            .filter(|&i| !list[i].writing)
            .min_by_key(|&i| (list[i + 1].seq - list[i - 1].seq, list[i].seq));
        let victim = match interior {
            Some(i) => i,
            None if n >= 2 && !list[0].writing => 0,
            None if n == 1 && !list[0].writing => 0,
            None => return Ok(evicted),
        };
        let e = list.remove(victim);
        std::fs::remove_dir_all(&e.dir).map_err(|err| DynamicError::io("store eviction", &e.dir, &err))?;
        evicted.push(e.dir);
    }
}



pub fn mark_interrupted(root: &Path) -> DynamicResult<usize> {
    let mut n = 0;
    for e in entries(root)?.into_iter().filter(|e| e.writing) {
        let path = e.dir.join(STATUS_FILE);
        let Some(mut status) =
            std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        status["state"] = json!("aborted");
        status["interrupted"] = json!(true);
        let text =
            serde_json::to_vec_pretty(&status).map_err(|err| DynamicError::invalid(err.to_string()))?;
        implexity_io::atomic::write_atomic(&path, &text)
            .map_err(|err| DynamicError::io("status write", &path, &err))?;
        n += 1;
    }
    Ok(n)
}



pub fn begin(mut manifest: FrameManifest, label: &str) -> DynamicResult<Option<FrameWriter>> {
    let Some(root) = current_root()? else { return Ok(None) };
    begin_in(&root, &mut manifest, label).map(Some)
}



pub fn begin_for_design(
    mut manifest: FrameManifest,
    label: &str,
    design: &implexity_optim::NamedArrays,
) -> DynamicResult<Option<FrameWriter>> {
    let Some(root) = current_root()? else { return Ok(None) };
    let id = implexity_optim::design_identity(design).map_err(|e| DynamicError::invalid(e.to_string()))?;
    manifest.provenance.insert("design_state_id".to_owned(), json!(id));
    begin_in(&root, &mut manifest, label).map(Some)
}



pub fn begin_in(root: &CaptureRoot, manifest: &mut FrameManifest, label: &str) -> DynamicResult<FrameWriter> {
    implexity_io::fsguard::create_dir_all_owner_only(&root.dir)
        .map_err(|e| DynamicError::io("creation", &root.dir, &e))?;
    manifest.retention.byte_limit = manifest.retention.byte_limit.min(root.budget_bytes).max(MIN_BYTE_LIMIT);
    if let Some(Value::Object(id)) = identity() {
        for (k, v) in id {
            manifest.provenance.entry(k).or_insert(v);
        }
    }
    manifest.provenance.entry("label".to_owned()).or_insert_with(|| json!(label));
    manifest.provenance.insert("created_utc".to_owned(), json!(implexity_io::digest::utc_timestamp()));
    manifest.validate()?;
    make_room(&root.dir, root.budget_bytes, manifest.retention.byte_limit)?;
    let next = entries(&root.dir)?.last().map_or(0, |e| e.seq + 1);
    let dir = root.dir.join(format!("{next:06}_{}", sanitize(label)));
    FrameWriter::create(&dir, manifest.clone())
}

