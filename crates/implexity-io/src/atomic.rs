// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use implexity_core::json::{DumpOptions, dumps};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0}")]
    Unsafe(String),
}

fn io(path: &Path, e: std::io::Error) -> StorageError {
    StorageError::Io(path.display().to_string(), e)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn unique_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let state = std::collections::hash_map::RandomState::new();
    let mut a = state.build_hasher();
    a.write_u64(n);
    a.write_u128(now);
    a.write_u32(std::process::id());
    let mut b = state.build_hasher();
    b.write_u64(a.finish());
    b.write_u64(n.rotate_left(17));
    format!("{:016x}{:016x}", a.finish(), b.finish())
}


pub fn os_random_bytes(n: usize) -> Result<Vec<u8>, StorageError> {
    let path = Path::new("/dev/urandom");
    let mut f = File::open(path).map_err(|e| io(path, e))?;
    let mut out = vec![0u8; n];
    f.read_exact(&mut out).map_err(|e| io(path, e))?;
    Ok(out)
}

#[must_use]
pub fn current_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("Uid:") {

                    if let Some(euid) = rest.split_whitespace().nth(1).and_then(|v| v.parse().ok()) {
                        return Some(euid);
                    }
                }
            }
        }

        let probe = std::env::temp_dir().join(format!(".implexity-uid-probe-{}", unique_token()));
        let created = OpenOptions::new().write(true).create_new(true).open(&probe);
        let uid = created.ok().and_then(|f| f.metadata().ok()).map(|m| m.uid());
        let _ = std::fs::remove_file(&probe);
        uid
    }
    #[cfg(not(unix))]
    {
        None
    }
}


pub fn fsync_directory(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        let dir = File::open(path).map_err(|e| io(path, e))?;
        dir.sync_all().map_err(|e| io(path, e))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn sanitise(label: &str, max: usize) -> String {
    label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .take(max)
        .collect()
}


pub fn stage_exclusive(dir: &Path, label: &str, role: &str, bytes: &[u8]) -> Result<PathBuf, StorageError> {
    let path = dir.join(format!(".{}.{}.{}.tmp", sanitise(label, 96), sanitise(role, 48), unique_token()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&path).map_err(|e| io(&path, e))?;
    let result = f.write_all(bytes).and_then(|()| f.sync_all());
    if let Err(e) = result {
        drop(f);
        let _ = std::fs::remove_file(&path);
        return Err(io(&path, e));
    }
    Ok(path)
}

fn parent_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}


pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let dir = parent_of(path);
    let label = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let staged = stage_exclusive(&dir, &label, "replace", bytes)?;
    if let Err(e) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(io(path, e));
    }
    fsync_directory(&dir)
}


pub fn publish_exclusive(path: &Path, bytes: &[u8]) -> Result<bool, StorageError> {
    let dir = parent_of(path);
    let label = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let staged = stage_exclusive(&dir, &label, "publish", bytes)?;
    let linked = std::fs::hard_link(&staged, path);
    let _ = std::fs::remove_file(&staged);
    match linked {
        Ok(()) => {
            fsync_directory(&dir)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(io(path, e)),
    }
}


pub fn write_json_atomic(
    path: &Path,
    value: &Value,
    options: &DumpOptions,
    newline: bool,
) -> Result<(), StorageError> {
    let mut text = dumps(value, options);
    if newline {
        text.push('\n');
    }
    write_atomic(path, text.as_bytes())
}


pub fn read_owned_regular(path: &Path, absent_ok: bool) -> Result<Option<Vec<u8>>, StorageError> {
    let st = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if absent_ok && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    let refuse = || {
        StorageError::Unsafe(format!(
            "refusing non-regular, non-owned, or multiply-linked persistence path {}",
            path.display()
        ))
    };
    if st.file_type().is_symlink() || !st.file_type().is_file() {
        return Err(refuse());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if current_uid().is_some_and(|u| u != st.uid()) || st.nlink() != 1 {
            return Err(refuse());
        }
    }
    let mut f = File::open(path).map_err(|e| io(path, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let fst = f.metadata().map_err(|e| io(path, e))?;
        if !fst.is_file() || fst.nlink() != 1 || (fst.dev(), fst.ino()) != (st.dev(), st.ino()) {
            return Err(StorageError::Unsafe(format!(
                "persistence path changed while it was being checked: {}",
                path.display()
            )));
        }
    }
    let mut out = Vec::new();
    f.read_to_end(&mut out).map_err(|e| io(path, e))?;
    Ok(Some(out))
}

