// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use implexity_core::contracts::MatchingTimeNewtonGuess;
use implexity_core::{CaeError, CaeResult};
use implexity_io::npy::{NpyArray, NpyData};
use serde_json::{Map, Value, json};

use crate::error::{JobError, JobResult};
use crate::private::{canonical_text, sha256_file, sha256_hex};

pub const EPOCH_STATE_DIR: &str = "epochs_state";
pub const EPOCH_STATE_SCHEMA: &str = "implexity-epoch-state/1";
pub const EPOCH_PROVENANCE_SCHEMA: &str = "implexity-epoch-provenance/1";
pub const EPOCH_STATE_INDEX_SCHEMA: &str = "implexity-epoch-state-index/1";
pub const UNCOMMITTED_DIR: &str = "uncommitted";

const CHECKPOINT_PREFIX: &str = "ckpt__";
const WARM_PREFIX: &str = "warm__";

#[must_use]
pub fn archive_name(epoch: usize) -> String {
    format!("epoch_{epoch:06}.npz")
}

#[must_use]
pub fn manifest_name(epoch: usize) -> String {
    format!("epoch_{epoch:06}.json")
}

fn epoch_of(name: &str, extensions: &[&str]) -> Option<usize> {
    let rest = name.strip_prefix("epoch_")?;
    let (digits, ext) = rest.split_at_checked(6)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !extensions.contains(&ext) {
        return None;
    }
    digits.parse().ok()
}

#[must_use]
pub fn is_epoch_state_name(name: &str) -> bool {
    epoch_of(name, &[".npz", ".json"]).is_some()
}

#[must_use]
pub fn run_provenance(spec: &Map<String, Value>, provider: &str, identities: &Map<String, Value>) -> Value {
    let executable = std::env::current_exe().ok();
    let executable_sha256 = executable.as_deref().and_then(|p| sha256_file(p).ok());
    json!({
        "schema": EPOCH_PROVENANCE_SCHEMA,
        "job_spec_sha256": sha256_hex(canonical_text(&Value::Object(spec.clone())).as_bytes()),
        "provider": provider,
        "executable": executable.map(|p| p.to_string_lossy().into_owned()),
        "executable_sha256": executable_sha256,
        "identities": identities,
    })
}

pub struct EpochRecord<'a> {
    pub epoch: usize,
    pub checkpoint: &'a [(String, NpyArray)],
    pub warm_start: Option<&'a MatchingTimeNewtonGuess>,
    pub history_row: &'a Value,
    pub history_payload: &'a Value,
    pub provenance: &'a Value,
    pub seeded_from: Option<&'a Value>,
}

fn io(context: &str, e: &std::io::Error) -> JobError {
    JobError::contract(format!("epoch state {context}: {e}"))
}

fn write_durably(path: &Path, bytes: &[u8]) -> JobResult<()> {
    if path.exists() || path.is_symlink() {
        return Err(JobError::contract(format!(
            "epoch state {} already exists; epoch states are immutable",
            path.display()
        )));
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!("{name}.tmp"));
    {
        let mut file = std::fs::File::create(&tmp).map_err(|e| io("write", &e))?;
        file.write_all(bytes).map_err(|e| io("write", &e))?;
        file.sync_all().map_err(|e| io("fsync", &e))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| io("rename", &e))?;
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {

        let _ = dir.sync_all();
    }
    Ok(())
}

fn text_member(members: &[(String, NpyArray)], key: &str) -> Option<String> {
    members.iter().find(|(k, _)| k == key).and_then(|(_, a)| match &a.data {
        NpyData::Unicode { values, .. } if values.len() == 1 => Some(values[0].clone()),
        _ => None,
    })
}


pub fn write_epoch_state(out_dir: &Path, record: &EpochRecord<'_>) -> JobResult<Value> {
    let dir = out_dir.join(EPOCH_STATE_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| io("directory", &e))?;
    let archive = dir.join(archive_name(record.epoch));
    let manifest_path = dir.join(manifest_name(record.epoch));
    if manifest_path.exists() || manifest_path.is_symlink() {
        return Err(JobError::contract(format!(
            "epoch state {} already exists; epoch states are immutable",
            manifest_path.display()
        )));
    }
    let mut members: Vec<(String, NpyArray)> = vec![
        ("epoch_state_schema".into(), NpyArray::scalar_str(EPOCH_STATE_SCHEMA)),
        ("epoch".into(), NpyArray::scalar_i64(i64::try_from(record.epoch).unwrap_or(i64::MAX))),
        ("history_row".into(), NpyArray::scalar_str(&canonical_text(record.history_row))),
        ("history_payload".into(), NpyArray::scalar_str(&canonical_text(record.history_payload))),
    ];
    members.extend(record.checkpoint.iter().map(|(k, v)| (format!("{CHECKPOINT_PREFIX}{k}"), v.clone())));
    let warm_states = match record.warm_start {
        Some(guess) => {
            let warm = crate::hierarchical_job::warm_start_members(guess)?;
            members.extend(warm.into_iter().map(|(k, v)| (format!("{WARM_PREFIX}{k}"), v)));
            Some(guess.states().len())
        }
        None => None,
    };
    let refs: Vec<(&str, &NpyArray)> = members.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let bytes = implexity_io::npz::save_compressed(&refs)
        .map_err(|e| JobError::contract(format!("epoch state encoding: {e}")))?;
    write_durably(&archive, &bytes)?;
    let warm_doubles: usize = record.warm_start.map_or(0, |g| g.states().iter().map(|s| s.len()).sum());
    let manifest = json!({
        "schema": EPOCH_STATE_SCHEMA,
        "epoch": record.epoch,
        "archive": {"file": archive_name(record.epoch), "bytes": bytes.len(), "sha256": sha256_hex(&bytes),
                    "compression": "deflate"},
        "design_state_id": record.history_row.get("design_state_id").cloned().unwrap_or(Value::Null),
        "accepted": record.history_row.get("accepted").cloned().unwrap_or(Value::Null),
        "objective": record.history_row.get("L").cloned().unwrap_or(Value::Null),
        "live_snapshot": record.history_row.get("push").cloned().unwrap_or(Value::Null),
        "history_digest": text_member(record.checkpoint, "history_digest"),
        "checkpoint_schema": text_member(record.checkpoint, "checkpoint_schema"),
        "checkpoint_members": record.checkpoint.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
        "warm_start": warm_states.map_or(Value::Null, |n| json!({"states": n, "values": warm_doubles})),
        "provenance": record.provenance,
        "seeded_from": record.seeded_from.cloned().unwrap_or(Value::Null),
    });
    let mut text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| JobError::contract(format!("epoch state manifest: {e}")))?;
    text.push('\n');
    write_durably(&manifest_path, text.as_bytes())?;
    let committed = json!({"schema":"implexity-committed-epoch-generation/1", "epoch":record.epoch, "archive_sha256":manifest["archive"]["sha256"], "manifest_sha256":sha256_hex(text.as_bytes())});
    implexity_io::atomic::write_atomic(&dir.join("committed.json"), canonical_text(&committed).as_bytes()).map_err(|e| JobError::runtime(e.to_string()))?;
    Ok(manifest)
}

pub struct EpochState {
    pub manifest: Value,
    pub checkpoint: BTreeMap<String, NpyArray>,
    pub warm_start: Option<MatchingTimeNewtonGuess>,
    pub history_row: Value,
    pub history_payload: Option<Value>,
}

impl EpochState {
    #[must_use]
    pub fn checkpoint_text(&self, key: &str) -> Option<String> {
        self.checkpoint.get(key).and_then(|a| match &a.data {
            NpyData::Unicode { values, .. } if values.len() == 1 => Some(values[0].clone()),
            _ => None,
        })
    }
}


pub fn read_epoch_state(job_dir: &Path, epoch: usize) -> CaeResult<Option<EpochState>> {
    let dir = job_dir.join(EPOCH_STATE_DIR);
    let manifest_path = dir.join(manifest_name(epoch));
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest: Value = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|m: &Value| {
            m.get("schema").and_then(Value::as_str) == Some(EPOCH_STATE_SCHEMA)
                && m.get("epoch").and_then(Value::as_u64) == u64::try_from(epoch).ok()
        })
        .ok_or_else(|| CaeError::contract(format!("epoch state manifest {epoch} is malformed")))?;
    let archive = dir.join(archive_name(epoch));
    let bytes = std::fs::read(&archive)
        .map_err(|_| CaeError::contract(format!("epoch state archive {epoch} is missing")))?;
    if manifest["archive"]["sha256"].as_str() != Some(sha256_hex(&bytes).as_str()) {
        return Err(CaeError::contract(format!("epoch state archive {epoch} does not match its manifest")));
    }
    let npz = implexity_io::npz::load(&bytes)
        .map_err(|e| CaeError::contract(format!("epoch state archive {epoch} is unreadable: {e}")))?;
    let text = |key: &str| match npz.get(key).map(|a| &a.data) {
        Some(NpyData::Unicode { values, .. }) if values.len() == 1 => Some(values[0].clone()),
        _ => None,
    };
    if text("epoch_state_schema").as_deref() != Some(EPOCH_STATE_SCHEMA) {
        return Err(CaeError::contract(format!("epoch state archive {epoch} schema is unsupported")));
    }
    let history_row: Value = text("history_row")
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| CaeError::contract(format!("epoch state archive {epoch} lacks its history row")))?;
    let mut checkpoint = BTreeMap::new();
    let mut warm_count = 0_u64;
    for (name, array) in npz.members() {
        if let Some(key) = name.strip_prefix(CHECKPOINT_PREFIX) {
            checkpoint.insert(key.to_string(), array.clone());
        } else if name.strip_prefix(WARM_PREFIX).is_some_and(|k| k.starts_with("state_")) {
            warm_count += 1;
        }
    }
    let declared = manifest.get("warm_start").and_then(|w| w.get("states")).and_then(Value::as_u64);
    let warm_start = match declared {
        None if warm_count == 0 => None,
        Some(n) if n == warm_count => Some(crate::hierarchical_job::warm_start_from_members(
            &|key| npz.get(&format!("{WARM_PREFIX}{key}")),
            n,
        )?),
        _ => {
            return Err(CaeError::contract(format!(
                "epoch state archive {epoch} warm start is inconsistent"
            )));
        }
    };
    let history_payload = text("history_payload").map(|t| serde_json::from_str(&t)).transpose().map_err(|_| CaeError::contract("epoch history payload is malformed"))?;
    Ok(Some(EpochState { manifest, checkpoint, warm_start, history_row, history_payload }))
}


pub fn restore_committed_generation(job_dir: &Path, identities: &Map<String, Value>, solve_id: &str) -> JobResult<Option<Value>> {
    let dir = job_dir.join(EPOCH_STATE_DIR);
    let pointer = dir.join("committed.json");
    if !pointer.is_file() { return Ok(None); }
    let committed: Value = serde_json::from_slice(&std::fs::read(&pointer)?).map_err(|_| JobError::contract("committed epoch generation is malformed"))?;
    if committed["schema"] != "implexity-committed-epoch-generation/1" { return Err(JobError::contract("committed epoch generation schema is unsupported")); }
    let epoch = committed["epoch"].as_u64().and_then(|e| usize::try_from(e).ok()).ok_or_else(|| JobError::contract("committed epoch generation has no epoch"))?;
    let manifest_bytes = std::fs::read(dir.join(manifest_name(epoch)))?;
    if committed["manifest_sha256"].as_str() != Some(sha256_hex(&manifest_bytes).as_str()) { return Err(JobError::contract("committed epoch manifest identity drifted")); }
    let state = read_epoch_state(job_dir, epoch)?.ok_or_else(|| JobError::contract("committed epoch state is missing"))?;
    if committed["archive_sha256"] != state.manifest["archive"]["sha256"] { return Err(JobError::contract("committed epoch archive identity drifted")); }
    for (key, expected) in identities {
        if state.checkpoint_text(key).as_deref() != expected.as_str() { return Err(JobError::contract(format!("committed epoch {key} identity drifted"))); }
    }
    if state.checkpoint_text("solve_id").as_deref().or_else(|| state.manifest["provenance"]["identities"]["solve_id"].as_str()) != Some(solve_id) { return Err(JobError::contract("committed epoch solve identity drifted")); }
    let payload = state.history_payload.as_ref().ok_or_else(|| JobError::contract("committed epoch lacks complete history"))?;
    let rows = payload["history"].as_array().ok_or_else(|| JobError::contract("committed epoch history is malformed"))?;
    if rows.len() != epoch + 1 || rows.last() != Some(&state.history_row) || state.checkpoint_text("history_digest").as_deref() != Some(sha256_hex(canonical_text(payload).as_bytes()).as_str()) { return Err(JobError::contract("committed epoch history identity drifted")); }
    let members: Vec<(&str, &NpyArray)> = state.checkpoint.iter().map(|(k,v)| (k.as_str(),v)).collect();
    let checkpoint = implexity_io::npz::save(&members).map_err(|e| JobError::contract(e.to_string()))?;
    let history = canonical_text(payload);
    let checkpoint_matches = implexity_io::npz::load_file(&job_dir.join("ckpt.npz")).is_ok_and(|archive| archive.members().len() == state.checkpoint.len() && archive.members().iter().all(|(name, array)| state.checkpoint.get(name) == Some(array)));
    let mut best_members = BTreeMap::new();
    if let Some(best_id) = state.checkpoint_text("best_design_state_id") {
        let best_index = rows.iter().position(|row| row["design_state_id"].as_str() == Some(best_id.as_str())).ok_or_else(|| JobError::contract("committed best design is absent from history"))?;
        let best_state = read_epoch_state(job_dir, best_index)?.ok_or_else(|| JobError::contract("committed best epoch is missing"))?;
        if best_state.checkpoint_text("design_state_id").as_deref() != Some(best_id.as_str()) { return Err(JobError::contract("committed best epoch design identity drifted")); }
        if best_state.checkpoint_text("solve_id").as_deref().or_else(|| best_state.manifest["provenance"]["identities"]["solve_id"].as_str()) != Some(solve_id) { return Err(JobError::contract("committed best epoch solve identity drifted")); }
        for (key, expected) in identities {
            if best_state.checkpoint_text(key).as_deref() != expected.as_str() { return Err(JobError::contract(format!("committed best epoch {key} identity drifted"))); }
        }
        let refs = if let Some(raw) = best_state.checkpoint_text("coordinate_design_state_ids") {
            let ids: Value = serde_json::from_str(&raw).map_err(|_| JobError::contract("committed best coordinate identities are malformed"))?;
            let ids = ids.as_object().ok_or_else(|| JobError::contract("committed best coordinate identities are malformed"))?;
            let ordered = match best_state.checkpoint.get("coordinate_order").map(|v| &v.data) {
                Some(NpyData::Unicode { values, .. }) => values.clone(),
                _ => return Err(JobError::contract("committed best coordinate order is missing")),
            };
            if ordered.len() != ids.len() || ordered.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len() || ordered.iter().any(|name| !ids.contains_key(name)) { return Err(JobError::contract("committed best coordinate order identity drifted")); }
            ordered
        } else { vec!["model:control".to_string()] };
        let slots = if best_state.checkpoint.contains_key("topology") { vec!["topology".to_string()] } else { refs.iter().map(|name| crate::hierarchical_job::slot(name)).collect() };
        for slot in &slots {
            let key = format!("p_{slot}");
            let source = if slot == "topology" && best_state.checkpoint.contains_key("topology") { "topology" } else { key.as_str() };
            let member = best_state.checkpoint.get(source).ok_or_else(|| JobError::contract("committed best coordinate is missing"))?.clone();
            best_members.insert(key, member);
        }
        best_members.insert("refs".into(), NpyArray::strings(&refs));
        best_members.insert("slots".into(), NpyArray::strings(&slots));
        best_members.insert("units".into(), NpyArray::strings(&vec!["-".to_string(); refs.len()]));
        best_members.insert("solve_id".into(), NpyArray::scalar_str(solve_id));
    }
    let best_matches = best_members.is_empty() || implexity_io::npz::load_file(&job_dir.join("best.npz")).is_ok_and(|archive| archive.members().len() == best_members.len() && archive.members().iter().all(|(name, array)| best_members.get(name) == Some(array)));
    let differs = !checkpoint_matches || !best_matches || std::fs::read(job_dir.join("history.json")).ok().as_deref() != Some(history.as_bytes());
    if !differs { return Ok(None); }
    if !best_matches {
        let refs: Vec<(&str, &NpyArray)> = best_members.iter().map(|(key, value)| (key.as_str(), value)).collect();
        let bytes = implexity_io::npz::save(&refs).map_err(|e| JobError::contract(e.to_string()))?;
        implexity_io::atomic::write_atomic(&job_dir.join("best.npz"), &bytes).map_err(|e| JobError::runtime(e.to_string()))?;
    }
    crate::hierarchical_job::write_resume_warm_start(job_dir, state.warm_start.as_ref(), state.checkpoint_text("design_state_id").as_deref().ok_or_else(|| JobError::contract("committed epoch design identity is missing"))?)?;
    implexity_io::atomic::write_atomic(&job_dir.join("history.json"), history.as_bytes()).map_err(|e| JobError::runtime(e.to_string()))?;
    implexity_io::atomic::write_atomic(&job_dir.join("ckpt.npz"), &checkpoint).map_err(|e| JobError::runtime(e.to_string()))?;
    Ok(Some(json!({"schema":"implexity-epoch-generation-recovery/1","epoch":epoch,"recovered":true,"solve_started":false,"optimizer_state_changed":false})))
}

pub fn set_aside_uncommitted(job_dir: &Path, committed: usize) -> JobResult<Vec<String>> {
    let dir = job_dir.join(EPOCH_STATE_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut moved = Vec::new();
    let stamp =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = match epoch_of(&name, &[".npz", ".json"]) {
            Some(epoch) => epoch >= committed,
            None => epoch_of(&name, &[".npz.tmp", ".json.tmp"]).is_some(),
        };
        if stale {
            let target_dir = dir.join(UNCOMMITTED_DIR);
            std::fs::create_dir_all(&target_dir).map_err(|e| io("directory", &e))?;
            std::fs::rename(entry.path(), target_dir.join(format!("{name}.{stamp}")))
                .map_err(|e| io("set aside", &e))?;
            moved.push(name);
        }
    }
    moved.sort();
    Ok(moved)
}

#[must_use]
pub fn epoch_state_files(job_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(job_dir.join(EPOCH_STATE_DIR))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| is_epoch_state_name(n))
                .map(|n| format!("{EPOCH_STATE_DIR}/{n}"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}


pub fn copy_epoch_states(from: &Path, to: &Path, upto: Option<usize>) -> JobResult<usize> {
    let mut copied = 0;
    for relative in epoch_state_files(from) {
        let name = relative.trim_start_matches(&format!("{EPOCH_STATE_DIR}/")).to_string();
        if upto.is_some_and(|u| epoch_of(&name, &[".npz", ".json"]).is_some_and(|e| e > u)) {
            continue;
        }
        let source = from.join(&relative);
        let target = to.join(&relative);
        if target.exists() {
            let same = sha256_file(&source).ok().zip(sha256_file(&target).ok()).is_some_and(|(a, b)| a == b);
            if !same {
                return Err(JobError::contract(format!(
                    "epoch state {relative} differs between {} and {}",
                    from.display(),
                    to.display()
                )));
            }
            continue;
        }
        let bytes = std::fs::read(&source).map_err(|e| io("read", &e))?;
        std::fs::create_dir_all(to.join(EPOCH_STATE_DIR)).map_err(|e| io("directory", &e))?;
        write_durably(&target, &bytes)?;
        copied += 1;
    }
    Ok(copied)
}


#[allow(clippy::too_many_lines)]
pub fn epoch_state_index(generation: &Path) -> CaeResult<Value> {
    let payload: Value = std::fs::read_to_string(generation.join("history.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| CaeError::contract("generation history is missing or unreadable"))?;
    let rows = payload.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
    let warm_sidecar: Value = std::fs::read_to_string(generation.join("resume_warm_start.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let warm_generations: Vec<Value> = match warm_sidecar.get("generations") {
        Some(Value::Array(g)) => g.clone(),
        _ if warm_sidecar.get("committed_design_state_id").is_some() => vec![warm_sidecar.clone()],
        _ => Vec::new(),
    };
    let mut epochs = Vec::with_capacity(rows.len());
    let (mut full, mut with_design, mut with_warm) = (0_usize, 0_usize, 0_usize);
    let mut errors = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let design_id = row.get("design_state_id").and_then(Value::as_str).unwrap_or("");
        let live = row.get("push").and_then(|p| p.get("file")).and_then(Value::as_str);
        let live_ok = live.is_some_and(|f| {
            let path = generation.join(f);
            match row["push"].get("sha256").and_then(Value::as_str) {
                Some(expected) => sha256_file(&path).is_ok_and(|d| d == expected),
                None => path.is_file(),
            }
        });
        with_design += usize::from(live_ok);
        let state = match read_epoch_state(generation, index) {
            Ok(state) => state,
            Err(e) => {
                errors.push(json!({"epoch": index, "error": e.message()}));
                None
            }
        };
        let (optimizer, warm) = if let Some(s) = &state {
            full += 1;
            let warm = s.warm_start.as_ref().map_or_else(
                || json!({"source": "epoch_state", "states": 0, "note": "committed evaluation started cold"}),
                |g| json!({"source": "epoch_state", "states": g.states().len()}),
            );
            ("epoch_state_checkpoint", warm)
        } else {
            let generation_warm = warm_generations.iter().find(|g| {
                !design_id.is_empty()
                    && g.get("committed_design_state_id").and_then(Value::as_str) == Some(design_id)
            });
            let optimizer = if row.get("search_state").is_some_and(Value::is_object) {
                "history_row_search_state"
            } else {
                "unavailable"
            };
            let warm = generation_warm.map_or(Value::Null, |g| {
                json!({"source": "resume_warm_start", "archive": g.get("archive"), "states": g.get("states")})
            });
            (optimizer, warm)
        };
        with_warm += usize::from(!warm.is_null());
        let restart = if state.is_some() {
            "bitwise"
        } else if !warm.is_null() {
            "warm (latest generations only)"
        } else if live_ok {
            "cold re-evaluation (continuation tolerance)"
        } else {
            "unavailable"
        };
        epochs.push(json!({
            "epoch": index,
            "design_state_id": design_id,
            "accepted": row.get("accepted"),
            "objective": row.get("L"),
            "design_snapshot": live.filter(|_| live_ok),
            "optimizer_state": optimizer,
            "warm_start": warm,
            "epoch_state": state.as_ref().map(|s| s.manifest["archive"].clone()),
            "restart": restart,
        }));
    }
    Ok(json!({
        "schema": EPOCH_STATE_INDEX_SCHEMA,
        "generation": generation.to_string_lossy(),
        "epochs": rows.len(),
        "epochs_with_design_snapshot": with_design,
        "epochs_with_full_state": full,
        "epochs_with_warm_start": with_warm,
        "limits": if full == rows.len() { Value::Null } else { json!(
            "Epochs without a per-epoch state (written before W-CKPT): the design snapshot is \
             live_NNNNNN.npz, the optimizer state is reconstructed from the history row's \
             search_state (stage cursor from the schedule), and the solver warm start exists only \
             for the last two committed generations (resume_warm_start*.npz). A continuation from \
             such an epoch re-evaluates its design cold and is compared within the continuation \
             tolerance (1e-6 relative); it is not a bitwise reproduction of the original run.") },
        "errors": errors,
        "per_epoch": epochs,
    }))
}

#[must_use]
pub fn archive_path(job_dir: &Path, epoch: usize) -> PathBuf {
    job_dir.join(EPOCH_STATE_DIR).join(archive_name(epoch))
}

