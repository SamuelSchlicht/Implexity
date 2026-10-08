// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::{Read as _, Write as _};
use std::path::Path;

use implexity_io::fsguard::{self, FileStat};
use implexity_io::npy::NpyArray;
use serde_json::{Value, json};

use crate::error::{JobError, JobResult};

fn verr(m: &str) -> JobError {
    JobError::value(m)
}

pub const JSON_LIMIT: u64 = 64 * 1024 * 1024;
pub const NPZ_LIMIT: u64 = 512 * 1024 * 1024;
pub const COPY_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

fn regular_owned_single(meta: &FileStat) -> bool {
    meta.is_owned_single_regular()
}

fn read_checked(
    path: &Path,
    min: u64,
    maximum_bytes: u64,
    unsafe_msg: &str,
    drift_msg: &str,
    trunc_msg: &str,
) -> JobResult<Vec<u8>> {
    let before = fsguard::stat_nofollow(path).map_err(|_| verr(unsafe_msg))?;
    if !regular_owned_single(&before) || before.size < min || before.size > maximum_bytes {
        return Err(verr(unsafe_msg));
    }
    let mut file = crate::private::open_nofollow(path).map_err(|_| verr(unsafe_msg))?;
    let opened = fsguard::stat_file(&file)?;
    if !opened.same_object(&before) || opened.size != before.size || !regular_owned_single(&opened) {
        return Err(verr(drift_msg));
    }
    let mut data = Vec::with_capacity(usize::try_from(opened.size).unwrap_or(0));
    (&mut file).take(opened.size).read_to_end(&mut data)?;
    if u64::try_from(data.len()).ok() != Some(opened.size) {
        return Err(verr(trunc_msg));
    }
    Ok(data)
}


pub fn read_json(path: &Path, maximum_bytes: u64) -> JobResult<Value> {
    let data = read_checked(
        path,
        2,
        maximum_bytes,
        "managed optimization JSON artifact is unsafe",
        "managed optimization JSON artifact drifted",
        "managed optimization JSON artifact truncated",
    )?;
    let text = String::from_utf8(data).map_err(|e| JobError::of("UnicodeDecodeError", e.to_string()))?;
    implexity_core::json::parse_with(
        &text,
        implexity_core::json::ParseOptions { reject_duplicate_keys: false },
    )
    .map_err(|e| JobError::of("JSONDecodeError", e.to_string()))
}

fn fingerprint(data: &[u8]) -> Value {
    json!({"bytes": data.len(), "sha256": crate::private::sha256_hex(data)})
}


pub fn read_npz(
    path: &Path,
    maximum_bytes: u64,
    expected_fingerprint: Option<&Value>,
) -> JobResult<(Vec<(String, NpyArray)>, Value)> {
    if maximum_bytes == 0 || maximum_bytes > 8 * 1024 * 1024 * 1024 {
        return Err(verr("managed NumPy container bound is invalid"));
    }
    if let Some(fp) = expected_fingerprint {
        let ok = fp.as_object().is_some_and(|m| {
            m.len() == 2
                && m.get("bytes").and_then(Value::as_u64).is_some_and(|b| b >= 1)
                && m.get("bytes").is_some_and(|b| b.is_u64() || b.is_i64())
                && m.get("sha256").and_then(Value::as_str).is_some_and(crate::private::is_sha256)
        });
        if !ok {
            return Err(verr("managed NumPy container fingerprint is invalid"));
        }
    }
    let data = read_checked(
        path,
        1,
        maximum_bytes,
        "managed NumPy container is unsafe",
        "managed NumPy container identity drifted",
        "managed NumPy container truncated",
    )?;
    let actual = fingerprint(&data);
    if expected_fingerprint.is_some_and(|fp| fp != &actual) {
        return Err(verr("managed NumPy container fingerprint drifted"));
    }
    let archive = implexity_io::zip::ZipArchive::new(&data)
        .map_err(|_| verr("managed NumPy container is unreadable"))?;
    let entries = archive.entries();

    let total: u64 = entries.iter().fold(0_u64, |acc, e| acc.saturating_add(e.size));
    if entries.is_empty() || entries.len() > 16_384 || total > maximum_bytes {
        return Err(verr("managed NumPy container expansion is unsafe"));
    }
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    let logical: std::collections::BTreeSet<&str> =
        names.iter().map(|n| n.get(..n.len().saturating_sub(4)).unwrap_or("")).collect();
    if unique.len() != names.len() || logical.len() != names.len() {
        return Err(verr("managed NumPy container has duplicate members"));
    }
    for e in entries {
        let n = e.name.as_str();
        if e.flags & 0x1 != 0
            || e.method != 0
            || !n.ends_with(".npy")
            || n.contains('/')
            || n.contains('\\')
            || n.is_empty()
            || n == "."
            || n == ".."
        {
            return Err(verr("managed NumPy container member is unsafe"));
        }
    }
    let npz = implexity_io::npz::load(&data).map_err(|_| verr("managed NumPy container is unreadable"))?;
    Ok((npz.members().to_vec(), actual))
}


pub fn copy_regular(source: &Path, destination: &Path, maximum_bytes: u64, replace: bool) -> JobResult<()> {
    let before =
        fsguard::stat_nofollow(source).map_err(|_| verr("managed optimization source artifact is unsafe"))?;
    if !regular_owned_single(&before) || before.size > maximum_bytes {
        return Err(verr("managed optimization source artifact is unsafe"));
    }
    let dir =
        destination.parent().ok_or_else(|| verr("managed optimization destination directory is unsafe"))?;
    let dir_before = fsguard::stat_nofollow(dir)
        .map_err(|_| verr("managed optimization destination directory is unsafe"))?;
    if dir_before.is_symlink() || !dir_before.is_dir() || !dir_before.owned {
        return Err(verr("managed optimization destination directory is unsafe"));
    }
    let mut reader = crate::private::open_nofollow(source)
        .map_err(|_| verr("managed optimization source artifact is unsafe"))?;
    let opened = fsguard::stat_file(&reader)?;
    if !opened.same_object(&before) || opened.size != before.size || !regular_owned_single(&opened) {
        return Err(verr("managed optimization source artifact drifted"));
    }
    let name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = dir.join(format!("{name}.managed-{}.tmp", crate::private::token_hex(16)?));
    let result = (|| -> JobResult<()> {
        let mut writer = crate::private::create_exclusive(&temporary, 0o600)?;
        let mut remaining = opened.size;
        let mut buf = vec![0_u8; 1024 * 1024];
        while remaining > 0 {
            let want = usize::try_from(remaining.min(1024 * 1024)).unwrap_or(1024 * 1024);
            let n = reader.read(&mut buf[..want])?;
            if n == 0 {
                return Err(verr("managed optimization source artifact truncated"));
            }
            writer.write_all(&buf[..n])?;
            remaining -= n as u64;
        }
        writer.sync_all()?;
        drop(writer);
        if replace {
            std::fs::rename(&temporary, destination)?;
        } else {
            std::fs::hard_link(&temporary, destination)?;
            std::fs::remove_file(&temporary)?;
        }
        let d = fsguard::Dir::open(dir)?;
        let dm = d.stat()?;
        if !dm.same_object(&dir_before) || !dm.is_dir() || !dm.owned {
            return Err(verr("managed optimization destination directory drifted"));
        }
        d.sync_all()?;
        Ok(())
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}


pub fn artifact_fingerprint(path: &Path, maximum_bytes: u64) -> JobResult<Value> {
    crate::manager::ModelOptimizeManager::managed_artifact_fingerprint(path, maximum_bytes)
}

